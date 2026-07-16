# ===== File: norm.mojo — RMSNorm kernels (row-per-block, f32 accumulation) =====
# One thread block per row: LLM hidden sizes (1k-16k) keep a 256-thread block
# busy via grid-stride columns, and the two-level warp/shared reduction avoids
# atomics. Accumulation is always f32 regardless of storage dtype so the
# result matches the CPU golden reference within f16 rounding only.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.primitives import warp
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import rsqrt

comptime WARP_SIZE = 32
comptime MAX_WARPS = 32


def _block_reduce_sum(val: Float32) -> Float32:
    # Two-level reduction: intra-warp shuffle sum, then first warp reduces the
    # per-warp partials staged in shared memory. Returns the total to every
    # thread via a shared broadcast slot.
    shared = stack_allocation[MAX_WARPS, Float32, address_space = AddressSpace.SHARED]()
    var v = warp.sum(val)
    lane = thread_idx.x % WARP_SIZE
    wid = thread_idx.x // WARP_SIZE
    if lane == 0:
        shared[wid] = v
    barrier()
    n_warps = (block_dim.x + WARP_SIZE - 1) // WARP_SIZE
    if wid == 0:
        var partial: Float32 = 0.0
        if lane < n_warps:
            partial = shared[lane]
        partial = warp.sum(partial)
        if lane == 0:
            shared[0] = partial
    barrier()
    return shared[0]


def rmsnorm_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    weight: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    eps: Float32,
):
    """out[row] = x[row] / rms(x[row]) * weight, one block per row."""
    row = Int(block_idx.x)
    base = row * n_cols

    var ss: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i < n_cols:
        v = Float32(x[base + i])
        ss += v * v
        i += Int(block_dim.x)

    total = _block_reduce_sum(ss)
    inv = rsqrt(total / Float32(n_cols) + eps)

    i = Int(thread_idx.x)
    while i < n_cols:
        out_ptr[base + i] = Float16(Float32(x[base + i]) * inv * Float32(weight[i]))
        i += Int(block_dim.x)


def rmsnorm_residual_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    residual_io: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    weight: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    eps: Float32,
):
    """Fused residual-add + RMSNorm: residual += x, out = rmsnorm(residual).

    The residual stream update and the norm read the same values, so fusing
    them halves DRAM traffic on the decode path.
    """
    row = Int(block_idx.x)
    base = row * n_cols

    var ss: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i < n_cols:
        v = Float32(residual_io[base + i]) + Float32(x[base + i])
        residual_io[base + i] = Float16(v)
        ss += v * v
        i += Int(block_dim.x)

    total = _block_reduce_sum(ss)
    inv = rsqrt(total / Float32(n_cols) + eps)

    i = Int(thread_idx.x)
    while i < n_cols:
        out_ptr[base + i] = Float16(Float32(residual_io[base + i]) * inv * Float32(weight[i]))
        i += Int(block_dim.x)
