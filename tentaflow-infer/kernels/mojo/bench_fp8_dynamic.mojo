# =============================================================================
# Plik: bench_fp8_dynamic.mojo
# Opis: Sprawdza, czy wieloetapowy GEMM FP8 znosi RUNTIME'OWE N, K i LDY.
# Przyklad: pixi run mojo bench_fp8_dynamic.mojo
# =============================================================================
#
# Dzisiejsze plastry kolumnowe sa zahardkodowane: kazda szerokosc plastra to
# osobna instancja `gemm_fp8_mod_tile[N, K, BN, LDY]`, czyli osobny PTX i wpis
# w tabeli `match (rows, cols)`. Dziala to dla jednego modelu i rozsypuje sie
# przy kazdym nastepnym (ksztalty Gemmy nie sa nawet w katalogu).
#
# Jesli kernel przyjmie N, K i LDY jako argumenty wykonania, zostaje JEDEN
# artefakt na BN, a szerokosc plastra liczy sie w runtime z liczby SM. Pytanie
# jest wylacznie o cene: statyczne wymiary pozwalaja rozwinac petle po K i
# policzyc predykaty brzegowe w czasie kompilacji.
#
# Bench mierzy trzy warianty na ksztaltach prefillu Bielika i sprawdza, czy
# wyjscie zostaje BITOWO identyczne.

from layout import TileTensor, Idx, Coord, row_major
from linalg.matmul.gpu._multistage_gemm_gpu import multistage_gemm_kernel
from linalg.utils_gpu import MatmulConfig
from std.gpu.host import DeviceBuffer, DeviceContext
from std.time import perf_counter_ns
from std.utils.index import Index, IndexList

comptime ITERS = 20
comptime ROUNDS = 7
comptime BK = 64
comptime BN = 256
comptime BM = 128
comptime THREADS = 256
comptime SMEM = 98304


comptime config = MatmulConfig[
    DType.float8_e4m3fn, DType.float8_e4m3fn, DType.float32, True
](block_tile_shape=Index(BM, BN, BK), warp_tile_shape=Index(64, 64, BK))


def gemm_static[
    N: Int, K: Int, LDY: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    b: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    ws: UnsafePointer[Float32, MutAnyOrigin],
    m: Int,
):
    """Dzisiejszy ksztalt: wszystko poza liczba tokenow jest statyczne."""
    var c_dummy = y.bitcast[Float32]()
    var a_nd = TileTensor(a, row_major(Coord(m, Idx[K])))
    var b_nd = TileTensor(b, row_major(Coord(Idx[N], Idx[K])))
    var c_nd = TileTensor(c_dummy, row_major(Coord(m, Idx[N])))

    @parameter
    @always_inline
    def epi[
        dtype: DType, width: SIMDSize, *, alignment: Int = 1
    ](idx: IndexList[2], val: SIMD[dtype, width]):
        var t = idx[0]
        var col = idx[1]
        var sa = xs[t]
        var row_scales = (ws + col).load[width=width]()
        (y + t * LDY + col).store(
            (val.cast[DType.float32]() * sa * row_scales).cast[DType.float16]()
        )

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


def gemm_dyn_n[
    K: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    b: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    ws: UnsafePointer[Float32, MutAnyOrigin],
    m: Int,
    n: Int,
    ldy: Int,
):
    """Szerokosc plastra i krok wyjscia z runtime, K nadal statyczne."""
    var c_dummy = y.bitcast[Float32]()
    var a_nd = TileTensor(a, row_major(Coord(m, Idx[K])))
    var b_nd = TileTensor(b, row_major(Coord(n, Idx[K])))
    var c_nd = TileTensor(c_dummy, row_major(Coord(m, n)))

    @parameter
    @always_inline
    def epi[
        dtype: DType, width: SIMDSize, *, alignment: Int = 1
    ](idx: IndexList[2], val: SIMD[dtype, width]):
        var t = idx[0]
        var col = idx[1]
        var sa = xs[t]
        var row_scales = (ws + col).load[width=width]()
        (y + t * ldy + col).store(
            (val.cast[DType.float32]() * sa * row_scales).cast[DType.float16]()
        )

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


def gemm_dyn_all(
    y: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    b: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    ws: UnsafePointer[Float32, MutAnyOrigin],
    m: Int,
    n: Int,
    k: Int,
    ldy: Int,
):
    """Jeden artefakt na BN: kazdy wymiar przychodzi w argumentach."""
    var c_dummy = y.bitcast[Float32]()
    var a_nd = TileTensor(a, row_major(Coord(m, k)))
    var b_nd = TileTensor(b, row_major(Coord(n, k)))
    var c_nd = TileTensor(c_dummy, row_major(Coord(m, n)))

    @parameter
    @always_inline
    def epi[
        dtype: DType, width: SIMDSize, *, alignment: Int = 1
    ](idx: IndexList[2], val: SIMD[dtype, width]):
        var t = idx[0]
        var col = idx[1]
        var sa = xs[t]
        var row_scales = (ws + col).load[width=width]()
        (y + t * ldy + col).store(
            (val.cast[DType.float32]() * sa * row_scales).cast[DType.float16]()
        )

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


def _fill(
    ctx: DeviceContext,
    mut a: DeviceBuffer[DType.float8_e4m3fn],
    mut b: DeviceBuffer[DType.float8_e4m3fn],
    mut xs: DeviceBuffer[DType.float32],
    mut ws: DeviceBuffer[DType.float32],
) raises:
    with a.map_to_host() as values:
        for index in range(len(values)):
            values[index] = Scalar[DType.float8_e4m3fn](
                Float32((index % 13) - 6) * 0.03125
            )
    with b.map_to_host() as values:
        for index in range(len(values)):
            values[index] = Scalar[DType.float8_e4m3fn](
                Float32((index % 7) - 3) * 0.015625
            )
    with xs.map_to_host() as values:
        for index in range(len(values)):
            values[index] = 1.0 + Float32(index % 5) * 0.25
    with ws.map_to_host() as values:
        for index in range(len(values)):
            values[index] = 0.5 + Float32(index % 11) * 0.125


def _case[N: Int, K: Int, M: Int](ctx: DeviceContext, name: String) raises:
    var y0 = ctx.enqueue_create_buffer[DType.float16](M * N)
    var y1 = ctx.enqueue_create_buffer[DType.float16](M * N)
    var y2 = ctx.enqueue_create_buffer[DType.float16](M * N)
    var a = ctx.enqueue_create_buffer[DType.float8_e4m3fn](M * K)
    var b = ctx.enqueue_create_buffer[DType.float8_e4m3fn](N * K)
    var xs = ctx.enqueue_create_buffer[DType.float32](M)
    var ws = ctx.enqueue_create_buffer[DType.float32](N)
    _fill(ctx, a, b, xs, ws)

    comptime gx = (N + BN - 1) // BN
    comptime gy = (M + BM - 1) // BM

    var times = InlineArray[Float64, ROUNDS](uninitialized=True)

    # --- statyczny ---
    for _ in range(200):
        ctx.enqueue_function[gemm_static[N, K, N]](
            y0.unsafe_ptr(), a.unsafe_ptr(), b.unsafe_ptr(),
            xs.unsafe_ptr(), ws.unsafe_ptr(), M,
            grid_dim=(gx, gy), block_dim=THREADS, shared_mem_bytes=SMEM,
        )
    ctx.synchronize()
    for r in range(ROUNDS):
        var s0 = perf_counter_ns()
        for _ in range(ITERS):
            ctx.enqueue_function[gemm_static[N, K, N]](
                y0.unsafe_ptr(), a.unsafe_ptr(), b.unsafe_ptr(),
                xs.unsafe_ptr(), ws.unsafe_ptr(), M,
                grid_dim=(gx, gy), block_dim=THREADS, shared_mem_bytes=SMEM,
            )
        ctx.synchronize()
        times[r] = Float64(perf_counter_ns() - s0) / Float64(ITERS)
    var t_static = _median(times) / 1000.0

    # --- dynamiczne N ---
    for _ in range(200):
        ctx.enqueue_function[gemm_dyn_n[K]](
            y1.unsafe_ptr(), a.unsafe_ptr(), b.unsafe_ptr(),
            xs.unsafe_ptr(), ws.unsafe_ptr(), M, N, N,
            grid_dim=(gx, gy), block_dim=THREADS, shared_mem_bytes=SMEM,
        )
    ctx.synchronize()
    for r in range(ROUNDS):
        var s0 = perf_counter_ns()
        for _ in range(ITERS):
            ctx.enqueue_function[gemm_dyn_n[K]](
                y1.unsafe_ptr(), a.unsafe_ptr(), b.unsafe_ptr(),
                xs.unsafe_ptr(), ws.unsafe_ptr(), M, N, N,
                grid_dim=(gx, gy), block_dim=THREADS, shared_mem_bytes=SMEM,
            )
        ctx.synchronize()
        times[r] = Float64(perf_counter_ns() - s0) / Float64(ITERS)
    var t_dyn_n = _median(times) / 1000.0

    # --- wszystko dynamiczne ---
    for _ in range(200):
        ctx.enqueue_function[gemm_dyn_all](
            y2.unsafe_ptr(), a.unsafe_ptr(), b.unsafe_ptr(),
            xs.unsafe_ptr(), ws.unsafe_ptr(), M, N, K, N,
            grid_dim=(gx, gy), block_dim=THREADS, shared_mem_bytes=SMEM,
        )
    ctx.synchronize()
    for r in range(ROUNDS):
        var s0 = perf_counter_ns()
        for _ in range(ITERS):
            ctx.enqueue_function[gemm_dyn_all](
                y2.unsafe_ptr(), a.unsafe_ptr(), b.unsafe_ptr(),
                xs.unsafe_ptr(), ws.unsafe_ptr(), M, N, K, N,
                grid_dim=(gx, gy), block_dim=THREADS, shared_mem_bytes=SMEM,
            )
        ctx.synchronize()
        times[r] = Float64(perf_counter_ns() - s0) / Float64(ITERS)
    var t_dyn_all = _median(times) / 1000.0

    var h0 = ctx.enqueue_create_host_buffer[DType.float16](M * N)
    var h1 = ctx.enqueue_create_host_buffer[DType.float16](M * N)
    var h2 = ctx.enqueue_create_host_buffer[DType.float16](M * N)
    ctx.enqueue_copy(h0, y0)
    ctx.enqueue_copy(h1, y1)
    ctx.enqueue_copy(h2, y2)
    ctx.synchronize()
    var diff_n = 0
    var diff_all = 0
    for i in range(M * N):
        if h0[i] != h1[i]:
            diff_n += 1
        if h0[i] != h2[i]:
            diff_all += 1
    var bad_rows = 0
    var first_bad_row = -1
    var first_bad_col = -1
    for t in range(M):
        var row_bad = 0
        for c in range(N):
            if h0[t * N + c] != h1[t * N + c]:
                row_bad += 1
                if first_bad_row < 0:
                    first_bad_row = t
                    first_bad_col = c
        if row_bad != 0:
            bad_rows += 1
    print(
        "  zle wiersze:", bad_rows, "z", M,
        "| pierwszy blad t=", first_bad_row, "c=", first_bad_col,
    )
    var shown = 0
    var good = String("  dobre wiersze:")
    for t in range(M):
        if shown >= 24:
            break
        var ok = True
        for c in range(N):
            if h0[t * N + c] != h1[t * N + c]:
                ok = False
                break
        if ok:
            good += " " + String(t)
            shown += 1
    print(good)
    var zero_run = 0
    for c in range(N):
        if h1[1 * N + c] == Float16(0.0):
            zero_run += 1
    print("  zer w wierszu 1 (dyn):", zero_run, "z", N)

    var flops = 2.0 * Float64(M) * Float64(N) * Float64(K)
    print(
        name,
        "| static", t_static, "us", flops / (t_static * 1e6), "TFLOPS",
        "| dyn_n", t_dyn_n, "us", flops / (t_dyn_n * 1e6), "TFLOPS",
        "| dyn_all", t_dyn_all, "us", flops / (t_dyn_all * 1e6), "TFLOPS",
        "| roznice:", diff_n, diff_all,
    )


def main() raises:
    var ctx = DeviceContext()
    _case[4096, 4096, 1024](ctx, "q_o      (4096,4096)")
    _case[1536, 4096, 1024](ctx, "plaster  (1536,4096)")
    _case[11264, 4096, 1024](ctx, "gate_up  (11264,4096)")
    _case[4096, 11264, 1024](ctx, "down     (4096,11264)")
