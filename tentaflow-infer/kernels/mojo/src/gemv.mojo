# ===== File: gemv.mojo — fused dequant matrix-vector kernels (decode path) =====
# Decode GEMV is memory-bound: weights stream once per token, so dequant fuses
# into the dot product and never materializes f16 weights. One block per output
# row; threads stride over quant blocks and reduce at the end.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.primitives import warp
from src.reduce import block_reduce_sum

comptime Q8_0_BLOCK_BYTES = 34
comptime Q8_0_BLOCK_ELEMS = 32


def gemv_q8_0_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
):
    """y[row] = dot(dequant_q8_0(w[row, :]), x). Grid.x = rows.

    Q8_0 block: f16 scale + 32 int8 values (34 bytes). Block starts are 2-byte
    aligned (34 × k is even), so the scale can be loaded as an aligned f16.
    """
    row = Int(block_idx.x)
    blocks_per_row = n_cols // Q8_0_BLOCK_ELEMS
    row_base = row * blocks_per_row * Q8_0_BLOCK_BYTES

    var acc: Float32 = 0.0
    var b = Int(thread_idx.x)
    while b < blocks_per_row:
        off = row_base + b * Q8_0_BLOCK_BYTES
        scale = Float32((w + off).bitcast[Float16]()[0])
        qs = (w + off + 2).bitcast[Int8]()
        xb = b * Q8_0_BLOCK_ELEMS
        var s: Float32 = 0.0
        for k in range(Q8_0_BLOCK_ELEMS):
            s += Float32(qs[k]) * Float32(x[xb + k])
        acc += scale * s
        b += Int(block_dim.x)

    total = block_reduce_sum(acc)
    if Int(thread_idx.x) == 0:
        y[row] = Float16(total)


def gemv_f16_bias(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    bias: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
):
    """f16 GEMV with bias: y[row] = dot(w[row, :], x) + bias[row].

    Whisper-class linears carry biases; the LLM path keeps the bias-free
    variant below so it pays nothing for them.
    """
    row = Int(block_idx.x)
    base = row * n_cols

    var acc: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i < n_cols:
        acc += Float32(w[base + i]) * Float32(x[i])
        i += Int(block_dim.x)

    total = block_reduce_sum(acc)
    if Int(thread_idx.x) == 0:
        y[row] = Float16(total + Float32(bias[row]))


def gemv_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
):
    """Unquantized f16 GEMV: y[row] = dot(w[row, :], x). Grid.x = rows."""
    row = Int(block_idx.x)
    base = row * n_cols

    var acc: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i < n_cols:
        acc += Float32(w[base + i]) * Float32(x[i])
        i += Int(block_dim.x)

    total = block_reduce_sum(acc)
    if Int(thread_idx.x) == 0:
        y[row] = Float16(total)
