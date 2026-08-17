# =============================================================================
# Plik: gemm_q6_k_wmma.mojo
# Opis: Kafelkowany GEMM Q6_K na jednostkach macierzowych RDNA3+ (WMMA), czytający
#       wprost superbloki GGML Q6_K 210 B / 256 wartości. Q6_K był jedynym
#       K-kwantem bez wariantu macierzowego i schodził na `dot4`.
# Przykład: gemm_q6_k_wmma_f16_bm256 obsługuje długi prefill bez repacku.
# =============================================================================
#
# DLACZEGO TEN KERNEL POWSTAŁ. Profil prefillu ThinkingCap-Qwen3.6-27B Q4_K_M na
# Radeonie AI PRO R9700 (`rocprofv3 --kernel-trace`) pokazał, że projekcja `down`
# w Q6_K zjada 25,4% czasu prefillu — 4,66 ms na warstwę wobec 1,43 ms typowej
# projekcji Q4_K o tej samej liczbie mnożeń. Wszystkie pozostałe K-kwanty
# (Q2_K–Q5_K) miały już kafel WMMA, Q6_K nie, więc jako jedyny liczył się na
# `v_dot4_i32_i8`.
#
# UKŁAD SUPERBLOKU Q6_K (zgodny z `_gemv_q6_k_row_acc` w `src/gemv2.mojo`, żeby
# wynik był tą samą matematyką co ścieżka GEMV):
#   bajty   0..127  ql      (młodsze CZTERY bity każdej wartości)
#   bajty 128..191  qh      (starsze DWA bity każdej wartości)
#   bajty 192..207  scales  (16 x int8, jedna skala na 16 wartości)
#   bajty 208..209  d       (f16)
#
# Superblok dzieli się na dwie połowy po 128 kolumn, a każda połowa na cztery
# grupy po 32 kolumny. Dla kolumny `r`:
#   half  = r // 128 ; j = r % 128 ; group = j // 32 ; l = j % 32
#   ql    = bajt `half*64 + (group % 2)*32 + l`, półbajt młodszy dla group < 2
#   qh    = bajt `half*32 + l`, bity `group*2` i `group*2 + 1`
#   scale = `scales[half*8 + group*2 + l // 16]`
#
# Wartość: `(q6 - 32) * (d * sc)`, gdzie `q6` ma sześć bitów.
#
# Szesnaście KOLEJNYCH kolumn (dokładnie bok kafla WMMA) ma to samo `half`,
# `group` i `l // 16`, więc dzieli JEDNĄ skalę i leży w jednym ciągłym odczycie
# szesnastu bajtów `ql` oraz szesnastu `qh`. Dlatego kafel K=16 pokrywa się z
# granicą skalowania i skalę wolno wmnożyć w wagi PRZED mnożeniem macierzowym.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation

from src.arch_wmma import wmma_f16_16x16x16, wmma_acc_row

comptime TILE = 16  # bok kafla WMMA
comptime SUPERBLOCK = 256  # wartości opisane jednym superblokiem
comptime SB_BYTES = 210
comptime QH_OFFSET = 128
comptime SC_OFFSET = 192
comptime D_OFFSET = 208
comptime CHUNK = 64  # kolumny stagowane za jednym razem
comptime GROUPS = CHUNK // TILE  # grupy po 16 kolumn w kafelku stagowania
# Rozsuniecie wierszy kafla wag w LDS, dobrane pomiarem na R9700 (TFLOPS,
# T=1024): patrz komentarz przy `ROW` nizej.
comptime LDS_PAD = 16


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
    var j = first_column % 128
    var group = j // 32
    var l = j % 32

    # `d` to DWA BAJTY — trzeba je reinterpretować, a nie rzutować bajt.
    var d = Float32(
        (weights + superblock + D_OFFSET).bitcast[Float16]().load[
            width=1, alignment=1
        ]()[0]
    )
    var sc = Float32(
        (weights + superblock + SC_OFFSET + half * 8 + group * 2 + l // 16)
        .bitcast[Int8]()
        .load[width=1, alignment=1]()[0]
    )
    var scale = Float16(d * sc)

    var packed = (
        weights + superblock + half * 64 + (group % 2) * 32 + l
    ).load[width=16, alignment=1]()
    var nibbles = packed >> 4 if group >= 2 else packed & 0x0F
    var high_bits = (weights + superblock + QH_OFFSET + half * 32 + l).load[
        width=16, alignment=1
    ]()
    var shift = UInt8(group * 2)
    var q6 = nibbles | (((high_bits >> shift) & 3) << 4)
    # Przesunięcie o 32 ŚCIĄGA SIĘ NA LICZBIE CAŁKOWITEJ. Zapis
    # `q6 * scale - 32 * scale` w f16 kasuje znaczące cyfry dla kodów bliskich
    # 32: oba człony sięgają `63 * scale`, a wynik jest wtedy bliski zeru, więc
    # błąd bezwzględny zostaje na poziomie większego z nich. Odjęcie przed
    # skalowaniem jest dokładne (kod ma sześć bitów) i zostawia jedno
    # zaokrąglenie zamiast trzech.
    return (q6.cast[DType.int16]() - 32).cast[DType.float16]() * scale


def gemm_q6_k_wmma_tile_impl[
    WAVES_M: Int, WAVES_N: Int, MTILE: Int, NTILE: Int, PAD: Int = 0
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    tile_first: Int,
    tile_end: Int,
):
    """GEMM Q6_K na WMMA: Y[t, r] = dot(w[r], x[t]).

    Siatka (ceil(n_rows / BN), ceil(n_tokens / BM)), blok WAVES_M*WAVES_N*32
    wątków, `n_cols % 256 == 0` (niezmiennik superbloku Q6_K). Kontrakt
    argumentów jest identyczny z `gemm_q6_k_f16`, więc launcher wybiera wariant
    wyłącznie po dostępności artefaktu.
    """
    comptime BM = WAVES_M * MTILE * TILE
    comptime BN = WAVES_N * NTILE * TILE

    var lane = Int(thread_idx.x) % 32
    var wave = Int(thread_idx.x) // 32
    var base_m = tile_first + (wave // WAVES_N) * MTILE * TILE
    var base_n = Int(block_idx.x) * BN + (wave % WAVES_N) * NTILE * TILE

    var superblocks = n_cols // SUPERBLOCK

    # Wiersze i tokeny poza zakresem czytają ostatni legalny indeks — ich wyniki
    # i tak nie są zapisywane, a zacisk trzyma odczyty w obrębie buforów.
    var x_base = InlineArray[Int, MTILE](fill=0)
    comptime for mt in range(MTILE):
        var m = base_m + mt * TILE + lane % 16
        if m > tile_end - 1:
            m = tile_end - 1
        x_base[mt] = m * n_cols

    var acc = InlineArray[SIMD[DType.float32, 8], MTILE * NTILE](
        fill=SIMD[DType.float32, 8](0.0)
    )

    # Wagi rozpakowujemy RAZ NA BLOK do LDS — bez tego każda z WAVES_M fal wzdłuż
    # tokenów dekwantyzowałaby dokładnie te same kolumny.
    #
    # `PAD` rozsuwa wiersze kafla wag w LDS. Bez niego szesnaście linii czyta
    # fragment `b` z adresów odległych o `CHUNK * 2 = 128 B`, czyli dokładnie
    # o wielokrotność 32 banków — wszystkie trafiają w ten sam bank. Zmierzone
    # na R9700 (`bench-amd/bench_q6k_wmma_tiles.mojo`, TFLOPS):
    #
    #   rows=5120  cols=17408 T=1024 : pad0 64 | pad8 67 | pad16 67
    #   rows=5120  cols=17408 T=512  : pad0 52 | pad8 57 | pad16 60
    #   rows=10240 cols=5120  T=1024 : pad0 58 | pad8 62 | pad16 62
    comptime ROW = CHUNK + PAD
    comptime WEIGHT_TILE = BN * ROW
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
                (ws + local_row * ROW + group * TILE).store(frag)
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
                    b = (ws + local_n * ROW + sub * TILE).load[width=16, alignment=2]()
                    comptime for mt in range(MTILE):
                        acc[mt * NTILE + nt] = wmma_f16_16x16x16(
                            a[mt], b, acc[mt * NTILE + nt]
                        )
            barrier()

    comptime for mt in range(MTILE):
        comptime for nt in range(NTILE):
            comptime for i in range(8):
                var m = base_m + mt * TILE + wmma_acc_row(lane, i)
                var n = base_n + nt * TILE + lane % 16
                if m < tile_end and n < n_rows:
                    y[m * n_rows + n] = Float16(acc[mt * NTILE + nt][i])


def gemm_q6_k_wmma_impl[
    WAVES_M: Int, WAVES_N: Int, MTILE: Int, NTILE: Int, PAD: Int = 0
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    gemm_q6_k_wmma_tile_impl[WAVES_M, WAVES_N, MTILE, NTILE, PAD](
        y,
        weights,
        x,
        n_cols,
        n_rows,
        Int(block_idx.y) * WAVES_M * MTILE * TILE,
        n_tokens,
    )


def gemm_q6_k_wmma_grouped_impl[
    WAVES_M: Int, WAVES_N: Int, MTILE: Int, NTILE: Int, PAD: Int = 0
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    wtab: UnsafePointer[UnsafePointer[UInt8, MutAnyOrigin], MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    tile_expert: UnsafePointer[Int32, MutAnyOrigin],
    tile_first: UnsafePointer[Int32, MutAnyOrigin],
    tile_end: UnsafePointer[Int32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    var tile = Int(block_idx.y)
    gemm_q6_k_wmma_tile_impl[WAVES_M, WAVES_N, MTILE, NTILE, PAD](
        y,
        wtab[Int(tile_expert[tile])],
        x,
        n_cols,
        n_rows,
        Int(tile_first[tile]),
        Int(tile_end[tile]),
    )


# Kafle dobrane pomiarem na R9700 (`bench-amd/bench_q6k_wmma_tiles.mojo`,
# TFLOPS):
#
#   ksztalt                      BM256/BN64  BM256/BN128  BM512/BN128
#   5120x17408  T=1024                   68           77           76
#   5120x17408  T=512                    55           58           59
#   10240x5120  T=1024                   60           74           92
comptime gemm_q6_k_wmma_f16_bm32 = gemm_q6_k_wmma_impl[2, 2, 1, 2, LDS_PAD]
comptime gemm_q6_k_wmma_f16_bm256 = gemm_q6_k_wmma_impl[4, 2, 4, 2, LDS_PAD]
comptime gemm_q6_k_wmma_f16_bm256_bn128 = gemm_q6_k_wmma_impl[4, 2, 4, 4, LDS_PAD]
comptime gemm_q6_k_wmma_f16_bm512_bn128 = gemm_q6_k_wmma_impl[8, 2, 4, 4, LDS_PAD]
comptime gemm_q6_k_wmma_f16_grouped = gemm_q6_k_wmma_grouped_impl[4, 2, 1, 2, LDS_PAD]
comptime gemm_q6_k_wmma_f16_grouped_bm128_bn64 = gemm_q6_k_wmma_grouped_impl[4, 2, 2, 2, LDS_PAD]
