# =============================================================================
# Plik: gemm_legacy32_wmma.mojo
# Opis: Kafelkowany GEMM starych formatów 32-elementowych (Q4_0, Q4_1, Q5_0,
#       Q5_1) na jednostkach macierzowych RDNA3 (WMMA). Bez niego żaden z nich
#       nie uruchamiał się na AMD — jedyny wariant w drzewie wymagał fragmentu
#       `mma`, którego RDNA nie ma.
# Przykład: gemm_q4_0_wmma_f16_bm256 obsługuje długi prefill bez repacku.
# =============================================================================
#
# UKŁAD BLOKU (zgodny z `gemm_legacy32_impl`, jeden blok to 32 kolumny):
#   Q4_0  18 B: d(f16)              + 16 B kodów, wartość `(nib - 8) * d`
#   Q4_1  20 B: d(f16), m(f16)      + 16 B kodów, wartość `nib * d + m`
#   Q5_0  22 B: d(f16), qh(u32)     + 16 B kodów, wartość `(nib|bit<<4 - 16) * d`
#   Q5_1  24 B: d(f16), m(f16), qh  + 16 B kodów, wartość `(nib|bit<<4) * d + m`
#
# Kolumny 0..15 leżą w MŁODSZYCH półbajtach bajtów 0..15, kolumny 16..31 w
# starszych. Piąty bit kolumny `e` to bit `e` słowa `qh`. Kafel WMMA ma bok 16,
# więc grupa szesnastu kolumn pokrywa równo jedną połówkę bloku i ma jednolity
# wybór półbajtu — dlatego rozpakowanie jest bezgałęziowe.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation

from src.arch_wmma import wmma_f16_16x16x16

comptime TILE = 16  # bok kafla WMMA
comptime BLOCK_VALUES = 32  # kolumny opisane jednym blokiem
comptime GROUPS = BLOCK_VALUES // TILE


@always_inline
def _weight_frag[FMT: Int](
    weights: UnsafePointer[UInt8, MutAnyOrigin], block: Int, group: Int
) -> SIMD[DType.float16, 16]:
    """Szesnaście wag jednego wiersza dla połówki bloku, już przeskalowanych."""
    comptime HAS_MIN = FMT == 1 or FMT == 3
    comptime HAS_QH = FMT >= 2
    comptime QS_OFF = 2 + (2 if FMT == 1 else 0) + (4 if FMT == 2 else 0) + (
        6 if FMT == 3 else 0
    )

    var d = (weights + block).bitcast[Float16]().load[width=1, alignment=1]()[0]
    var packed = (weights + block + QS_OFF).load[width=16, alignment=1]()
    var nibbles = packed >> 4 if group == 1 else packed & 0x0F
    var q = nibbles.cast[DType.int16]()

    comptime if HAS_QH:
        comptime QH_OFF = 2 + (2 if FMT == 3 else 0)
        var lo = (weights + block + QH_OFF).bitcast[UInt16]().load[width=1, alignment=1]()[0]
        var hi = (weights + block + QH_OFF + 2).bitcast[UInt16]().load[
            width=1, alignment=1
        ]()[0]
        var qh = UInt32(lo) | (UInt32(hi) << 16)
        var iota = SIMD[DType.uint32, 16](
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15
        )
        var bits = ((SIMD[DType.uint32, 16](qh) >> (iota + UInt32(group * 16))) & 1)
        q += bits.cast[DType.int16]() * 16

    comptime if FMT == 0:
        q -= 8
    comptime if FMT == 2:
        q -= 16

    var out = q.cast[DType.float16]() * d
    comptime if HAS_MIN:
        out += (weights + block + 2).bitcast[Float16]().load[width=1, alignment=1]()[0]
    return out


def gemm_legacy32_wmma_impl[
    WAVES_M: Int, WAVES_N: Int, MTILE: Int, NTILE: Int, FMT: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """GEMM starego formatu 32-elementowego na WMMA: Y[t, r] = dot(w[r], x[t]).

    Siatka (ceil(n_rows / BN), ceil(n_tokens / BM)), blok WAVES_M*WAVES_N*32
    wątków, `n_cols % 32 == 0`. Kontrakt argumentów jest identyczny z rodziną
    `gemm_q4_0_f16`, więc launcher wybiera wariant wyłącznie po architekturze.
    """
    comptime BM = WAVES_M * MTILE * TILE
    comptime BN = WAVES_N * NTILE * TILE
    comptime BB = 18 + 2 * FMT

    var lane = Int(thread_idx.x) % 32
    var wave = Int(thread_idx.x) // 32
    var base_m = Int(block_idx.y) * BM + (wave // WAVES_N) * MTILE * TILE
    var base_n = Int(block_idx.x) * BN + (wave % WAVES_N) * NTILE * TILE

    var blocks_per_row = n_cols // BLOCK_VALUES

    # Wiersze i tokeny poza zakresem czytają ostatni legalny indeks — ich wyniki
    # i tak nie są zapisywane, a zacisk trzyma odczyty w obrębie buforów.
    var x_base = InlineArray[Int, MTILE](fill=0)
    comptime for mt in range(MTILE):
        var m = base_m + mt * TILE + lane % 16
        if m > n_tokens - 1:
            m = n_tokens - 1
        x_base[mt] = m * n_cols

    var acc = InlineArray[SIMD[DType.float32, 8], MTILE * NTILE](
        fill=SIMD[DType.float32, 8](0.0)
    )

    # Wagi rozpakowujemy RAZ NA BLOK do LDS — inaczej każda z WAVES_M fal wzdłuż
    # tokenów dekwantyzowałaby dokładnie te same kolumny.
    comptime WEIGHT_TILE = BN * BLOCK_VALUES
    ws = stack_allocation[
        WEIGHT_TILE, Float16, address_space = AddressSpace.SHARED
    ]()
    var threads = Int(block_dim.x)
    var tid = Int(thread_idx.x)

    for block in range(blocks_per_row):
        var slot = tid
        while slot < BN * GROUPS:
            local_row = slot // GROUPS
            group = slot % GROUPS
            var source_row = Int(block_idx.x) * BN + local_row
            if source_row > n_rows - 1:
                source_row = n_rows - 1
            frag = _weight_frag[FMT](
                weights, source_row * blocks_per_row * BB + block * BB, group
            )
            (ws + local_row * BLOCK_VALUES + group * TILE).store(frag)
            slot += threads
        barrier()

        comptime for sub in range(GROUPS):
            var column = block * BLOCK_VALUES + sub * TILE
            var a = InlineArray[SIMD[DType.float16, 16], MTILE](
                fill=SIMD[DType.float16, 16](0.0)
            )
            comptime for mt in range(MTILE):
                a[mt] = (x + x_base[mt] + column).load[width=16, alignment=2]()
            comptime for nt in range(NTILE):
                local_n = (wave % WAVES_N) * NTILE * TILE + nt * TILE + lane % 16
                b = (ws + local_n * BLOCK_VALUES + sub * TILE).load[width=16]()
                comptime for mt in range(MTILE):
                    acc[mt * NTILE + nt] = wmma_f16_16x16x16(
                        a[mt], b, acc[mt * NTILE + nt]
                    )
        barrier()

    comptime for mt in range(MTILE):
        comptime for nt in range(NTILE):
            comptime for i in range(8):
                # (m, n) tego pola: m = i*2 + lane//16, n = lane % 16.
                var m = base_m + mt * TILE + i * 2 + lane // 16
                var n = base_n + nt * TILE + lane % 16
                if m < n_tokens and n < n_rows:
                    y[m * n_rows + n] = Float16(acc[mt * NTILE + nt][i])


comptime gemm_q4_0_wmma_f16_bm32 = gemm_legacy32_wmma_impl[2, 2, 1, 2, 0]
comptime gemm_q4_0_wmma_f16_bm256 = gemm_legacy32_wmma_impl[4, 2, 4, 2, 0]
comptime gemm_q4_1_wmma_f16_bm32 = gemm_legacy32_wmma_impl[2, 2, 1, 2, 1]
comptime gemm_q4_1_wmma_f16_bm256 = gemm_legacy32_wmma_impl[4, 2, 4, 2, 1]
comptime gemm_q5_0_wmma_f16_bm32 = gemm_legacy32_wmma_impl[2, 2, 1, 2, 2]
comptime gemm_q5_0_wmma_f16_bm256 = gemm_legacy32_wmma_impl[4, 2, 4, 2, 2]
comptime gemm_q5_1_wmma_f16_bm32 = gemm_legacy32_wmma_impl[2, 2, 1, 2, 3]
comptime gemm_q5_1_wmma_f16_bm256 = gemm_legacy32_wmma_impl[4, 2, 4, 2, 3]
