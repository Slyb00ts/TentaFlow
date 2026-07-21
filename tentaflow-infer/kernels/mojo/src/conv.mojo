# ===== File: conv.mojo — 1-D convolution + GELU (Whisper audio encoder stem) =====

from std.gpu import block_dim, block_idx, thread_idx
from std.math import erf, sqrt


def _gelu(v: Float32) -> Float32:
    # Exact erf formulation — matches the PyTorch/Whisper reference closer
    # than the tanh approximation at f16 tolerances.
    return 0.5 * v * (1.0 + erf(v / sqrt(Float32(2.0))))


def gelu_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n: Int,
):
    i = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    if i < n:
        out_ptr[i] = Float16(_gelu(Float32(x[i])))


def conv1d_k3_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    weight: UnsafePointer[Float16, MutAnyOrigin],
    bias: UnsafePointer[Float16, MutAnyOrigin],
    in_ch: Int,
    in_t: Int,
    out_t: Int,
    stride: Int,
    apply_gelu: Int,
):
    """1-D conv, kernel size 3, padding 1, fused optional GELU.

    Layouts (Whisper GGUF convention):
      x:      [in_ch, in_t]        (channel rows contiguous in time)
      weight: [out_ch, in_ch, 3]
      out:    [out_ch, out_t]
    Grid: (ceil(out_t / block), out_ch) — one thread per output sample.
    """
    t = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    oc = Int(block_idx.y)
    if t >= out_t:
        return

    center = t * stride
    var acc: Float32 = Float32(bias[oc])
    w_base = oc * in_ch * 3
    for ic in range(in_ch):
        x_base = ic * in_t
        wb = w_base + ic * 3
        # k = 0,1,2 maps to input offsets center-1, center, center+1 (pad 1).
        src = center - 1
        if src >= 0 and src < in_t:
            acc += Float32(weight[wb]) * Float32(x[x_base + src])
        if center < in_t:
            acc += Float32(weight[wb + 1]) * Float32(x[x_base + center])
        src = center + 1
        if src < in_t:
            acc += Float32(weight[wb + 2]) * Float32(x[x_base + src])

    if apply_gelu != 0:
        acc = _gelu(acc)
    out_ptr[oc * out_t + t] = Float16(acc)
