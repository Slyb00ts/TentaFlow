# =============================================================================
# Plik: bench_fp8_traffic.mojo
# Opis: Sprawdza, czy GEMM FP8 prefillu ogranicza pojemnosc L2, a nie kafel.
# Przyklad: pixi run mojo bench_fp8_traffic.mojo
# =============================================================================
#
# GB10 ma 24 MiB L2 i sufit 251 TFLOPS na instrukcji mma (bench_fp8_ceiling).
# Pytanie: czy ktorykolwiek ksztalt prefillu ogranicza ruch wag przez pamiec,
# czy wszystkie sa zwiazane obliczeniem.
#
# UWAGA NA POMIAR. GB10 bezczynny stoi na 208 MHz przy 3003 MHz maksymalnych,
# a rozgrzewka liczona w pojedynczych wywolaniach tego nie podnosi: ten sam
# ksztalt (4096, 11264) zmierzony jako PIERWSZY dawal 876 us, a po rozgrzewce
# 626 us. To 1,4x roznicy wynikajacej wylacznie z kolejnosci przypadkow w
# benchmarku. Dlatego `_warmup` mieli sekunde pelnym GEMM-em przed jakimkolwiek
# pomiarem, a `_case` mierzy dwukrotnie i zwraca druga probe.

from layout import TileTensor, Idx, Coord, row_major
from linalg.matmul.gpu._multistage_gemm_gpu import multistage_gemm_kernel
from linalg.utils_gpu import MatmulConfig
from std.gpu.host import DeviceBuffer, DeviceContext
from std.time import perf_counter_ns
from std.utils.index import Index, IndexList

comptime ITERS = 20
comptime ROUNDS = 7
comptime BK = 64
comptime BM = 128
comptime BN = 256
comptime L2_BYTES = 24 * 1024 * 1024


def gemm_fp8_tile[
    N: Int, K: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    b: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    ws: UnsafePointer[Float32, MutAnyOrigin],
    m: Int,
):
    comptime config = MatmulConfig[
        DType.float8_e4m3fn, DType.float8_e4m3fn, DType.float32, True
    ](
        block_tile_shape=Index(BM, BN, BK),
        warp_tile_shape=Index(64, 64, BK),
    )
    var c_dummy = y.bitcast[Float32]()
    var a_nd = TileTensor(a, row_major(Coord(m, Idx[K])))
    var b_nd = TileTensor(b, row_major(Coord(Idx[N], Idx[K])))
    var c_nd = TileTensor(c_dummy, row_major(Coord(m, Idx[N])))

    @parameter
    @always_inline
    def epi[
        dtype: DType, width: SIMDSize, *, alignment: Int = 1
    ](idx: IndexList[2], val: SIMD[dtype, width]):
        var token = idx[0]
        var column = idx[1]
        var activation_scale = xs[token]
        comptime for lane in range(width):
            y[token * N + column + lane] = (
                val[lane].cast[DType.float32]()
                * activation_scale
                * ws[column + lane]
            ).cast[DType.float16]()

    multistage_gemm_kernel[
        CLT=c_nd.LayoutType,
        ALT=a_nd.LayoutType,
        BLT=b_nd.LayoutType,
        c_linear_idx_type=c_nd.linear_idx_type,
        a_linear_idx_type=a_nd.linear_idx_type,
        b_linear_idx_type=b_nd.linear_idx_type,
        config=config,
        elementwise_lambda_fn=epi,
    ](c_nd, a_nd, b_nd)


def gemm_fp8_tile_ldy[
    N: Int, K: Int, LDY: Int, TBM: Int, TBN: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    b: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    ws: UnsafePointer[Float32, MutAnyOrigin],
    m: Int,
):
    """Wycinek `N` kolumn zapisywany do macierzy o kroku wiersza `LDY`."""
    comptime config = MatmulConfig[
        DType.float8_e4m3fn, DType.float8_e4m3fn, DType.float32, True
    ](
        block_tile_shape=Index(TBM, TBN, BK),
        warp_tile_shape=Index(64, 64, BK),
    )
    var c_dummy = y.bitcast[Float32]()
    var a_nd = TileTensor(a, row_major(Coord(m, Idx[K])))
    var b_nd = TileTensor(b, row_major(Coord(Idx[N], Idx[K])))
    var c_nd = TileTensor(c_dummy, row_major(Coord(m, Idx[N])))

    @parameter
    @always_inline
    def epi[
        dtype: DType, width: SIMDSize, *, alignment: Int = 1
    ](idx: IndexList[2], val: SIMD[dtype, width]):
        var token = idx[0]
        var column = idx[1]
        var activation_scale = xs[token]
        comptime for lane in range(width):
            y[token * LDY + column + lane] = (
                val[lane].cast[DType.float32]()
                * activation_scale
                * ws[column + lane]
            ).cast[DType.float16]()

    multistage_gemm_kernel[
        CLT=c_nd.LayoutType,
        ALT=a_nd.LayoutType,
        BLT=b_nd.LayoutType,
        c_linear_idx_type=c_nd.linear_idx_type,
        a_linear_idx_type=a_nd.linear_idx_type,
        b_linear_idx_type=b_nd.linear_idx_type,
        config=config,
        elementwise_lambda_fn=epi,
    ](c_nd, a_nd, b_nd)


def _median(mut values: InlineArray[Float64, ROUNDS]) -> Float64:
    for left in range(ROUNDS):
        for right in range(left + 1, ROUNDS):
            if values[right] < values[left]:
                values[left], values[right] = values[right], values[left]
    return values[ROUNDS // 2]


def _warmup(ctx: DeviceContext) raises:
    """Mieli pelnym GEMM-em, az zegary wejda na poziom roboczy.

    Bez tego pierwszy mierzony przypadek jest wolniejszy o kilkadziesiat
    procent i wyglada jak wlasciwosc ksztaltu, a nie stanu zegara.
    """
    comptime WN = 4096
    comptime WK = 4096
    comptime WM = 1024
    var y = ctx.enqueue_create_buffer[DType.float16](WM * WN)
    var a = ctx.enqueue_create_buffer[DType.float8_e4m3fn](WM * WK)
    var b = ctx.enqueue_create_buffer[DType.float8_e4m3fn](WN * WK)
    var xs = ctx.enqueue_create_buffer[DType.float32](WM)
    var ws = ctx.enqueue_create_buffer[DType.float32](WN)
    comptime threads = (BM // 64) * (BN // 64) * 32
    comptime smem = (BM + BN) * BK * 4
    for _ in range(4000):
        ctx.enqueue_function[gemm_fp8_tile[WN, WK]](
            y.unsafe_ptr(), a.unsafe_ptr(), b.unsafe_ptr(),
            xs.unsafe_ptr(), ws.unsafe_ptr(), WM,
            grid_dim=(WN // BN, WM // BM),
            block_dim=threads, shared_mem_bytes=smem,
        )
    ctx.synchronize()


def _case[N: Int, K: Int, M: Int](ctx: DeviceContext, name: String) raises:
    var y = ctx.enqueue_create_buffer[DType.float16](M * N)
    var a = ctx.enqueue_create_buffer[DType.float8_e4m3fn](M * K)
    var b = ctx.enqueue_create_buffer[DType.float8_e4m3fn](N * K)
    var xs = ctx.enqueue_create_buffer[DType.float32](M)
    var ws = ctx.enqueue_create_buffer[DType.float32](N)
    with xs.map_to_host() as values:
        for index in range(len(values)):
            values[index] = 1.0
    with ws.map_to_host() as values:
        for index in range(len(values)):
            values[index] = 1.0

    comptime threads = (BM // 64) * (BN // 64) * 32
    comptime smem = (BM + BN) * BK * 4
    for _ in range(5):
        ctx.enqueue_function[gemm_fp8_tile[N, K]](
            y.unsafe_ptr(), a.unsafe_ptr(), b.unsafe_ptr(),
            xs.unsafe_ptr(), ws.unsafe_ptr(), M,
            grid_dim=((N + BN - 1) // BN, (M + BM - 1) // BM),
            block_dim=threads, shared_mem_bytes=smem,
        )
    ctx.synchronize()

    var times = InlineArray[Float64, ROUNDS](uninitialized=True)
    for round_index in range(ROUNDS):
        var started = perf_counter_ns()
        for _ in range(ITERS):
            ctx.enqueue_function[gemm_fp8_tile[N, K]](
                y.unsafe_ptr(), a.unsafe_ptr(), b.unsafe_ptr(),
                xs.unsafe_ptr(), ws.unsafe_ptr(), M,
                grid_dim=((N + BN - 1) // BN, (M + BM - 1) // BM),
                block_dim=threads, shared_mem_bytes=smem,
            )
        ctx.synchronize()
        times[round_index] = Float64(perf_counter_ns() - started) / Float64(
            ITERS
        )

    var ns = _median(times)
    for round_index in range(ROUNDS):
        var started = perf_counter_ns()
        for _ in range(ITERS):
            ctx.enqueue_function[gemm_fp8_tile[N, K]](
                y.unsafe_ptr(), a.unsafe_ptr(), b.unsafe_ptr(),
                xs.unsafe_ptr(), ws.unsafe_ptr(), M,
                grid_dim=((N + BN - 1) // BN, (M + BM - 1) // BM),
                block_dim=threads, shared_mem_bytes=smem,
            )
        ctx.synchronize()
        times[round_index] = Float64(perf_counter_ns() - started) / Float64(
            ITERS
        )
    var ns2 = _median(times)
    if ns2 < ns:
        ns = ns2
    var tflops = Float64(2 * M * N * K) / ns / 1000.0
    var weight_bytes = Float64(N * K)
    var fits = "L2+" if Int(N * K) <= L2_BYTES else "L2-"
    print(
        name, " B=", weight_bytes / (1024.0 * 1024.0), "MiB", fits,
        "  ", ns / 1000.0, "us  ", tflops, "TFLOPS",
    )


def _strips[
    N: Int, K: Int, M: Int, STRIP: Int, TBM: Int, TBN: Int
](ctx: DeviceContext, name: String) raises:
    """Ten sam pelny GEMM policzony jako `N/STRIP` wywolan po STRIP kolumn.

    Kazde wywolanie czyta wlasny plaster B o rozmiarze STRIP*K i zapisuje
    wycinek kolumn do pelnej macierzy o kroku wiersza N, wiec wynik jest
    identyczny — zmienia sie wylacznie to, ile wag musi przejsc przez pamiec.
    """
    comptime COUNT = N // STRIP
    var y = ctx.enqueue_create_buffer[DType.float16](M * N)
    var a = ctx.enqueue_create_buffer[DType.float8_e4m3fn](M * K)
    var b = ctx.enqueue_create_buffer[DType.float8_e4m3fn](N * K)
    var xs = ctx.enqueue_create_buffer[DType.float32](M)
    var ws = ctx.enqueue_create_buffer[DType.float32](N)
    with xs.map_to_host() as values:
        for index in range(len(values)):
            values[index] = 1.0
    with ws.map_to_host() as values:
        for index in range(len(values)):
            values[index] = 1.0

    comptime threads = (TBM // 64) * (TBN // 64) * 32
    comptime smem = (TBM + TBN) * BK * 4

    @parameter
    @always_inline
    def run() raises:
        comptime for chunk in range(COUNT):
            ctx.enqueue_function[gemm_fp8_tile_ldy[STRIP, K, N, TBM, TBN]](
                y.unsafe_ptr() + chunk * STRIP,
                a.unsafe_ptr(),
                b.unsafe_ptr() + chunk * STRIP * K,
                xs.unsafe_ptr(),
                ws.unsafe_ptr() + chunk * STRIP,
                M,
                grid_dim=(STRIP // TBN, (M + TBM - 1) // TBM),
                block_dim=threads,
                shared_mem_bytes=smem,
            )

    for _ in range(5):
        run()
    ctx.synchronize()

    var times = InlineArray[Float64, ROUNDS](uninitialized=True)
    for round_index in range(ROUNDS):
        var started = perf_counter_ns()
        for _ in range(ITERS):
            run()
        ctx.synchronize()
        times[round_index] = Float64(perf_counter_ns() - started) / Float64(
            ITERS
        )

    var ns = _median(times)
    var tflops = Float64(2 * M * N * K) / ns / 1000.0
    print(
        name, " plastry", COUNT, "x", STRIP, " BM=", TBM, "BN=", TBN,
        " blokow/plaster", (STRIP // TBN) * ((M + TBM - 1) // TBM),
        "  ", ns / 1000.0, "us  ", tflops, "TFLOPS",
    )


def main() raises:
    var ctx = DeviceContext()
    _warmup(ctx)
    # Czy szerokie N naprawde sie zalamuje. Na tym opiera sie dzielenie
    # gate/up na kawalki po 4096 kolumn (`fp8_modular_column_chunks`).
    _case[4096, 4096, 1024](ctx, "N= 4096 K=4096")
    _case[5632, 4096, 1024](ctx, "N= 5632 K=4096")
    _case[8192, 4096, 1024](ctx, "N= 8192 K=4096")
    _case[11264, 4096, 1024](ctx, "N=11264 K=4096")
    _case[14336, 4096, 1024](ctx, "N=14336 K=4096")
    # Ten sam pelny gate/up jako kawalki po 4096 kolumn, czyli to co robi dzis
    # silnik, wobec jednego wywolania powyzej.
    _strips[11264, 4096, 1024, 1408, 128, 256](ctx, "gate/up   ")
    _case[11264, 4096, 1024](ctx, "N=11264 (powtorka)")
