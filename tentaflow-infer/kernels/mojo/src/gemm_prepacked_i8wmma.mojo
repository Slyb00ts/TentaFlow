# =============================================================================
# Plik: gemm_prepacked_i8wmma.mojo
# Opis: Prefillowy GEMM na WMMA int8 czytający wagi PRZEPAKOWANE przy ładowaniu:
#       kody int8 i tablice skal osobno. W pętli nie ma już wyciągania półbajtów
#       ani rozpakowywania sześciobitowych skal — to jedyna różnica wobec
#       `gemm_q4_k_i8wmma`, która liczy to samo w locie.
# Przykład: pomiar wobec kafla f16 rozstrzyga, czy przepakowanie się opłaca.
# =============================================================================
#
# UKŁAD WEJŚCIA (wszystko wiersz-major, `cols % 32 == 0`):
#   w_i8 : [rows][cols]      kody int8 (czterobitowe wartości Q4_K bez offsetu)
#   w_ds : [rows][cols / 32] skala bloku, czyli `d * sc`
#   w_dm : [rows][cols / 32] offset bloku, czyli `dmin * mn`
# Aktywacje jak w `quantize_act_q8_1`: kody [T, K], skale i sumy [K/32, T].

from std.gpu import block_idx, thread_idx
from std.memory import bitcast

from src.arch_wmma import wmma_i8_16x16x16

comptime TILE = 16


def gemm_prepacked_i8wmma_impl[
    WAVES_M: Int, WAVES_N: Int, MTILE: Int, NTILE: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w_i8: UnsafePointer[Int8, MutAnyOrigin],
    w_ds: UnsafePointer[Float32, MutAnyOrigin],
    w_dm: UnsafePointer[Float32, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    xsm: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Y[t, r] = Σ_b (ds[r,b] * Σ q·x - dm[r,b] * Σx), wszystko już rozpakowane."""
    comptime BM = WAVES_M * MTILE * TILE
    comptime BN = WAVES_N * NTILE * TILE

    var lane = Int(thread_idx.x) % 32
    var wave = Int(thread_idx.x) // 32
    var base_m = Int(block_idx.y) * BM + (wave // WAVES_N) * MTILE * TILE
    var base_n = Int(block_idx.x) * BN + (wave % WAVES_N) * NTILE * TILE
    var blocks = n_cols // 32

    var x_base = InlineArray[Int, MTILE](fill=0)
    comptime for mt in range(MTILE):
        var m = base_m + mt * TILE + lane % 16
        if m > n_tokens - 1:
            m = n_tokens - 1
        x_base[mt] = m * n_cols
    var w_base = InlineArray[Int, NTILE](fill=0)
    var w_scale = InlineArray[Int, NTILE](fill=0)
    comptime for nt in range(NTILE):
        var n = base_n + nt * TILE + lane % 16
        if n > n_rows - 1:
            n = n_rows - 1
        w_base[nt] = n * n_cols
        w_scale[nt] = n * blocks

    var acc = InlineArray[SIMD[DType.float32, 8], MTILE * NTILE](
        fill=SIMD[DType.float32, 8](0.0)
    )

    for b in range(blocks):
        var column = b * 32

        var act_d = InlineArray[SIMD[DType.float32, 8], MTILE](
            fill=SIMD[DType.float32, 8](0.0)
        )
        var act_s = InlineArray[SIMD[DType.float32, 8], MTILE](
            fill=SIMD[DType.float32, 8](0.0)
        )
        comptime for mt in range(MTILE):
            comptime for i in range(8):
                var m = base_m + mt * TILE + i * 2 + lane // 16
                if m > n_tokens - 1:
                    m = n_tokens - 1
                act_d[mt][i] = xd[b * n_tokens + m]
                act_s[mt][i] = xsm[b * n_tokens + m]

        var a_lo = InlineArray[SIMD[DType.int32, 4], MTILE](
            fill=SIMD[DType.int32, 4](0)
        )
        var a_hi = InlineArray[SIMD[DType.int32, 4], MTILE](
            fill=SIMD[DType.int32, 4](0)
        )
        comptime for mt in range(MTILE):
            a_lo[mt] = bitcast[DType.int32, 4](
                (xq + x_base[mt] + column).load[width=16, alignment=1]()
            )
            a_hi[mt] = bitcast[DType.int32, 4](
                (xq + x_base[mt] + column + TILE).load[width=16, alignment=1]()
            )

        comptime for nt in range(NTILE):
            var b_lo = bitcast[DType.int32, 4](
                (w_i8 + w_base[nt] + column).load[width=16, alignment=1]()
            )
            var b_hi = bitcast[DType.int32, 4](
                (w_i8 + w_base[nt] + column + TILE).load[width=16, alignment=1]()
            )
            var ds = w_ds[w_scale[nt] + b]
            var dm = w_dm[w_scale[nt] + b]
            comptime for mt in range(MTILE):
                var block_acc = SIMD[DType.int32, 8](0)
                block_acc = wmma_i8_16x16x16(a_lo[mt], b_lo, block_acc)
                block_acc = wmma_i8_16x16x16(a_hi[mt], b_hi, block_acc)
                acc[mt * NTILE + nt] += (
                    block_acc.cast[DType.float32]() * ds * act_d[mt]
                    - dm * act_s[mt]
                )

    comptime for mt in range(MTILE):
        comptime for nt in range(NTILE):
            comptime for i in range(8):
                var m = base_m + mt * TILE + i * 2 + lane // 16
                var n = base_n + nt * TILE + lane % 16
                if m < n_tokens and n < n_rows:
                    y[m * n_rows + n] = Float16(acc[mt * NTILE + nt][i])


comptime gemm_prepacked_i8wmma_f16_bm256 = gemm_prepacked_i8wmma_impl[4, 2, 4, 2]
