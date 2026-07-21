# ===== File: norm.mojo — RMSNorm kernels (row-per-block, f32 accumulation) =====
# One thread block per row: LLM hidden sizes (1k-16k) keep a 256-thread block
# busy via grid-stride columns, and the two-level warp/shared reduction avoids
# atomics. Accumulation is always f32 regardless of storage dtype so the
# result matches the CPU golden reference within f16 rounding only.

from std.gpu import block_dim, block_idx, thread_idx
from std.math import rsqrt
from std.memory import bitcast
from src.reduce import block_reduce_sum, block_reduce_max

comptime E4M3_MAX: Float32 = 448.0


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

    total = block_reduce_sum(ss)
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

    total = block_reduce_sum(ss)
    inv = rsqrt(total / Float32(n_cols) + eps)

    i = Int(thread_idx.x)
    while i < n_cols:
        out_ptr[base + i] = Float16(Float32(residual_io[base + i]) * inv * Float32(weight[i]))
        i += Int(block_dim.x)


def _rmsnorm_fp8_emit(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    src: UnsafePointer[Float16, MutAnyOrigin],
    weight: UnsafePointer[Float16, MutAnyOrigin],
    base: Int,
    n_cols: Int,
    inv: Float32,
):
    """Second half of the fused rmsnorm→fp8 path: write the f16 normed row
    (out_ptr, kept for the residual carry / any f16 reader), take the row absmax
    of those f16-rounded normed values, and emit ONE per-token e4m3 activation +
    per-token f32 scale in the layout `gemm_fp8_mod` consumes (codes [T,K]
    row-major, scale [T]). Numerics match the standalone rmsnorm_f16 →
    quantize_act_fp8 pair bit-for-bit (same f16 round point, same absmax/448
    scale), so PPL is unchanged — only WHERE the quant runs moves."""
    var amax_local: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i < n_cols:
        h = Float16(Float32(src[base + i]) * inv * Float32(weight[i]))
        out_ptr[base + i] = h
        a = abs(Float32(h))
        if a > amax_local:
            amax_local = a
        i += Int(block_dim.x)

    amax = block_reduce_max(amax_local)
    row = base // n_cols
    if amax == 0.0:
        if thread_idx.x == 0:
            xs[row] = 0.0
        var z = Int(thread_idx.x)
        while z < n_cols:
            xq[base + z] = 0
            z += Int(block_dim.x)
        return

    scale = amax / E4M3_MAX
    inv_s = E4M3_MAX / amax
    if thread_idx.x == 0:
        xs[row] = scale
    var q = Int(thread_idx.x)
    while q < n_cols:
        e = Scalar[DType.float8_e4m3fn](Float32(out_ptr[base + q]) * inv_s)
        xq[base + q] = bitcast[DType.int8, 1](e)
        q += Int(block_dim.x)


def rmsnorm_fp8(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    weight: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    eps: Float32,
):
    """out = rmsnorm(x)*weight, ALSO emit the shared per-token e4m3 activation.

    One block per row. Fuses the standalone rmsnorm_f16 → quantize_act_fp8 pair
    so the q/k/v (or gate/up) projections read ONE quantized activation instead
    of re-quantizing the norm output per projection (`FORGE_GEMM=fp8mod`)."""
    row = Int(block_idx.x)
    base = row * n_cols

    var ss: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i < n_cols:
        v = Float32(x[base + i])
        ss += v * v
        i += Int(block_dim.x)

    total = block_reduce_sum(ss)
    inv = rsqrt(total / Float32(n_cols) + eps)
    _rmsnorm_fp8_emit(out_ptr, xq, xs, x, weight, base, n_cols, inv)


def rmsnorm_residual_fp8(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    residual_io: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    weight: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    eps: Float32,
):
    """Fused residual-add + RMSNorm + shared fp8 activation emit: residual += x,
    out = rmsnorm(residual)*weight, and the per-token e4m3 codes + scale for the
    following gate/up (or next-layer q/k/v) projections. See rmsnorm_fp8."""
    row = Int(block_idx.x)
    base = row * n_cols

    var ss: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i < n_cols:
        v = Float32(residual_io[base + i]) + Float32(x[base + i])
        residual_io[base + i] = Float16(v)
        ss += v * v
        i += Int(block_dim.x)

    total = block_reduce_sum(ss)
    inv = rsqrt(total / Float32(n_cols) + eps)
    _rmsnorm_fp8_emit(out_ptr, xq, xs, residual_io, weight, base, n_cols, inv)
