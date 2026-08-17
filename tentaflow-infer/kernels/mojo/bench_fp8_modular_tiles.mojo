# =============================================================================
# Plik: bench_fp8_modular_tiles.mojo
# Opis: Porównuje kafle wieloetapowego GEMM FP8 dla kształtów prefill Bielika.
# Przykład: pixi run mojo bench_fp8_modular_tiles.mojo
# =============================================================================

from layout import TileTensor, Idx, Coord, row_major
from linalg.matmul.gpu._multistage_gemm_gpu import multistage_gemm_kernel
from linalg.utils_gpu import MatmulConfig
from std.gpu.host import DeviceBuffer, DeviceContext
from std.time import perf_counter_ns
from std.utils.index import Index, IndexList

comptime ITERS = 20
comptime ROUNDS = 7
comptime BK = 64


def gemm_fp8_tile[
    N: Int, K: Int, BM: Int, BN: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    b: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    ws: UnsafePointer[Float32, MutAnyOrigin],
    m: Int,
):
    comptime config = MatmulConfig[
        DType.float8_e4m3fn,
        DType.float8_e4m3fn,
        DType.float32,
        True,
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


def _median(mut values: InlineArray[Float64, ROUNDS]) -> Float64:
    for left in range(ROUNDS):
        for right in range(left + 1, ROUNDS):
            if values[right] < values[left]:
                values[left], values[right] = values[right], values[left]
    return values[ROUNDS // 2]


def _measure[
    N: Int, K: Int, M: Int, BM: Int, BN: Int
](
    ctx: DeviceContext,
    mut y: DeviceBuffer[DType.float16],
    mut a: DeviceBuffer[DType.float8_e4m3fn],
    mut b: DeviceBuffer[DType.float8_e4m3fn],
    mut xs: DeviceBuffer[DType.float32],
    mut ws: DeviceBuffer[DType.float32],
) raises -> Float64:
    comptime threads = BM // 64 * (BN // 64) * 32
    comptime smem = (BM + BN) * BK * 4
    for _ in range(5):
        ctx.enqueue_function[gemm_fp8_tile[N, K, BM, BN]](
            y.unsafe_ptr(),
            a.unsafe_ptr(),
            b.unsafe_ptr(),
            xs.unsafe_ptr(),
            ws.unsafe_ptr(),
            M,
            grid_dim=((N + BN - 1) // BN, (M + BM - 1) // BM),
            block_dim=threads,
            shared_mem_bytes=smem,
        )
    ctx.synchronize()
    var times = InlineArray[Float64, ROUNDS](uninitialized=True)
    for round_index in range(ROUNDS):
        var started = perf_counter_ns()
        for _ in range(ITERS):
            ctx.enqueue_function[gemm_fp8_tile[N, K, BM, BN]](
                y.unsafe_ptr(),
                a.unsafe_ptr(),
                b.unsafe_ptr(),
                xs.unsafe_ptr(),
                ws.unsafe_ptr(),
                M,
                grid_dim=((N + BN - 1) // BN, (M + BM - 1) // BM),
                block_dim=threads,
                shared_mem_bytes=smem,
            )
        ctx.synchronize()
        times[round_index] = Float64(perf_counter_ns() - started) / Float64(
            ITERS
        )
    return _median(times) / 1000.0


def _case[N: Int, K: Int, M: Int](ctx: DeviceContext, name: String) raises:
    var y = ctx.enqueue_create_buffer[DType.float16](M * N)
    var a = ctx.enqueue_create_buffer[DType.float8_e4m3fn](M * K)
    var b = ctx.enqueue_create_buffer[DType.float8_e4m3fn](N * K)
    var xs = ctx.enqueue_create_buffer[DType.float32](M)
    var ws = ctx.enqueue_create_buffer[DType.float32](N)
    with a.map_to_host() as values:
        for index in range(len(values)):
            values[index] = Scalar[DType.float8_e4m3fn](0.015625)
    with b.map_to_host() as values:
        for index in range(len(values)):
            values[index] = Scalar[DType.float8_e4m3fn](
                Float32((index % 7) - 3) * 0.015625
            )
    with xs.map_to_host() as values:
        for index in range(len(values)):
            values[index] = 1.0
    with ws.map_to_host() as values:
        for index in range(len(values)):
            values[index] = 1.0

    current = _measure[N, K, M, 128, 128](ctx, y, a, b, xs, ws)
    m64_n128 = _measure[N, K, M, 64, 128](ctx, y, a, b, xs, ws)
    m128_n64 = _measure[N, K, M, 128, 64](ctx, y, a, b, xs, ws)
    m64_n256 = _measure[N, K, M, 64, 256](ctx, y, a, b, xs, ws)
    m128_n256 = _measure[N, K, M, 128, 256](ctx, y, a, b, xs, ws)
    print(
        name,
        "current_us",
        current,
        "m64_n128",
        m64_n128,
        "m128_n64",
        m128_n64,
        "m64_n256",
        m64_n256,
        "m128_n256",
        m128_n256,
    )


def main() raises:
    var ctx = DeviceContext()
    _case[4096, 4096, 128](ctx, "q_o_m128")
    _case[4096, 4096, 256](ctx, "q_o_m256")
    _case[4096, 4096, 512](ctx, "q_o_m512")
    _case[4096, 4096, 640](ctx, "q_o_m640")
    _case[4096, 4096, 768](ctx, "q_o_m768")
    _case[4096, 4096, 896](ctx, "q_o_m896")
    _case[4096, 4096, 1024](ctx, "q_o")
    _case[11264, 4096, 128](ctx, "gate_up_m128")
    _case[11264, 4096, 256](ctx, "gate_up_m256")
    _case[11264, 4096, 512](ctx, "gate_up_m512")
    _case[11264, 4096, 640](ctx, "gate_up_m640")
    _case[11264, 4096, 768](ctx, "gate_up_m768")
    _case[11264, 4096, 896](ctx, "gate_up_m896")
    _case[11264, 4096, 1024](ctx, "gate_up")
    _case[4096, 11264, 128](ctx, "down_m128")
    _case[4096, 11264, 256](ctx, "down_m256")
    _case[4096, 11264, 512](ctx, "down_m512")
    _case[4096, 11264, 1024](ctx, "down")
