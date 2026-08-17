# =============================================================================
# Plik: bench_fp8_config.mojo
# Opis: Przeszukuje nastawy MatmulConfig dla GEMM FP8 prefillu na GB10.
# Przyklad: pixi run mojo bench_fp8_config.mojo
# =============================================================================
#
# `gemm_fp8_modular.mojo` podaje `MatmulConfig` tylko dwa ksztalty kafla, wiec
# CALA reszta zostaje na wartosciach domyslnych: `num_pipeline_stages=4`,
# `k_group_size=1`, `num_k_partitions=1`. `k_group_size` to glebokosc
# podwojnego buforowania fragmentow w rejestrach — przy 8 warpach na SM to
# jedyne, co moze zaslonic opoznienie `ldmatrix`, bo zajetosci podniesc sie nie
# da: kafel warpa musi zostac 64x64, a 16 warpow z takim akumulatorem nie
# miesci sie w pliku rejestrow.
#
# Pamiec wspoldzielona to `(BM+BN) * BK * stages` bajtow i limit na blok wynosi
# 101376, wiec glebokosc potoku i rozmiar kafla wymieniaja sie miedzy soba.

from layout import TileTensor, Idx, Coord, row_major
from linalg.matmul.gpu._multistage_gemm_gpu import multistage_gemm_kernel
from linalg.utils_gpu import MatmulConfig
from std.gpu.host import DeviceBuffer, DeviceContext
from std.time import perf_counter_ns
from std.utils.index import Index, IndexList

comptime ITERS = 20
comptime ROUNDS = 7


def gemm_cfg[
    N: Int, K: Int, BM: Int, BN: Int, BK: Int, STAGES: Int, KGROUP: Int,
    VEC_EPI: Bool = False,
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
        num_pipeline_stages=STAGES,
        k_group_size=KGROUP,
    )
    var c_dummy = y.bitcast[Float32]()
    var a_nd = TileTensor(a, row_major(Coord(m, Idx[K])))
    var b_nd = TileTensor(b, row_major(Coord(Idx[N], Idx[K])))
    var c_nd = TileTensor(c_dummy, row_major(Coord(m, Idx[N])))

    @parameter
    @always_inline
    def epi[
        dtype: DType, width: SIMDLength, *, alignment: Int = 1
    ](idx: IndexList[2], val: SIMD[dtype, width]):
        var token = idx[0]
        var column = idx[1]
        var activation_scale = xs[token]
        comptime if VEC_EPI:
            # Epilog dostaje `width` kolumn naraz. Skala tokena jest dla nich
            # wspolna, a skale wierszy leza obok siebie, wiec caly kafel idzie
            # jednym odczytem i jednym zapisem zamiast `width` zapisow po 2 B.
            var row_scales = (ws + column).load[width=width]()
            (y + token * N + column).store(
                (
                    val.cast[DType.float32]() * activation_scale * row_scales
                ).cast[DType.float16]()
            )
        else:
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


def _median(mut values: InlineArray[Float64, ROUNDS]) -> Float64:
    for left in range(ROUNDS):
        for right in range(left + 1, ROUNDS):
            if values[right] < values[left]:
                values[left], values[right] = values[right], values[left]
    return values[ROUNDS // 2]


def _warmup(ctx: DeviceContext) raises:
    var y = ctx.enqueue_create_buffer[DType.float16](1024 * 4096)
    var a = ctx.enqueue_create_buffer[DType.float8_e4m3fn](1024 * 4096)
    var b = ctx.enqueue_create_buffer[DType.float8_e4m3fn](4096 * 4096)
    var xs = ctx.enqueue_create_buffer[DType.float32](1024)
    var ws = ctx.enqueue_create_buffer[DType.float32](4096)
    for _ in range(4000):
        ctx.enqueue_function[gemm_cfg[4096, 4096, 128, 256, 64, 4, 1]](
            y.unsafe_ptr(), a.unsafe_ptr(), b.unsafe_ptr(),
            xs.unsafe_ptr(), ws.unsafe_ptr(), 1024,
            grid_dim=(16, 8), block_dim=256, shared_mem_bytes=98304,
        )
    ctx.synchronize()


def _run[
    N: Int, K: Int, M: Int, BM: Int, BN: Int, BK: Int, STAGES: Int, KGROUP: Int,
    VEC_EPI: Bool = False,
](ctx: DeviceContext, name: String) raises:
    comptime THREADS = (BM // 64) * (BN // 64) * 32
    comptime SMEM = (BM + BN) * BK * STAGES
    var y = ctx.enqueue_create_buffer[DType.float16](M * N)
    var a = ctx.enqueue_create_buffer[DType.float8_e4m3fn](M * K)
    var b = ctx.enqueue_create_buffer[DType.float8_e4m3fn](N * K)
    var xs = ctx.enqueue_create_buffer[DType.float32](M)
    var ws = ctx.enqueue_create_buffer[DType.float32](N)

    @parameter
    @always_inline
    def go() raises:
        ctx.enqueue_function[
            gemm_cfg[N, K, BM, BN, BK, STAGES, KGROUP, VEC_EPI]
        ](
            y.unsafe_ptr(), a.unsafe_ptr(), b.unsafe_ptr(),
            xs.unsafe_ptr(), ws.unsafe_ptr(), M,
            grid_dim=(N // BN, (M + BM - 1) // BM),
            block_dim=THREADS, shared_mem_bytes=SMEM,
        )

    for _ in range(20):
        go()
    ctx.synchronize()
    var times = InlineArray[Float64, ROUNDS](uninitialized=True)
    for round_index in range(ROUNDS):
        var started = perf_counter_ns()
        for _ in range(ITERS):
            go()
        ctx.synchronize()
        times[round_index] = Float64(perf_counter_ns() - started) / Float64(
            ITERS
        )
    var ns = _median(times)
    print(
        name, " BM=", BM, "BN=", BN, "BK=", BK,
        "etapy=", STAGES, "kgroup=", KGROUP, "smem=", SMEM,
        "  ", ns / 1000.0, "us  ",
        Float64(2 * M * N * K) / ns / 1000.0, "TFLOPS",
    )


def main() raises:
    var ctx = DeviceContext()
    _warmup(ctx)
    # Najpierw baza, potem ta sama nastawa z epilogiem wektorowym.
    _run[4096, 4096, 1024, 128, 256, 64, 4, 1, False](ctx, "q/o  skalarny")
    _run[4096, 4096, 1024, 128, 256, 64, 4, 1, True](ctx, "q/o  WEKTOR  ")
    _run[4096, 11264, 1024, 128, 256, 64, 4, 1, False](ctx, "down skalarny")
    _run[4096, 11264, 1024, 128, 256, 64, 4, 1, True](ctx, "down WEKTOR  ")
    _run[11264, 4096, 1024, 128, 256, 64, 4, 1, False](ctx, "g/u  skalarny")
    _run[11264, 4096, 1024, 128, 256, 64, 4, 1, True](ctx, "g/u  WEKTOR  ")
