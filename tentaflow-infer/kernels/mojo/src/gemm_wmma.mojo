# =============================================================================
# Plik: gemm_wmma.mojo
# Opis: Prefillowy GEMM Q8_0 na jednostkach macierzowych RDNA3 (WMMA 16x16x16).
#       Y[T, rows] = X[T, cols] · W^T, akumulacja int32 w obrębie bloku
#       kwantyzacji i skalowanie do f32 — ta sama matematyka co `gemm_q8_0_dot4`.
# Przykład: gemm_q8_0_wmma_64x128 = gemm_q8_0_wmma_impl[4, 2, 4, DType.float16]
# =============================================================================
#
# DLACZEGO OSOBNY KERNEL, A NIE WARIANT `gemm_dot`: tamten rozkład daje każdemu
# wątkowi własny kafel TM x TN w rejestrach, bo bez jednostki macierzowej to
# jedyny układ, w którym wątek ma dane na swój wynik. WMMA odwraca ten kontrakt —
# fragment jest własnością CAŁEJ fali, a wynik wraca rozrzucony po liniach.
#
# UKŁAD FRAGMENTÓW RDNA3 w trybie wave32 (zmierzony wyczerpująco na karcie,
# 256 pól, `bench-amd/bench_wmma_gfx11.mojo` pilnuje regresji):
#   A: linia L niesie CAŁY wiersz m = L % 16 po K=16 bajtów; linie 16-31
#      powielają wiersze 0-15.
#   B: linia L niesie całą kolumnę n = L % 16 po K=16 bajtów.
#   C/D: element (m, n) leży w linii 16*(m % 2) + n pod indeksem m // 2.
# Ten układ jest powodem, dla którego A i B czytamy WPROST z pamięci globalnej:
# linia potrzebuje szesnastu KOLEJNYCH bajtów swojego wiersza, a oba źródła
# (kody aktywacji [T, K] i payload bloku Q8_0) już tak leżą. LDS nic by tu nie
# dołożył poza barierą.
#
# Skale zmieniają się co 32 kolumny, więc jeden blok kwantyzacji to DOKŁADNIE
# dwa wywołania WMMA (K=16 + K=16) z akumulacją int32, a dopiero ich suma jest
# skalowana i dodawana do akumulatora f32. Identycznie jak w MMQ i w wariancie
# dot4, więc wyniki obu ścieżek są porównywalne co do zaokrągleń.

from std.sys.info import _accelerator_arch
from std.gpu import block_idx, thread_idx
from std.memory import bitcast

from src.arch_wmma import wmma_i8_16x16x16, wmma_acc_row

comptime TILE = 16  # bok kafla WMMA
comptime QK = 32  # kolumny w bloku kwantyzacji Q8_0
comptime QBYTES = 34  # 2 bajty skali f16 + 32 kody int8


@always_inline
def _row_frag_i8(
    src: UnsafePointer[Int8, MutAnyOrigin], offset: Int
) -> SIMD[DType.int32, 4]:
    """Fragment WMMA jednej linii, w rozmiarze, jakiego zada karta.

    RDNA3 wymaga CALEGO wiersza szesnastu bajtow na linie i dubluje go miedzy
    polowami fali. RDNA4 dublowanie usunelo: linia niesie osiem bajtow, te ktore
    wskazuje numer polowy fali. Czytanie tam pelnych szesnastu i odrzucanie
    polowy kosztowalo ZMIERZONE 13% prefillu Q8_0 (2047 -> 1777 tok/s), wiec
    rozmiar odczytu idzie za architektura, a nie za najszerszym wariantem.

    Payload bloku Q8_0 zaczyna sie dwa bajty za poczatkiem bloku, wiec odczyt
    jest NIEWYROWNANY — stad jawne `alignment=1`.
    """
    comptime if _accelerator_arch().startswith("amdgpu:gfx12"):
        var half = Int(thread_idx.x) % 32 // 16
        var raw = (src + offset + half * 8).load[width=8, alignment=1]()
        var lo = bitcast[DType.int32, 2](raw)
        return SIMD[DType.int32, 4](lo[0], lo[1], 0, 0)
    else:
        var raw = (src + offset).load[width=16, alignment=1]()
        return bitcast[DType.int32, 4](raw)


@always_inline
def _wmma_q8_tile[
    WAVES_M: Int, WAVES_N: Int, MTILE: Int, NTILE: Int, OUT: DType
](
    y: UnsafePointer[Scalar[OUT], MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    tile_end: Int,
    activation_tokens: Int,
    row0: Int,
    token0: Int,
):
    """Kafel bloku o początku (token0, row0). Wydzielony, bo ten sam kafel liczy
    zarówno pojedynczy GEMM, jak i trójkę projekcji DeltaNet w jednym gridzie."""
    var lane = Int(thread_idx.x) % 32
    var wave = Int(thread_idx.x) // 32
    var base_m = token0 + (wave // WAVES_N) * MTILE * TILE
    var base_n = row0 + (wave % WAVES_N) * NTILE * TILE

    var blocks = n_cols // QK
    var wq = w.bitcast[Int8]()

    # Wiersze i tokeny poza zakresem czytają ostatni legalny indeks — ich wyniki
    # i tak nie są zapisywane, a zacisk trzyma odczyty w obrębie buforów.
    var x_base = InlineArray[Int, MTILE](fill=0)
    comptime for mt in range(MTILE):
        var m = base_m + mt * TILE + lane % 16
        if m > tile_end - 1:
            m = tile_end - 1
        x_base[mt] = m * n_cols
    var w_base = InlineArray[Int, NTILE](fill=0)
    comptime for nt in range(NTILE):
        var n = base_n + nt * TILE + lane % 16
        if n > n_rows - 1:
            n = n_rows - 1
        w_base[nt] = n * blocks * QBYTES

    var acc = InlineArray[SIMD[DType.float32, 8], MTILE * NTILE](
        fill=SIMD[DType.float32, 8](0.0)
    )

    for b in range(blocks):
        var a_lo = InlineArray[SIMD[DType.int32, 4], MTILE](fill=SIMD[DType.int32, 4](0))
        var a_hi = InlineArray[SIMD[DType.int32, 4], MTILE](fill=SIMD[DType.int32, 4](0))
        comptime for mt in range(MTILE):
            a_lo[mt] = _row_frag_i8(xq, x_base[mt] + b * QK)
            a_hi[mt] = _row_frag_i8(xq, x_base[mt] + b * QK + TILE)

        comptime for nt in range(NTILE):
            var wb = w_base[nt] + b * QBYTES
            var b_lo = _row_frag_i8(wq, wb + 2)
            var b_hi = _row_frag_i8(wq, wb + 2 + TILE)
            # Skala wagi zależy tylko od `n`, czyli od linii — jedna na kafel N.
            var dw = Float32(
                (w + wb).bitcast[Float16]().load[width=1, alignment=1]()[0]
            )
            comptime for mt in range(MTILE):
                var block_acc = SIMD[DType.int32, 8](0)
                block_acc = wmma_i8_16x16x16[preselected=True](a_lo[mt], b_lo, block_acc)
                block_acc = wmma_i8_16x16x16[preselected=True](a_hi[mt], b_hi, block_acc)
                comptime for i in range(8):
                    # (m, n) tego pola: m = i*2 + lane//16, n = lane % 16.
                    var m = base_m + mt * TILE + wmma_acc_row(lane, i)
                    if m > tile_end - 1:
                        m = tile_end - 1
                    acc[mt * NTILE + nt][i] += (
                        Float32(block_acc[i]) * dw * xd[b * activation_tokens + m]
                    )

    comptime for mt in range(MTILE):
        comptime for nt in range(NTILE):
            comptime for i in range(8):
                var m = base_m + mt * TILE + wmma_acc_row(lane, i)
                var n = base_n + nt * TILE + lane % 16
                if m < tile_end and n < n_rows:
                    y[m * n_rows + n] = acc[mt * NTILE + nt][i].cast[OUT]()


def gemm_q8_0_wmma_impl[WAVES_M: Int, WAVES_N: Int, MTILE: Int, NTILE: Int, OUT: DType](
    y: UnsafePointer[Scalar[OUT], MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    xsm: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Q8_0 GEMM na WMMA: Y[t, r] = dot(w[r], x[t]).

    Aktywacja jest wcześniej skwantowana przez `quantize_act_q8_1` (kody int8
    `xq` w układzie [T, K], skale `xd` blok-major [K/32, T]); `xsm` jest w
    sygnaturze dla zgodności z rodziną i8mma i nie jest tu potrzebne, bo Q8_0
    jest symetryczne. Wagi czytamy WPROST z bajtów GGUF, bez przepakowania.

    Fala liczy MTILE x NTILE kafli 16x16, więc jeden odczyt fragmentu A służy
    NTILE kaflom, a jeden fragment B — MTILE kaflom. To jest jedyna dźwignia na
    ruch globalny w tym kernelu: kafel 16x64 zmierzył się GORZEJ od kafla dot4
    128x128 przy dużym T właśnie dlatego, że jego FLOP/bajt (25,6) był 2,5x
    niższy. Kafel bloku (WAVES_M*MTILE*16) x (WAVES_N*NTILE*16) podnosi to
    z powrotem ponad wariant dot4.

    Siatka (ceil(rows/BN), ceil(T/BM)), blok WAVES_M*WAVES_N*32 wątków,
    `n_cols % 32 == 0` (niezmiennik formatu Q8_0).

    Dwa kafle produkcyjne, bo ich zakresy się nie pokrywają (zmierzone A/B wobec
    `gemm_q8_0_dot4_128x128` na 7900 XT, 4096x4096): przy krótkim prompcie duży
    kafel nie ma czym wypełnić fal i przegrywa (T=128: 10 wobec 19 TOPS), przy
    długim wygrywa reużyciem danych (T=1024: 43 wobec 32 TOPS).
    """
    comptime BM = WAVES_M * MTILE * TILE
    comptime BN = WAVES_N * NTILE * TILE
    _wmma_q8_tile[WAVES_M, WAVES_N, MTILE, NTILE, OUT](
        y, w, xq, xd, n_cols, n_rows, n_tokens, n_tokens,
        Int(block_idx.x) * BN, Int(block_idx.y) * BM,
    )


def gemm_q8_0_wmma_grouped_impl[
    WAVES_M: Int, WAVES_N: Int, MTILE: Int, NTILE: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    wtab: UnsafePointer[UnsafePointer[UInt8, MutAnyOrigin], MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    xsm: UnsafePointer[Float32, MutAnyOrigin],
    tile_expert: UnsafePointer[Int32, MutAnyOrigin],
    tile_first: UnsafePointer[Int32, MutAnyOrigin],
    tile_end: UnsafePointer[Int32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Jeden grid WMMA Q8_0 dla wszystkich ekspertów routowanego kroku."""
    _ = xsm
    var tile = Int(block_idx.y)
    _wmma_q8_tile[WAVES_M, WAVES_N, MTILE, NTILE, DType.float16](
        y,
        wtab[Int(tile_expert[tile])],
        xq,
        xd,
        n_cols,
        n_rows,
        Int(tile_end[tile]),
        n_tokens,
        Int(block_idx.x) * WAVES_N * NTILE * TILE,
        Int(tile_first[tile]),
    )


def gemm_q8_0_wmma_triplet_bm64(
    y0: UnsafePointer[Float16, MutAnyOrigin],
    w0: UnsafePointer[UInt8, MutAnyOrigin],
    n_rows0: Int,
    y1: UnsafePointer[Float16, MutAnyOrigin],
    w1: UnsafePointer[UInt8, MutAnyOrigin],
    n_rows1: Int,
    y2: UnsafePointer[Float16, MutAnyOrigin],
    w2: UnsafePointer[UInt8, MutAnyOrigin],
    n_rows2: Int,
    xq_g: UnsafePointer[Int8, MutAnyOrigin],
    xd_g: UnsafePointer[Float32, MutAnyOrigin],
    xsm_g: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_tokens: Int,
):
    """Trzy projekcje DeltaNet w JEDNYM gridzie na wspólnej aktywacji Q8_1.

    Odpowiednik `gemm_q8_0_i8mma_triplet_bm64` na WMMA: `block_idx.x` wybiera,
    która z trzech macierzy wag jest liczona, więc wagi i wyjścia zostają
    rozdzielone, a aktywacja jest kwantowana raz na warstwę.
    """
    comptime BM = 64
    comptime BN = 64
    tile = Int(block_idx.x)
    blocks0 = (n_rows0 + BN - 1) // BN
    blocks1 = (n_rows1 + BN - 1) // BN
    t0 = Int(block_idx.y) * BM
    if tile < blocks0:
        _wmma_q8_tile[2, 2, 2, 2, DType.float16](
            y0, w0, xq_g, xd_g, n_cols, n_rows0, n_tokens, tile * BN, t0
        )
    elif tile < blocks0 + blocks1:
        _wmma_q8_tile[2, 2, 2, 2, DType.float16](
            y1, w1, xq_g, xd_g, n_cols, n_rows1, n_tokens,
            (tile - blocks0) * BN, t0,
        )
    else:
        _wmma_q8_tile[2, 2, 2, 2, DType.float16](
            y2, w2, xq_g, xd_g, n_cols, n_rows2, n_tokens,
            (tile - blocks0 - blocks1) * BN, t0,
        )


comptime gemm_q8_0_wmma_64x128 = gemm_q8_0_wmma_impl[2, 2, 2, 4, DType.float16]
comptime gemm_q8_0_wmma_out_f32_64x128 = gemm_q8_0_wmma_impl[2, 2, 2, 4, DType.float32]
# BM=128: kafel dla WYSOKICH macierzy (gate/up, rows=11264). Ruch wag to
# `(T/BM) * rows * cols` bajtow, wiec podwojenie BM POLOWI ponowne czytanie wag —
# przy 49 MB wag gate/up to ono, a nie moc obliczeniowa, decydowalo o przegranej
# kafla BM=64 z `dot4` (27 wobec 37 TOPS na R9700).
comptime gemm_q8_0_wmma_128x128 = gemm_q8_0_wmma_impl[4, 2, 2, 4, DType.float16]
comptime gemm_q8_0_wmma_16x64 = gemm_q8_0_wmma_impl[1, 4, 1, 1, DType.float16]
comptime gemm_q8_0_wmma_out_f32_16x64 = gemm_q8_0_wmma_impl[1, 4, 1, 1, DType.float32]
comptime gemm_q8_0_wmma_f16_grouped = gemm_q8_0_wmma_grouped_impl[2, 2, 2, 2]
comptime gemm_q8_0_wmma_f16_grouped_bm128_bn64 = gemm_q8_0_wmma_grouped_impl[4, 2, 2, 2]
