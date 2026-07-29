# =============================================================================
# Plik: gemm_q4_k_i8wmma.mojo
# Opis: Prefillowy GEMM Q4_K na CAŁKOWITOLICZBOWEJ jednostce macierzowej RDNA3
#       (WMMA int8). RDNA3 liczy int8 dwa razy szybciej niż f16, a pomiar wobec
#       llama.cpp pokazał, że to właśnie tam tracimy prefill.
# Przykład: gemm_q4_k_i8wmma_f16_bm256 zastępuje wariant f16 na długim prefillu.
# =============================================================================
#
# DLACZEGO TO SIĘ SPINA BEZ DRUGIEGO PRZEBIEGU. Wartość Q4_K to
# `q * ds - dm`, gdzie `q` ma cztery bity, `ds = d * sc`, `dm = dmin * mn`.
# Iloczyn skalarny po bloku 32 kolumn rozkłada się więc na:
#
#     Σ (q_i * ds - dm) * x_i  =  ds * Σ q_i x_i  -  dm * Σ x_i
#
# Pierwszy człon to zwykły iloczyn int8 (aktywacje skwantowane do q8_1), a drugi
# potrzebuje wyłącznie SUMY aktywacji w bloku — a tę `quantize_act_q8_1` już
# liczy i zapisuje do `xsm`. Blok kwantyzacji aktywacji ma 32 kolumny, dokładnie
# tyle co podblok skali Q4_K, więc oba podziały pokrywają się co do kolumny.

from std.gpu import block_idx, thread_idx
from std.memory import bitcast

from src.arch_wmma import wmma_i8_16x16x16
from src.gemv2 import _q4k_scale_min

comptime TILE = 16
comptime SUPERBLOCK = 256
comptime SB_BYTES = 144
comptime QS_OFFSET = 16


@always_inline
def _weight_frag_i8(
    weights: UnsafePointer[UInt8, MutAnyOrigin], superblock: Int, block: Int, half: Int
) -> SIMD[DType.int32, 4]:
    """Szesnaście czterobitowych kodów jednego wiersza jako fragment WMMA int8.

    `block` numeruje bloki 32-kolumnowe wewnątrz superbloku (0..7), `half`
    wybiera pierwszą albo drugą szesnastkę kolumn bloku.
    """
    var chunk = block // 2
    var high = (block % 2) == 1
    var packed = (
        weights + superblock + QS_OFFSET + chunk * 32 + half * TILE
    ).load[width=16, alignment=1]()
    var nibbles = packed >> 4 if high else packed & 0x0F
    return bitcast[DType.int32, 4](nibbles.cast[DType.int8]())


def gemm_q4_k_i8wmma_impl[
    WAVES_M: Int, WAVES_N: Int, MTILE: Int, NTILE: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    xsm: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """GEMM Q4_K na WMMA int8: Y[t, r] = dot(w[r], x[t]).

    `xq`/`xd`/`xsm` pochodzą z `quantize_act_q8_1`: kody [T, K], skale i sumy w
    układzie blokowym [K/32, T]. Siatka (ceil(n_rows / BN), ceil(n_tokens / BM)),
    `n_cols % 256 == 0`.
    """
    comptime BM = WAVES_M * MTILE * TILE
    comptime BN = WAVES_N * NTILE * TILE

    var lane = Int(thread_idx.x) % 32
    var wave = Int(thread_idx.x) // 32
    var base_m = Int(block_idx.y) * BM + (wave // WAVES_N) * MTILE * TILE
    var base_n = Int(block_idx.x) * BN + (wave % WAVES_N) * NTILE * TILE

    var superblocks = n_cols // SUPERBLOCK
    var blocks = n_cols // 32

    # Wiersze i tokeny poza zakresem czytają ostatni legalny indeks — ich wyniki
    # i tak nie są zapisywane, a zacisk trzyma odczyty w obrębie buforów.
    var x_base = InlineArray[Int, MTILE](fill=0)
    comptime for mt in range(MTILE):
        var m = base_m + mt * TILE + lane % 16
        if m > n_tokens - 1:
            m = n_tokens - 1
        x_base[mt] = m * n_cols
    var w_base = InlineArray[Int, NTILE](fill=0)
    comptime for nt in range(NTILE):
        var n = base_n + nt * TILE + lane % 16
        if n > n_rows - 1:
            n = n_rows - 1
        w_base[nt] = n * superblocks * SB_BYTES

    var acc = InlineArray[SIMD[DType.float32, 8], MTILE * NTILE](
        fill=SIMD[DType.float32, 8](0.0)
    )

    for b in range(blocks):
        var superblock = b // 8
        var block = b % 8
        var column = b * 32

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
            var sb_off = w_base[nt] + superblock * SB_BYTES
            var header = (weights + sb_off).load[width=16, alignment=1]()
            var d = Float32(
                (weights + sb_off).bitcast[Float16]().load[width=1, alignment=1]()[0]
            )
            var dmin = Float32(
                (weights + sb_off + 2).bitcast[Float16]().load[width=1, alignment=1]()[0]
            )
            sc, mn = _q4k_scale_min(header, block)
            var ds = d * sc
            var dm = dmin * mn

            var b_lo = _weight_frag_i8(weights, sb_off, block, 0)
            var b_hi = _weight_frag_i8(weights, sb_off, block, 1)

            comptime for mt in range(MTILE):
                var block_acc = SIMD[DType.int32, 8](0)
                block_acc = wmma_i8_16x16x16(a_lo[mt], b_lo, block_acc)
                block_acc = wmma_i8_16x16x16(a_hi[mt], b_hi, block_acc)
                comptime for i in range(8):
                    # (m, n) tego pola: m = i*2 + lane//16, n = lane % 16.
                    var m = base_m + mt * TILE + i * 2 + lane // 16
                    if m > n_tokens - 1:
                        m = n_tokens - 1
                    acc[mt * NTILE + nt][i] += (
                        Float32(block_acc[i]) * ds * xd[b * n_tokens + m]
                        - dm * xsm[b * n_tokens + m]
                    )

    comptime for mt in range(MTILE):
        comptime for nt in range(NTILE):
            comptime for i in range(8):
                var m = base_m + mt * TILE + i * 2 + lane // 16
                var n = base_n + nt * TILE + lane % 16
                if m < n_tokens and n < n_rows:
                    y[m * n_rows + n] = Float16(acc[mt * NTILE + nt][i])


comptime gemm_q4_k_i8wmma_f16_bm32 = gemm_q4_k_i8wmma_impl[2, 2, 1, 2]
comptime gemm_q4_k_i8wmma_f16_bm256 = gemm_q4_k_i8wmma_impl[4, 2, 4, 2]
