# =============================================================================
# Plik: gemm_q4_k_wmma.mojo
# Opis: Kafelkowany GEMM Q4_K na jednostkach macierzowych RDNA3 (WMMA), czytający
#       wprost superbloki GGML Q4_K 144 B / 256 wartości. Odpowiednik
#       `gemm_q4_k_i8mma` dla kart bez instrukcji `mma`/`ldmatrix`.
# Przykład: gemm_q4_k_wmma_f16_bm256 obsługuje długi prefill bez repacku.
# =============================================================================
#
# DLACZEGO TEN KERNEL W OGÓLE POWSTAŁ. Wszystkie ciała `gemm_*_impl` w
# `src/gemm.mojo` wołają `mma()` z fragmentem NVIDIA `m16n8k16`. RDNA3 ma
# fragment `16x16x16` — inny podział danych na linię — więc na AMD te kernele nie
# kompilują się w ogóle i Q4_K schodził tam na `dot4`, zostawiając jednostkę
# macierzową bezczynną. Pomiar: llama.cpp robił prefill Q4_K na 7900 XT 1,6x
# szybciej od nas właśnie dlatego.
#
# UKŁAD SUPERBLOKU Q4_K (zgodny z `_dequant_q4k`, żeby wynik był tą samą
# matematyką co ścieżka `dot4`):
#   bajty  0..1   d      (f16)
#   bajty  2..3   dmin   (f16)
#   bajty  4..15  scales (12 B, 6-bitowe pary sc/min dla ośmiu podbloków po 32)
#   bajty 16..143 qs     (128 B, po pół bajtu na wartość)
# Kolumna `r` leży w bajcie `16 + (r // 64) * 32 + (r % 32)`, w młodszym
# półbajcie dla `r % 64 < 32`, w starszym powyżej. Skala podbloku to `r // 32`.
#
# Wartość: `nibble * (d * sc) - (dmin * mn)`.
#
# Szesnaście KOLEJNYCH kolumn (dokładnie bok kafla WMMA) leży zawsze w jednym
# ciągłym odczycie 16 bajtów i ma JEDNĄ parę skal — dlatego kafel K=16 pokrywa
# się tu równo z granicą skalowania i skalę wolno wmnożyć w wagi PRZED mnożeniem
# macierzowym, zamiast rozbijać akumulację.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation

from src.arch_wmma import wmma_f16_16x16x16, wmma_acc_row
from src.gemv2 import _q4k_scale_min

comptime TILE = 16  # bok kafla WMMA
comptime SUPERBLOCK = 256  # wartości opisane jednym superblokiem
comptime SB_BYTES = 144  # 4 B d/dmin + 12 B skal + 128 B kodów
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
    var chunk = first_column // CHUNK
    var high = ((first_column % CHUNK) // TILE) >= 2
    var header = (weights + superblock).load[width=16, alignment=1]()
    # d i dmin to DWA BAJTY każdy — trzeba je reinterpretować, a nie rzutować
    # pojedynczy bajt nagłówka na f16.
    var d = Float32(
        (weights + superblock).bitcast[Float16]().load[width=1, alignment=1]()[0]
    )
    var dmin = Float32(
        (weights + superblock + 2).bitcast[Float16]().load[width=1, alignment=1]()[0]
    )
    sc, mn = _q4k_scale_min(header, first_column // 32)
    var scale = Float16(d * sc)
    var offset = Float16(dmin * mn)

    var packed = (
        weights + superblock + 16 + chunk * 32 + (first_column % 32)
    ).load[width=16, alignment=1]()
    var nibbles = packed >> 4 if high else packed & 0x0F
    return nibbles.cast[DType.float16]() * scale - offset


def gemm_q4_k_wmma_impl[
    WAVES_M: Int, WAVES_N: Int, MTILE: Int, NTILE: Int, PAD: Int = 0
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """GEMM Q4_K na WMMA: Y[t, r] = dot(w[r], x[t]).

    Siatka (ceil(n_rows / BN), ceil(n_tokens / BM)), blok WAVES_M*WAVES_N*32
    wątków, `n_cols % 256 == 0` (niezmiennik superbloku Q4_K). Kontrakt
    argumentów jest identyczny z `gemm_q4_k_i8mma`, więc launcher wybiera wariant
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
    #
    # `PAD` rozsuwa wiersze kafla wag. Bez niego szesnaście linii czyta fragment
    # `b` z adresów odległych o `CHUNK * 2 = 128 B`, czyli o wielokrotność 32
    # banków LDS — wszystkie trafiają w ten sam bank. Zmierzone na R9700
    # (`bench-amd/bench_q4k_wmma_tiles.mojo`, TFLOPS, T=1024):
    #
    #   rows=17408 cols=5120 : pad0 68 | pad8 69 | pad16 70 | pad24 70
    #   rows=6144  cols=5120 : pad0 65 | pad8 71 | pad16 73 | pad24 72
    #   rows=5120  cols=6144 : pad0 49 | pad8 65 | pad16 65 | pad24 66
    comptime ROW = CHUNK + PAD
    comptime WEIGHT_TILE = BN * ROW
    ws = stack_allocation[
        2 * WEIGHT_TILE, Float16, address_space = AddressSpace.SHARED
    ]()
    var threads = Int(block_dim.x)
    var tid = Int(thread_idx.x)

    # Podwójne buforowanie: kafel `slot` liczymy, a `slot ^ 1` w tym czasie się
    # zapełnia. Bez tego każda iteracja miała barierę po stronie stronicowania i
    # po stronie liczenia, więc jedno nie nakładało się na drugie.
    var chunks = superblocks * (SUPERBLOCK // CHUNK)

    var stage_superblock_0 = (0) // (SUPERBLOCK // CHUNK)
    var stage_chunk_0 = (0) % (SUPERBLOCK // CHUNK)
    var stage_base_0 = (0) * WEIGHT_TILE
    var pos_0 = tid
    while pos_0 < BN * GROUPS:
        local_row = pos_0 // GROUPS
        group = pos_0 % GROUPS
        var source_row = Int(block_idx.x) * BN + local_row
        if source_row > n_rows - 1:
            source_row = n_rows - 1
        frag = _weight_frag(
            weights,
            source_row * superblocks * SB_BYTES + stage_superblock_0 * SB_BYTES,
            stage_chunk_0 * GROUPS + group,
        )
        (ws + stage_base_0 + local_row * ROW + group * TILE).store(frag)
        pos_0 += threads

    barrier()

    for chunk_index in range(chunks):
        var slot = chunk_index % 2
        var compute_base = slot * WEIGHT_TILE
        var column_base = chunk_index * CHUNK

        if chunk_index + 1 < chunks:
            var stage_superblock_next = (chunk_index + 1) // (SUPERBLOCK // CHUNK)
            var stage_chunk_next = (chunk_index + 1) % (SUPERBLOCK // CHUNK)
            var stage_base_next = (slot ^ 1) * WEIGHT_TILE
            var pos_next = tid
            while pos_next < BN * GROUPS:
                local_row = pos_next // GROUPS
                group = pos_next % GROUPS
                var source_row = Int(block_idx.x) * BN + local_row
                if source_row > n_rows - 1:
                    source_row = n_rows - 1
                frag = _weight_frag(
                    weights,
                    source_row * superblocks * SB_BYTES + stage_superblock_next * SB_BYTES,
                    stage_chunk_next * GROUPS + group,
                )
                (ws + stage_base_next + local_row * ROW + group * TILE).store(frag)
                pos_next += threads

        comptime for sub in range(GROUPS):
            var column = column_base + sub * TILE
            var a = InlineArray[SIMD[DType.float16, 16], MTILE](
                fill=SIMD[DType.float16, 16](0.0)
            )
            comptime for mt in range(MTILE):
                a[mt] = (x + x_base[mt] + column).load[width=16, alignment=2]()
            comptime for nt in range(NTILE):
                local_n = (wave % WAVES_N) * NTILE * TILE + nt * TILE + lane % 16
                b = (ws + compute_base + local_n * ROW + sub * TILE).load[width=16, alignment=2]()
                comptime for mt in range(MTILE):
                    acc[mt * NTILE + nt] = wmma_f16_16x16x16(
                        a[mt], b, acc[mt * NTILE + nt]
                    )
        barrier()

    comptime for mt in range(MTILE):
        comptime for nt in range(NTILE):
            comptime for i in range(8):
                # (m, n) tego pola: m = i*2 + lane//16, n = lane % 16.
                var m = base_m + mt * TILE + wmma_acc_row(lane, i)
                var n = base_n + nt * TILE + lane % 16
                if m < n_tokens and n < n_rows:
                    y[m * n_rows + n] = Float16(acc[mt * NTILE + nt][i])


# Kafle dobrane pomiarem na R9700 (`bench-amd/bench_q4k_wmma_tiles.mojo`,
# TFLOPS, T=1024). BN=64 przegrywa wszedzie: aktywacje sa wtedy czytane
# `n_rows / 64` razy zamiast `n_rows / 128`.
#
#   ksztalt              BM256/BN64  BM256/BN128  BM512/BN128
#   17408x5120                   70           82           93
#   6144x5120                    68           89          100
#   5120x6144                    50           87           85
#
# BM512 potrzebuje T >= 512, zeby miec czym wypelnic kafel — wybor nalezy do
# launchera, ktory zna dlugosc chunka.
comptime gemm_q4_k_wmma_f16_bm32 = gemm_q4_k_wmma_impl[2, 2, 1, 2, LDS_PAD]
comptime gemm_q4_k_wmma_f16_bm256 = gemm_q4_k_wmma_impl[4, 2, 4, 2, LDS_PAD]
comptime gemm_q4_k_wmma_f16_bm256_bn128 = gemm_q4_k_wmma_impl[4, 2, 4, 4, LDS_PAD]
comptime gemm_q4_k_wmma_f16_bm512_bn128 = gemm_q4_k_wmma_impl[8, 2, 4, 4, LDS_PAD]


# ---------------------------------------------------------------------------
# Wariant PRZENOŚNY: ta sama dekwantyzacja, ale mnożenie w rejestrach zamiast na
# jednostce macierzowej. Powstał dla RDNA2 (gfx1030), która WMMA nie ma — bez
# niego ten format nie uruchamiał się tam w ogóle.
#
# Układ: kafel BM x BN liczony przez BM/4 * BN/4 wątków, każdy trzyma 4x4 wyniki
# w rejestrach. Aktywacje i rozpakowane wagi idą przez LDS, więc każdą wartość
# czyta się z pamięci globalnej raz na blok.


def gemm_q4_k_tile_impl[BM: Int, BN: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """GEMM Q5_K bez jednostki macierzowej. Siatka
    (ceil(n_rows / BN), ceil(n_tokens / BM)), blok (BM/4)*(BN/4) wątków."""
    comptime TM = 4
    comptime TN = 4
    comptime THREADS = (BM // TM) * (BN // TN)

    var tid = Int(thread_idx.x)
    var thread_m = (tid // (BN // TN)) * TM
    var thread_n = (tid % (BN // TN)) * TN
    var base_m = Int(block_idx.y) * BM
    var base_n = Int(block_idx.x) * BN
    var superblocks = n_cols // SUPERBLOCK

    xs = stack_allocation[BM * CHUNK, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[BN * CHUNK, Float16, address_space = AddressSpace.SHARED]()

    var acc = InlineArray[SIMD[DType.float32, TN], TM](
        fill=SIMD[DType.float32, TN](0.0)
    )

    for superblock in range(superblocks):
        for chunk in range(SUPERBLOCK // CHUNK):
            var column0 = superblock * SUPERBLOCK + chunk * CHUNK
            # Aktywacje: szesnaście kolumn na wątek, aż kafel BM x CHUNK pełny.
            var slot = tid
            while slot < BM * (CHUNK // TILE):
                token = slot // (CHUNK // TILE)
                part = slot % (CHUNK // TILE)
                var m = base_m + token
                if m > n_tokens - 1:
                    m = n_tokens - 1
                (xs + token * CHUNK + part * TILE).store(
                    (x + m * n_cols + column0 + part * TILE).load[
                        width=TILE, alignment=2
                    ]()
                )
                slot += THREADS
            # Wagi: jedna grupa szesnastu kolumn na wątek.
            slot = tid
            while slot < BN * GROUPS:
                local_row = slot // GROUPS
                group = slot % GROUPS
                var source_row = base_n + local_row
                if source_row > n_rows - 1:
                    source_row = n_rows - 1
                (ws + local_row * CHUNK + group * TILE).store(
                    _weight_frag(
                        weights,
                        source_row * superblocks * SB_BYTES + superblock * SB_BYTES,
                        chunk * GROUPS + group,
                    )
                )
                slot += THREADS
            barrier()

            for c in range(CHUNK):
                var b = SIMD[DType.float32, TN](0.0)
                comptime for n in range(TN):
                    b[n] = Float32(ws[(thread_n + n) * CHUNK + c])
                comptime for m in range(TM):
                    acc[m] += Float32(xs[(thread_m + m) * CHUNK + c]) * b
            barrier()

    comptime for m in range(TM):
        comptime for n in range(TN):
            var row = base_m + thread_m + m
            var col = base_n + thread_n + n
            if row < n_tokens and col < n_rows:
                y[row * n_rows + col] = Float16(acc[m][n])


comptime gemm_q4_k_tile_f16_bm32 = gemm_q4_k_tile_impl[32, 64]
