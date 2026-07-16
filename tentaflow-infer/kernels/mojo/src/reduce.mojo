# ===== File: reduce.mojo — shared block-level reduction primitives =====

from std.gpu import block_dim, thread_idx
from std.gpu.sync import barrier
from std.gpu.primitives import warp
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation

comptime WARP_SIZE = 32
comptime MAX_WARPS = 32


def block_reduce_sum(val: Float32) -> Float32:
    # Two-level reduction: intra-warp shuffle sum, then the first warp reduces
    # the per-warp partials staged in shared memory. Every thread receives the
    # total via the shared broadcast slot, so callers can use it uniformly.
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
