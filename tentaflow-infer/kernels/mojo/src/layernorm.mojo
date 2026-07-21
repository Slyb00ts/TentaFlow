# ===== File: layernorm.mojo — LayerNorm kernels (Whisper-class encoders/decoders) =====
# Same row-per-block strategy as rmsnorm; LayerNorm additionally subtracts the
# mean and applies a bias, both in f32 for parity with the CPU reference.

from std.gpu import block_dim, block_idx, thread_idx
from std.math import rsqrt
from src.reduce import block_reduce_sum


def layernorm_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    weight: UnsafePointer[Float16, MutAnyOrigin],
    bias: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    eps: Float32,
):
    """out[row] = (x[row] - mean) / sqrt(var + eps) * weight + bias."""
    row = Int(block_idx.x)
    base = row * n_cols

    var s: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i < n_cols:
        s += Float32(x[base + i])
        i += Int(block_dim.x)
    mean = block_reduce_sum(s) / Float32(n_cols)

    var ss: Float32 = 0.0
    i = Int(thread_idx.x)
    while i < n_cols:
        d = Float32(x[base + i]) - mean
        ss += d * d
        i += Int(block_dim.x)
    inv = rsqrt(block_reduce_sum(ss) / Float32(n_cols) + eps)

    i = Int(thread_idx.x)
    while i < n_cols:
        v = (Float32(x[base + i]) - mean) * inv
        out_ptr[base + i] = Float16(v * Float32(weight[i]) + Float32(bias[i]))
        i += Int(block_dim.x)


def layernorm_residual_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    residual_io: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    weight: UnsafePointer[Float16, MutAnyOrigin],
    bias: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    eps: Float32,
):
    """Fused residual-add + LayerNorm: residual += x, out = ln(residual).

    Mirrors rmsnorm_residual_f16 so pre-norm transformer blocks chain the
    residual stream without a standalone add kernel.
    """
    row = Int(block_idx.x)
    base = row * n_cols

    var s: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i < n_cols:
        v = Float32(residual_io[base + i]) + Float32(x[base + i])
        residual_io[base + i] = Float16(v)
        s += v
        i += Int(block_dim.x)
    mean = block_reduce_sum(s) / Float32(n_cols)

    var ss: Float32 = 0.0
    i = Int(thread_idx.x)
    while i < n_cols:
        d = Float32(residual_io[base + i]) - mean
        ss += d * d
        i += Int(block_dim.x)
    inv = rsqrt(block_reduce_sum(ss) / Float32(n_cols) + eps)

    i = Int(thread_idx.x)
    while i < n_cols:
        v = (Float32(residual_io[base + i]) - mean) * inv
        out_ptr[base + i] = Float16(v * Float32(weight[i]) + Float32(bias[i]))
        i += Int(block_dim.x)
