# ===== File: decode_dp4a_batch.mojo — weight-stationary small-batch dp4a GEMV (Q4_K / Q6_K) =====
# Batched-decode projections for T = 2/4/8/16 concurrent sequences: each weight
# superblock is decoded ONCE and dotted against every token's pre-quantized
# q8_1 activation, so a decode step costs roughly one weight sweep instead of
# the >=64-token GEMM tile the i8mma path pads to. Per-row math (lane
# decomposition, dp4a order, scale/min extraction) mirrors decode_dp4a.mojo's
# _dot_q4k_i8 / _dot_q6k_i8 exactly; only the activation source differs:
# global int8 codes [T, cols] plus block-major scales/sums [cols/32, T] as
# written by quantize_act_q8_1 (the same prepass the prefill GEMMs share).

from std.gpu import block_idx, thread_idx
from std.gpu.primitives import warp
from std.memory import bitcast
from src.decode_dp4a import _dp4a, _q4k_scale_pair

comptime WARP = 32
comptime ROWS_PER_BLOCK = 4


def gemv_q4_k_dp4a_batch_impl[token_tile: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    xq_g: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """y[t, row] = W·x[t] over Q4_K weights; one superblock decode serves all
    tokens. `xd`/`xs` are the q8_1 per-32-segment scales and true f32 segment
    sums (min term), block-major [seg, token]."""
    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return

    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 144
    # Szesnaście wątków na superblok, nie osiem. Przy ośmiu każdy brał
    # szesnastobajtowy kawałek i miał dwa razy dłuższy łańcuch zależności przy
    # połowie ładowań w locie — a ten kernel siedział na 34% pasma i nie
    # reagował ani na liczbę warpów w bloku, ani na liczbę wierszy na warp.
    p = lane % 16
    c = p // 4
    quarter = p % 4

    var acc = InlineArray[Float32, token_tile](fill=0.0)
    var mins = InlineArray[Float32, token_tile](fill=0.0)
    var b = lane // 16
    while b < blocks_per_row:
        off = row_base + b * 144
        dm = (w + off).bitcast[Float16]().load[width=2, alignment=4]()
        d = Float32(dm[0])
        dmin = Float32(dm[1])
        sc, mn = _q4k_scale_pair(w, off, c)
        qv = (w + off + 16 + c * 32 + quarter * 8).bitcast[UInt32]().load[
            width=2, alignment=8
        ]()
        seg = b * 8 + 2 * c
        comptime for token in range(token_tile):
            if token < n_tokens:
                xqi = (xq_g + token * n_cols).bitcast[Int32]()
                var s_lo: Int32 = 0
                var s_hi: Int32 = 0
                comptime for t in range(2):
                    q = Int32(qv[t])
                    s_lo = _dp4a(
                        q & 0x0F0F0F0F, xqi[seg * 8 + quarter * 2 + t], s_lo
                    )
                    s_hi = _dp4a(
                        (q >> 4) & 0x0F0F0F0F,
                        xqi[seg * 8 + 8 + quarter * 2 + t],
                        s_hi,
                    )
                acc[token] += d * sc[0] * xd[seg * n_tokens + token] * Float32(
                    s_lo
                )
                acc[token] += d * sc[1] * xd[
                    (seg + 1) * n_tokens + token
                ] * Float32(s_hi)
                if quarter == 0:
                    mins[token] += dmin * (
                        mn[0] * xs[seg * n_tokens + token]
                        + mn[1] * xs[(seg + 1) * n_tokens + token]
                    )
        b += 2

    comptime for token in range(token_tile):
        total = warp.sum(acc[token] - mins[token])
        if lane == 0 and token < n_tokens:
            y[token * n_rows + row] = Float16(total)


def gemv_q6_k_dp4a_batch_impl[token_tile: Int, OUT: DType](
    y: UnsafePointer[Scalar[OUT], MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    xq_g: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """y[t, row] = W·x[t] over Q6_K weights (no min term); lane decomposition
    and the -32 bias fold match _dot_q6k_i8."""
    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return

    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 210
    qh_w = 8 * (lane // 16) + lane % 8
    qh_shift = 2 * ((lane % 16) // 8)
    bq8 = 4 * (lane // 16) + (lane % 16) // 8
    sc_off = 8 * (lane // 16) + (lane % 16) // 4
    u_w = lane % 8

    var acc = InlineArray[Float32, token_tile](fill=0.0)
    var b = 0
    while b < blocks_per_row:
        off = row_base + b * 210
        d = Float32((w + off + 208).bitcast[Float16]()[0])
        vl = bitcast[DType.int32, 1](
            (w + off + 4 * lane).bitcast[UInt16]().load[width=2]()
        )[0]
        vh = (
            bitcast[DType.int32, 1](
                (w + off + 128 + 4 * qh_w).bitcast[UInt16]().load[width=2]()
            )[0]
            >> Int32(qh_shift)
        )
        sp = (w + off + 192 + sc_off).bitcast[Int8]()
        sc0 = Float32(Int(sp[0]))
        sc1 = Float32(Int(sp[4]))

        comptime for i in range(2):
            vil = (vl >> Int32(4 * i)) & 0x0F0F0F0F
            vih = ((vh >> Int32(4 * i)) << 4) & 0x30303030
            q = vil | vih
            seg = b * 8 + bq8 + 2 * i
            comptime for token in range(token_tile):
                if token < n_tokens:
                    xqi = (xq_g + token * n_cols).bitcast[Int32]()
                    u = xqi[seg * 8 + u_w]
                    s = _dp4a(q, u, 0)
                    su = _dp4a(0x01010101, u, 0)
                    if i == 0:
                        acc[token] += d * xd[
                            seg * n_tokens + token
                        ] * sc0 * Float32(s - 32 * su)
                    else:
                        acc[token] += d * xd[
                            seg * n_tokens + token
                        ] * sc1 * Float32(s - 32 * su)
        b += 1

    comptime for token in range(token_tile):
        total = warp.sum(acc[token])
        if lane == 0 and token < n_tokens:
            y[token * n_rows + row] = total.cast[OUT]()


comptime gemv_q4_k_dp4a_batch_b2 = gemv_q4_k_dp4a_batch_impl[2]
comptime gemv_q4_k_dp4a_batch_b4 = gemv_q4_k_dp4a_batch_impl[4]
comptime gemv_q4_k_dp4a_batch_b8 = gemv_q4_k_dp4a_batch_impl[8]
comptime gemv_q4_k_dp4a_batch_b16 = gemv_q4_k_dp4a_batch_impl[16]
comptime gemv_q6_k_dp4a_batch_b2 = gemv_q6_k_dp4a_batch_impl[2, DType.float16]
comptime gemv_q6_k_dp4a_batch_b4 = gemv_q6_k_dp4a_batch_impl[4, DType.float16]
comptime gemv_q6_k_dp4a_batch_b8 = gemv_q6_k_dp4a_batch_impl[8, DType.float16]
comptime gemv_q6_k_dp4a_batch_b16 = gemv_q6_k_dp4a_batch_impl[16, DType.float16]
# Wariant f32 dla batchowej głowy logitów: ta sama matematyka i ten sam odczyt
# wag, inny tylko typ zapisu. Bez niego weryfikacja MTP przy głowie Q6_K
# czytałaby całą głowę RAZ NA TOKEN draftu zamiast raz na cykl.
comptime gemv_q6_k_dp4a_batch_out_f32_b2 = gemv_q6_k_dp4a_batch_impl[2, DType.float32]
comptime gemv_q6_k_dp4a_batch_out_f32_b4 = gemv_q6_k_dp4a_batch_impl[4, DType.float32]
comptime gemv_q6_k_dp4a_batch_out_f32_b8 = gemv_q6_k_dp4a_batch_impl[8, DType.float32]
