# ===== File: gemm.mojo — batched prefill GEMM: Y[T,rows] = X[T,cols] · W^T =====
# Prefill processes T tokens at once. Decomposition: 8 warps per block, one
# output row per warp, one token per lane (grid.y tiles tokens by 32). The
# weight row is loaded once per warp — every lane reads the same bytes, which
# the hardware serves as a broadcast — giving a 32× weight-reuse factor over
# per-token GEMV. Activations are consumed TRANSPOSED (xT[cols, T]) so lane t
# reads consecutive addresses (coalesced); the transpose kernel below produces
# that layout once per chunk.

from std.gpu import block_dim, block_idx, thread_idx
from std.memory import bitcast
from src.gemv2 import _e2m1x8, _f8e4m3s

comptime WARP = 32
comptime ROWS_PER_BLOCK = 8


def transpose_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_tokens: Int,
    n_cols: Int,
):
    """xT[c, t] = x[t, c]. Grid: (ceil(cols/32), ceil(tokens/8)); block 256 =
    32×8 tile; naive but tiny next to the GEMMs it feeds."""
    c = Int(block_idx.x) * 32 + Int(thread_idx.x) % 32
    t = Int(block_idx.y) * 8 + Int(thread_idx.x) // 32
    if c < n_cols and t < n_tokens:
        out_ptr[c * n_tokens + t] = x[t * n_cols + c]


comptime TOKENS_PER_LANE = 4


def gemm_q8_0_xt_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    xt: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Y[t, row] = dot(dequant(w[row]), x[t]).

    Grid: (ceil(rows/8), ceil(T/128)); each lane carries 4 consecutive tokens
    so the per-k activation traffic amortizes over a 128-token tile and the
    x loads are 8-byte vectors. Callers must pad xt to a multiple of 4 tokens
    (the engine rounds its chunk buffers up).
    """
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    t0 = (Int(block_idx.y) * WARP + lane) * TOKENS_PER_LANE
    if t0 >= n_tokens:
        return

    blocks_per_row = n_cols // 32
    row_base = row * blocks_per_row * 34

    var acc = SIMD[DType.float32, TOKENS_PER_LANE](0.0)
    for b in range(blocks_per_row):
        off = row_base + b * 34
        # Same bytes for every lane — broadcast, one memory transaction.
        scale = Float32((w + off).bitcast[Float16]()[0])
        v16 = (w + off + 2).bitcast[UInt16]().load[width=16]()
        q = bitcast[DType.int8, 32](v16).cast[DType.float32]()
        var dot = SIMD[DType.float32, TOKENS_PER_LANE](0.0)

        comptime for j in range(32):
            xv = (xt + (b * 32 + j) * n_tokens + t0).load[
                width=TOKENS_PER_LANE, alignment=8
            ]().cast[DType.float32]()
            dot += q[j] * xv
        acc += scale * dot

    comptime for u in range(TOKENS_PER_LANE):
        if t0 + u < n_tokens:
            y[(t0 + u) * n_rows + row] = Float16(acc[u])


def gemm_nvfp4_xt_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[UInt8, MutAnyOrigin],
    xt: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    inv_global_scale: Float32,
):
    """NVFP4 batched GEMM, same decomposition as the Q8_0 variant."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    t0 = (Int(block_idx.y) * WARP + lane) * TOKENS_PER_LANE
    if t0 >= n_tokens:
        return

    groups = n_cols // 16
    packed_row = row * (n_cols // 2)
    scales_row = row * groups

    var acc = SIMD[DType.float32, TOKENS_PER_LANE](0.0)
    for g in range(groups):
        s = _f8e4m3s(scales[scales_row + g]) * inv_global_scale
        qv = (packed + packed_row + g * 8).load[width=8, alignment=8]()
        lo = _e2m1x8(qv & 0x0F)
        hi = _e2m1x8(qv >> 4)
        var dot = SIMD[DType.float32, TOKENS_PER_LANE](0.0)

        comptime for j in range(8):
            xe = (xt + (g * 16 + 2 * j) * n_tokens + t0).load[
                width=TOKENS_PER_LANE, alignment=8
            ]().cast[DType.float32]()
            xo = (xt + (g * 16 + 2 * j + 1) * n_tokens + t0).load[
                width=TOKENS_PER_LANE, alignment=8
            ]().cast[DType.float32]()
            dot += lo[j] * xe + hi[j] * xo
        acc += s * dot

    comptime for u in range(TOKENS_PER_LANE):
        if t0 + u < n_tokens:
            y[(t0 + u) * n_rows + row] = Float16(acc[u])


def gemm_f16_xt_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[Float16, MutAnyOrigin],
    xt: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """f16 batched GEMM, same decomposition."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    t0 = (Int(block_idx.y) * WARP + lane) * TOKENS_PER_LANE
    if t0 >= n_tokens:
        return

    base = row * n_cols
    var acc = SIMD[DType.float32, TOKENS_PER_LANE](0.0)
    var k = 0
    while k + 8 <= n_cols:
        wv = (w + base + k).load[width=8, alignment=16]().cast[DType.float32]()
        var dot = SIMD[DType.float32, TOKENS_PER_LANE](0.0)

        comptime for j in range(8):
            xv = (xt + (k + j) * n_tokens + t0).load[
                width=TOKENS_PER_LANE, alignment=8
            ]().cast[DType.float32]()
            dot += wv[j] * xv
        acc += dot
        k += 8

    comptime for u in range(TOKENS_PER_LANE):
        if t0 + u < n_tokens:
            y[(t0 + u) * n_rows + row] = Float16(acc[u])
