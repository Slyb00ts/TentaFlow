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


def gelu_mul_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    gate: UnsafePointer[Float16, MutAnyOrigin],
    up: UnsafePointer[Float16, MutAnyOrigin],
    n: Int,
):
    """out = gelu(gate) * up nad n kolejnymi elementami (GeGLU, rodzina Gemma).

    Wariant tanh (`gelu_pytorch_tanh`), bo to on jest w referencji Gemmy — nie
    dokladny erf. Rozjazd miedzy nimi jest rzedu 1e-3 i widoczny w logitach.
    """
    i = Int(global_idx.x)
    if i < n:
        g = Float32(gate[i])
        inner = 0.7978845608028654 * (g + 0.044715 * g * g * g)
        e = exp(2.0 * inner)
        tanh_inner = (e - 1.0) / (e + 1.0)
        out_ptr[i] = Float16(0.5 * g * (1.0 + tanh_inner) * Float32(up[i]))


def sigmoid_mul_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Float16, MutAnyOrigin],
    gate: UnsafePointer[Float16, MutAnyOrigin],
    n: Int,
):
    """out = a * sigmoid(gate) over n contiguous elements (attention output gate)."""
    i = Int(global_idx.x)
    if i < n:
        g = Float32(gate[i])
        s = 1.0 / (1.0 + exp(-g))
        out_ptr[i] = Float16(Float32(a[i]) * s)


def deinterleave_gate_f16(
    qc: UnsafePointer[Float16, MutAnyOrigin],
    gatec: UnsafePointer[Float16, MutAnyOrigin],
    q_full: UnsafePointer[Float16, MutAnyOrigin],
    head_dim: Int,
    n: Int,
):
    """De-interleave the gated Q projection [n_heads, 2*head_dim] into query and
    gate halves: for element i (head h = i // head_dim, lane d = i % head_dim),
    qc[i] = q_full[h*2*head_dim + d], gatec[i] = q_full[h*2*head_dim + head_dim + d].
    Pure data move (no math) — bit-identical to the per-head copy loop."""
    i = Int(global_idx.x)
    if i < n:
        h = i // head_dim
        d = i % head_dim
        base = h * 2 * head_dim + d
        qc[i] = q_full[base]
        gatec[i] = q_full[base + head_dim]
