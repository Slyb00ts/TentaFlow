# ===== File: onnx_ops.mojo — f32 GPU kernels for the ONNX subset executor =====
# These back forge-onnx's hybrid interpreter: the heavy VAD arithmetic (Conv,
# LSTM, activations, magnitude, reduction) runs here on the GPU. Shape/control
# ops stay on the host. Everything is Float32 — Silero VAD (and the other staged
# ONNX models) run in single precision, so there is no f16 path.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import exp, log, sqrt, tanh

comptime LSTM_MAX_HIDDEN = 512


def conv1d_f32(
    out_ptr: UnsafePointer[Float32, MutAnyOrigin],
    x: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[Float32, MutAnyOrigin],
    bias: UnsafePointer[Float32, MutAnyOrigin],
    in_ch: Int,
    in_t: Int,
    out_ch: Int,
    out_t: Int,
    ksize: Int,
    stride: Int,
    pad: Int,
    has_bias: Int,
):
    """General 1-D convolution, group=1, dilation=1 (ONNX Conv).

    Layouts (batch fixed at 1):
      x:      [in_ch, in_t]
      w:      [out_ch, in_ch, ksize]
      bias:   [out_ch]        (ignored when has_bias == 0)
      out:    [out_ch, out_t]
    Grid: (ceil(out_t / block), out_ch) — one thread per output sample.
    """
    t = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    oc = Int(block_idx.y)
    if t >= out_t or oc >= out_ch:
        return
    var acc: Float32 = 0.0
    if has_bias != 0:
        acc = bias[oc]
    start = t * stride - pad
    w_oc = oc * in_ch * ksize
    for ic in range(in_ch):
        xb = ic * in_t
        wb = w_oc + ic * ksize
        for k in range(ksize):
            src = start + k
            if src >= 0 and src < in_t:
                acc += w[wb + k] * x[xb + src]
    out_ptr[oc * out_t + t] = acc


def relu_f32(
    out_ptr: UnsafePointer[Float32, MutAnyOrigin],
    x: UnsafePointer[Float32, MutAnyOrigin],
    n: Int,
):
    i = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    if i < n:
        v = x[i]
        out_ptr[i] = v if v > 0.0 else Float32(0.0)


def sigmoid_f32(
    out_ptr: UnsafePointer[Float32, MutAnyOrigin],
    x: UnsafePointer[Float32, MutAnyOrigin],
    n: Int,
):
    i = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    if i < n:
        out_ptr[i] = Float32(1.0) / (Float32(1.0) + exp(-x[i]))


def add_f32(
    out_ptr: UnsafePointer[Float32, MutAnyOrigin],
    a: UnsafePointer[Float32, MutAnyOrigin],
    b: UnsafePointer[Float32, MutAnyOrigin],
    n: Int,
):
    # Same-shape elementwise add; broadcasting is materialized host-side before
    # launch (Silero's only Adds are equal-shape magnitude terms).
    i = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    if i < n:
        out_ptr[i] = a[i] + b[i]


def pow_f32(
    out_ptr: UnsafePointer[Float32, MutAnyOrigin],
    x: UnsafePointer[Float32, MutAnyOrigin],
    e: Float32,
    n: Int,
):
    i = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    if i < n:
        v = x[i]
        if e == 2.0:
            out_ptr[i] = v * v
        elif e == 0.5:
            out_ptr[i] = sqrt(v)
        elif e == 1.0:
            out_ptr[i] = v
        else:
            # General real exponent; base is non-negative on the paths that hit
            # this (magnitude/energy terms).
            out_ptr[i] = exp(e * log(v))


def sqrt_f32(
    out_ptr: UnsafePointer[Float32, MutAnyOrigin],
    x: UnsafePointer[Float32, MutAnyOrigin],
    n: Int,
):
    i = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    if i < n:
        out_ptr[i] = sqrt(x[i])


def reduce_mean_f32(
    out_ptr: UnsafePointer[Float32, MutAnyOrigin],
    x: UnsafePointer[Float32, MutAnyOrigin],
    outer: Int,
    axis: Int,
    inner: Int,
):
    """out[o, i] = mean over the reduced axis of x[o, :, i].

    x is viewed as [outer, axis, inner] row-major; one thread per (o, i).
    """
    idx = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    total = outer * inner
    if idx >= total:
        return
    o = idx // inner
    inr = idx % inner
    var acc: Float32 = 0.0
    base = o * axis * inner + inr
    for a in range(axis):
        acc += x[base + a * inner]
    out_ptr[idx] = acc / Float32(axis)


def lstm_f32(
    y: UnsafePointer[Float32, MutAnyOrigin],
    yh: UnsafePointer[Float32, MutAnyOrigin],
    yc: UnsafePointer[Float32, MutAnyOrigin],
    x: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[Float32, MutAnyOrigin],
    r: UnsafePointer[Float32, MutAnyOrigin],
    b: UnsafePointer[Float32, MutAnyOrigin],
    h0: UnsafePointer[Float32, MutAnyOrigin],
    c0: UnsafePointer[Float32, MutAnyOrigin],
    seq: Int,
    input_size: Int,
    hidden: Int,
):
    """Single-direction, batch-1 ONNX LSTM. Gate order i, o, f, c (ONNX).

    Shapes (direction/batch already squeezed by the host):
      x:  [seq, input_size]     w: [4*hidden, input_size]
      r:  [4*hidden, hidden]    b: [8*hidden]  (Wb[0:4h] then Rb[4h:8h])
      h0/c0: [hidden]           y: [seq, hidden]   yh/yc: [hidden]
    One block of `hidden` threads; the recurrent state lives in shared memory.
    """
    j = Int(thread_idx.x)
    if j >= hidden:
        return
    hs = stack_allocation[
        LSTM_MAX_HIDDEN, Float32, address_space = AddressSpace.SHARED
    ]()
    var c_reg: Float32 = c0[j]
    hs[j] = h0[j]
    barrier()

    ri = 0 * hidden + j
    ro = 1 * hidden + j
    rf = 2 * hidden + j
    rg = 3 * hidden + j
    bi = b[0 * hidden + j] + b[4 * hidden + j]
    bo = b[1 * hidden + j] + b[5 * hidden + j]
    bf = b[2 * hidden + j] + b[6 * hidden + j]
    bg = b[3 * hidden + j] + b[7 * hidden + j]

    for t in range(seq):
        xb = t * input_size
        var ai: Float32 = bi
        var ao: Float32 = bo
        var af: Float32 = bf
        var ag: Float32 = bg
        for k in range(input_size):
            xv = x[xb + k]
            ai += w[ri * input_size + k] * xv
            ao += w[ro * input_size + k] * xv
            af += w[rf * input_size + k] * xv
            ag += w[rg * input_size + k] * xv
        for k in range(hidden):
            hv = hs[k]
            ai += r[ri * hidden + k] * hv
            ao += r[ro * hidden + k] * hv
            af += r[rf * hidden + k] * hv
            ag += r[rg * hidden + k] * hv
        ii = Float32(1.0) / (Float32(1.0) + exp(-ai))
        oo = Float32(1.0) / (Float32(1.0) + exp(-ao))
        ff = Float32(1.0) / (Float32(1.0) + exp(-af))
        gg = tanh(ag)
        c_reg = ff * c_reg + ii * gg
        h_new = oo * tanh(c_reg)
        # Every thread must finish reading the previous hs before any overwrite,
        # and the new hs must be visible before the next step reads it.
        barrier()
        hs[j] = h_new
        barrier()
        y[t * hidden + j] = h_new

    yh[j] = hs[j]
    yc[j] = c_reg
