# ===== File: activation.mojo — elementwise activation kernels (SwiGLU) =====

from std.gpu import block_dim, block_idx, thread_idx, global_idx
from std.math import exp


def silu_mul_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    gate: UnsafePointer[Float16, MutAnyOrigin],
    up: UnsafePointer[Float16, MutAnyOrigin],
    n: Int,
):
    """out = silu(gate) * up over n contiguous elements (SwiGLU FFN)."""
    i = Int(global_idx.x)
    if i < n:
        g = Float32(gate[i])
        s = g / (1.0 + exp(-g))
        out_ptr[i] = Float16(s * Float32(up[i]))
