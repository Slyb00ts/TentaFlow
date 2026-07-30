# =============================================================================
# Plik: gemm_fp8_wmma.mojo
# Opis: GEMM FP8 (e4m3) na jednostce macierzowej RDNA4. Kontrakt argumentow jest
#       ten sam co `gemm_fp8_impl` NVIDII, wiec launcher wybiera wariant
#       wylacznie po architekturze.
# Przyklad: gemm_fp8_wmma_bm512_bn128 obsluguje dlugi prefill.
# =============================================================================
#
# DLACZEGO TEN KERNEL. Kafel f16 stoi na ~97 TFLOPS ze 179 i NIE DA SIE go
# poprawic tiling'iem: przy 16 akumulatorach (128 VGPR) i fragmencie f16
# zajmujacym 8 VGPR kernel ma 189 z 256 rejestrow, wiec kazda proba
# potokowania albo wiekszego kafla konczy sie zrzutem rejestrow (zmierzone:
# potok 18 TFLOPS, MTILE=8 17 TFLOPS). Fragment fp8 zajmuje 2 VGPR zamiast 8,
# czyli ta sama geometria miesci sie z zapasem, a sama instrukcja jest
# dwukrotnie szybsza (378 wobec 179 TFLOPS, zmierzone na karcie).
#
# Skale sa PER WIERSZ wagi i PER TOKEN aktywacji, czyli stale wzdluz K. To jest
# roznica jakosciowa wobec formatow blokowych (Q4_K, Q6_K, NVFP4), gdzie skala
# zmienia sie co 32 kolumny i akumulator trzeba zrzucac do f32 w srodku petli —
# tam rachunek wychodzi na zero i dlatego sciezki calkowitoliczbowe nie wygrywaja
# z f16 mimo dwukrotnie wyzszej przepustowosci instrukcji.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.memory import AddressSpace
from std.gpu.sync import barrier
from std.memory import bitcast, stack_allocation

from src.arch_wmma import wmma_acc_row, wmma_fp8_16x16x16

comptime TILE = 16
comptime CHUNK = 64
comptime GROUPS = CHUNK // TILE
# Rozsuniecie wierszy kafla wag w LDS, ta sama przyczyna co w rodzinie K-kwantow.
comptime LDS_PAD = 16


@always_inline
def _frag8(p: UnsafePointer[UInt8, MutAnyOrigin]) -> SIMD[DType.int32, 2]:
    """Osiem bajtow e4m3 tej linii jako fragment operandu RDNA4."""
    return bitcast[DType.int32, 2](p.load[width=8, alignment=1]())


def gemm_fp8_wmma_impl[
    WAVES_M: Int, WAVES_N: Int, MTILE: Int, NTILE: Int, PAD: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    ws_g: UnsafePointer[Float32, MutAnyOrigin],
    xq_g: UnsafePointer[UInt8, MutAnyOrigin],
    xs_g: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Y[t, r] = s_x[t] * s_w[r] * suma_k Xq[t,k] * Wq[r,k], oba operandy e4m3.

    Skale sa PER WIERSZ i PER TOKEN, czyli stale wzdluz K — dzieki temu w petli
    wewnetrznej nie ma zadnego zrzucania akumulatora, inaczej niz przy formatach
    z blokowa skala co 32 kolumny.

    Fragment RDNA4 to osiem bajtow na linie; polowa fali wybiera, ktore osiem z
    szesnastu kolumn kafla niesie ta linia.
    """
    comptime BM = WAVES_M * MTILE * TILE
    comptime BN = WAVES_N * NTILE * TILE
    comptime ROW = CHUNK + PAD

    var lane = Int(thread_idx.x) % 32
    var half = lane // 16
    var wave = Int(thread_idx.x) // 32
    var base_m = Int(block_idx.y) * BM + (wave // WAVES_N) * MTILE * TILE
    var base_n = Int(block_idx.x) * BN + (wave % WAVES_N) * NTILE * TILE

    var x_base = InlineArray[Int, MTILE](fill=0)
    comptime for mt in range(MTILE):
        var m = base_m + mt * TILE + lane % 16
        if m > n_tokens - 1:
            m = n_tokens - 1
        x_base[mt] = m * n_cols

    var acc = InlineArray[SIMD[DType.float32, 8], MTILE * NTILE](
        fill=SIMD[DType.float32, 8](0.0)
    )
    ws = stack_allocation[BN * ROW, UInt8, address_space = AddressSpace.SHARED]()
    var threads = Int(block_dim.x)
    var tid = Int(thread_idx.x)

    var chunk = 0
    while chunk < n_cols // CHUNK:
        var slot = tid
        while slot < BN * GROUPS:
            local_row = slot // GROUPS
            group = slot % GROUPS
            var source_row = Int(block_idx.x) * BN + local_row
            if source_row > n_rows - 1:
                source_row = n_rows - 1
            (ws + local_row * ROW + group * TILE).store(
                (w + source_row * n_cols + chunk * CHUNK + group * TILE).load[
                    width=16, alignment=1
                ]()
            )
            slot += threads
        barrier()

        comptime for sub in range(GROUPS):
            var column = chunk * CHUNK + sub * TILE + half * 8
            var a = InlineArray[SIMD[DType.int32, 2], MTILE](
                fill=SIMD[DType.int32, 2](0)
            )
            comptime for mt in range(MTILE):
                a[mt] = _frag8(xq_g + x_base[mt] + column)
            comptime for nt in range(NTILE):
                local_n = (wave % WAVES_N) * NTILE * TILE + nt * TILE + lane % 16
                b = bitcast[DType.int32, 2](
                    (ws + local_n * ROW + sub * TILE + half * 8).load[
                        width=8, alignment=1
                    ]()
                )
                comptime for mt in range(MTILE):
                    acc[mt * NTILE + nt] = wmma_fp8_16x16x16(
                        a[mt], b, acc[mt * NTILE + nt]
                    )
        barrier()
        chunk += 1

    comptime for mt in range(MTILE):
        comptime for nt in range(NTILE):
            var n = base_n + nt * TILE + lane % 16
            var scale_w: Float32 = 0.0
            if n < n_rows:
                scale_w = ws_g[n]
            comptime for i in range(8):
                var m = base_m + mt * TILE + wmma_acc_row(lane, i)
                if m < n_tokens and n < n_rows:
                    y[m * n_rows + n] = Float16(
                        acc[mt * NTILE + nt][i] * scale_w * xs_g[m]
                    )


# Kafle dobrane pomiarem na R9700 (`bench-amd/bench_gemm_fp8_wmma.mojo`, TFLOPS,
# T=1024), obok kafla f16 o tej samej geometrii:
#
#   ksztalt              f16   fp8 BM512/BN128   fp8 BM256/BN128
#   17408x5120            97               203               172
#   5120x6144             79               139               184
comptime gemm_fp8_wmma_bm512_bn128 = gemm_fp8_wmma_impl[8, 2, 4, 4, LDS_PAD]
comptime gemm_fp8_wmma_bm256_bn128 = gemm_fp8_wmma_impl[4, 2, 4, 4, LDS_PAD]
comptime gemm_fp8_wmma_bm32 = gemm_fp8_wmma_impl[2, 2, 1, 2, LDS_PAD]
