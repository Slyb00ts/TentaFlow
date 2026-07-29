# =============================================================================
# Plik: gemm_q3_k_wmma.mojo
# Opis: Kafelkowany GEMM Q3_K na jednostkach macierzowych RDNA3 (WMMA), czytający
#       wprost superbloki GGML Q3_K 110 B / 256 wartości. Bez niego Q3_K nie
#       uruchamiał się na AMD w ogóle.
# Przykład: gemm_q3_k_wmma_f16_bm256 obsługuje długi prefill bez repacku.
# =============================================================================
#
# DLACZEGO TEN KERNEL W OGÓLE POWSTAŁ. Wszystkie ciała `gemm_*_impl` w
# `src/gemm.mojo` wołają `mma()` z fragmentem NVIDIA `m16n8k16`. RDNA3 ma
# fragment `16x16x16` — inny podział danych na linię — więc na AMD te kernele nie
# kompilują się w ogóle i Q4_K schodził tam na `dot4`, zostawiając jednostkę
# macierzową bezczynną. Pomiar: llama.cpp robił prefill Q4_K na 7900 XT 1,6x
# szybciej od nas właśnie dlatego.
#
# UKŁAD SUPERBLOKU Q3_K (zgodny z `gemm_q3_k_impl`):
#   bajty   0..31  hmask  (TRZECI bit każdej wartości)
#   bajty  32..95  qs     (2 młodsze bity, cztery wartości w bajcie)
#   bajty  96..107 scales (12 B, 6-bitowe skale szesnastu podbloków, przesunięte
#                  o 32 — rozpakowuje je `_q3k_scales8`)
#   bajty 108..109 d      (f16)
# Dla kolumny `r`: `n = r // 128`, `sh = (r % 128) // 32`, bajt kodu to
# `32 + n * 32 + (r % 32)`, bit maski to `4 * n + sh` w bajcie `r % 32`.
#
# Wartość: `(q + 4 * hb - 4) * (d * sc)`, gdzie `q` ma dwa bity, a `hb` jest
# trzecim bitem. Odejmowanie czwórki daje zakres ze znakiem, więc arytmetyka
# musi iść po typie ze znakiem.
#
# Szesnaście KOLEJNYCH kolumn (dokładnie bok kafla WMMA) leży zawsze w jednym
# ciągłym odczycie 16 bajtów i ma JEDNĄ parę skal — dlatego kafel K=16 pokrywa
# się tu równo z granicą skalowania i skalę wolno wmnożyć w wagi PRZED mnożeniem
# macierzowym, zamiast rozbijać akumulację.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation

from src.arch_wmma import wmma_f16_16x16x16
from src.gemv2 import _q3k_scales8

comptime TILE = 16  # bok kafla WMMA
comptime SUPERBLOCK = 256  # wartości opisane jednym superblokiem
comptime SB_BYTES = 110
comptime CHUNK = 64  # kolumny stagowane za jednym razem
comptime GROUPS = CHUNK // TILE  # grupy po 16 kolumn w kafelku stagowania


@always_inline
def _weight_frag(
    weights: UnsafePointer[UInt8, MutAnyOrigin], superblock: Int, group16: Int
) -> SIMD[DType.float16, 16]:
    """Szesnaście wag jednego wiersza dla grupy `group16` (0..15), przeskalowanych.

    `group16` numeruje szesnastokolumnowe grupy WEWNĄTRZ superbloku, czyli
    pierwsza kolumna grupy to `group16 * 16`.
    """
    var first_column = group16 * TILE
    var half = first_column // 128
    var shift = UInt8((first_column % 128) // 32)
    var d = Float32(
        (weights + superblock + 108).bitcast[Float16]().load[width=1, alignment=1]()[0]
    )
    var scales = _q3k_scales8(weights, superblock, half)
    var scale = Float16(d * scales[2 * Int(shift) + (first_column % 32) // 16])

    var packed = (
        weights + superblock + 32 + half * 32 + (first_column % 32)
    ).load[width=16, alignment=1]()
    var mask = (weights + superblock + (first_column % 32)).load[width=16, alignment=1]()
    var q = ((packed >> shift * 2) & 3).cast[DType.int16]()
    var high = ((mask >> UInt8(4 * half + Int(shift))) & 1).cast[DType.int16]()
    return (q + high * 4 - 4).cast[DType.float16]() * scale

def gemm_q3_k_wmma_impl[
    WAVES_M: Int, WAVES_N: Int, MTILE: Int, NTILE: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """GEMM Q3_K na WMMA: Y[t, r] = dot(w[r], x[t]).

    Siatka (ceil(n_rows / BN), ceil(n_tokens / BM)), blok WAVES_M*WAVES_N*32
    wątków, `n_cols % 256 == 0` (niezmiennik superbloku Q3_K). Kontrakt
    argumentów jest identyczny z `gemm_q3_k_f16`, więc launcher wybiera wariant
    wyłącznie po architekturze.
    """
    comptime BM = WAVES_M * MTILE * TILE
    comptime BN = WAVES_N * NTILE * TILE

    var lane = Int(thread_idx.x) % 32
    var wave = Int(thread_idx.x) // 32
    var base_m = Int(block_idx.y) * BM + (wave // WAVES_N) * MTILE * TILE
    var base_n = Int(block_idx.x) * BN + (wave % WAVES_N) * NTILE * TILE

    var superblocks = n_cols // SUPERBLOCK

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

    # Wagi rozpakowujemy RAZ NA BLOK do LDS — bez tego każda z WAVES_M fal wzdłuż
    # tokenów dekwantyzowałaby dokładnie te same kolumny. Kafel BN x 64 wartości
    # to 8 KiB dla BN=64.
    comptime WEIGHT_TILE = BN * CHUNK
    ws = stack_allocation[
        WEIGHT_TILE, Float16, address_space = AddressSpace.SHARED
    ]()
    var threads = Int(block_dim.x)
    var tid = Int(thread_idx.x)

    for superblock in range(superblocks):
        for chunk in range(SUPERBLOCK // CHUNK):
            var slot = tid
            while slot < BN * GROUPS:
                local_row = slot // GROUPS
                group = slot % GROUPS
                var source_row = Int(block_idx.x) * BN + local_row
                if source_row > n_rows - 1:
                    source_row = n_rows - 1
                frag = _weight_frag(
                    weights,
                    source_row * superblocks * SB_BYTES + superblock * SB_BYTES,
                    chunk * GROUPS + group,
                )
                (ws + local_row * CHUNK + group * TILE).store(frag)
                slot += threads
            barrier()

            comptime for sub in range(GROUPS):
                var column = superblock * SUPERBLOCK + chunk * CHUNK + sub * TILE
                var a = InlineArray[SIMD[DType.float16, 16], MTILE](
                    fill=SIMD[DType.float16, 16](0.0)
                )
                comptime for mt in range(MTILE):
                    a[mt] = (x + x_base[mt] + column).load[width=16, alignment=2]()
                comptime for nt in range(NTILE):
                    local_n = (
                        (wave % WAVES_N) * NTILE * TILE + nt * TILE + lane % 16
                    )
                    b = (ws + local_n * CHUNK + sub * TILE).load[width=16]()
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


# Kafle dobrane tak samo jak w rodzinie NVFP4 WMMA: duży dla długiego prefillu,
# mały dla krótkich chunków, gdzie duży nie ma czym wypełnić fal.
comptime gemm_q3_k_wmma_f16_bm32 = gemm_q3_k_wmma_impl[2, 2, 1, 2]
comptime gemm_q3_k_wmma_f16_bm256 = gemm_q3_k_wmma_impl[4, 2, 4, 2]
