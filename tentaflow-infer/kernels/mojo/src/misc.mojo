# ===== File: misc.mojo — embedding gather and f32-logit GEMV =====

from std.gpu import block_dim, block_idx, thread_idx
from src.reduce import block_reduce_sum


def gather_rows_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    table: UnsafePointer[Float16, MutAnyOrigin],
    ids: UnsafePointer[Int32, MutAnyOrigin],
    n_rows: Int,
    n_cols: Int,
):
    """Bezpiecznie pobiera batch wierszy F16; błędne ID zeruje bez odczytu tabeli."""
    t = Int(block_idx.x)
    row = Int(ids[t])
    dst = t * n_cols
    var i = Int(thread_idx.x)
    if row < 0 or row >= n_rows:
        while i < n_cols:
            out_ptr[dst + i] = 0.0
            i += Int(block_dim.x)
    else:
        src = row * n_cols
        while i < n_cols:
            out_ptr[dst + i] = table[src + i]
            i += Int(block_dim.x)


def gemv_f16_out_f32(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
):
    """Logit head GEMV: f16 weights/activations, f32 output so sampling never
    quantizes the distribution. Grid.x = rows (vocab)."""
    row = Int(block_idx.x)
    base = row * n_cols

    var acc: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i < n_cols:
        acc += Float32(w[base + i]) * Float32(x[i])
        i += Int(block_dim.x)

    total = block_reduce_sum(acc)
    if Int(thread_idx.x) == 0:
        y[row] = total


def gemv_q8_0_out_f32(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
):
    """Logit head GEMV over Q8_0 weights (tied-embedding GGUF models)."""
    row = Int(block_idx.x)
    blocks_per_row = n_cols // 32
    row_base = row * blocks_per_row * 34

    var acc: Float32 = 0.0
    var b = Int(thread_idx.x)
    while b < blocks_per_row:
        off = row_base + b * 34
        scale = Float32((w + off).bitcast[Float16]()[0])
        qs = (w + off + 2).bitcast[Int8]()
        xb = b * 32
        var s: Float32 = 0.0
        for k in range(32):
            s += Float32(qs[k]) * Float32(x[xb + k])
        acc += scale * s
        b += Int(block_dim.x)

    total = block_reduce_sum(acc)
    if Int(thread_idx.x) == 0:
        y[row] = total
