# =============================================================================
# Plik: test_gemm_dot.mojo
# Opis: Sprawdza poprawność `gemm_f16_dot2` wobec referencji na hoście i mierzy
#       jego przepustowość na kształtach prefillu.
# Przykład: pixi run mojo run -I . bench-amd/test_gemm_dot.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from std.math import isclose
from src.gemm_dot import gemm_f16_dot2_impl

comptime ITERS = 10


def check[BM: Int, BN: Int, TM: Int, TN: Int](ctx: DeviceContext, tokens: Int, rows: Int, cols: Int) raises:
    var xh = ctx.enqueue_create_host_buffer[DType.float16](tokens * cols)
    var wh = ctx.enqueue_create_host_buffer[DType.float16](rows * cols)
    ctx.synchronize()
    for t in range(tokens):
        for k in range(cols):
            xh[t * cols + k] = Float16(Float32((t * 7 + k * 3) % 17) * 0.0625 - 0.5)
    for r in range(rows):
        for k in range(cols):
            wh[r * cols + k] = Float16(Float32((r * 5 + k * 11) % 13) * 0.0625 - 0.375)

    var xd = ctx.enqueue_create_buffer[DType.float16](tokens * cols)
    var wd = ctx.enqueue_create_buffer[DType.float16](rows * cols)
    var yd = ctx.enqueue_create_buffer[DType.float16](tokens * rows)
    ctx.enqueue_copy(xd, xh)
    ctx.enqueue_copy(wd, wh)
    ctx.synchronize()

    grid_x = (rows + BN - 1) // BN
    grid_y = (tokens + BM - 1) // BM
    ctx.enqueue_function[gemm_f16_dot2_impl[BM, BN, TM, TN]](
        yd.unsafe_ptr(),
        wd.unsafe_ptr(),
        xd.unsafe_ptr(),
        cols,
        rows,
        tokens,
        grid_dim=(grid_x, grid_y),
        block_dim=(BM // TM) * (BN // TN),
    )
    ctx.synchronize()

    var worst: Float32 = 0.0
    with yd.map_to_host() as got:
        for t in range(tokens):
            for r in range(rows):
                var expect: Float32 = 0.0
                for k in range(cols):
                    expect += Float32(xh[t * cols + k]) * Float32(wh[r * cols + k])
                have = Float32(got[t * rows + r])
                denom = abs(expect) if abs(expect) > 1.0 else 1.0
                err = abs(have - expect) / denom
                if err > worst:
                    worst = err
    print("T=", tokens, "rows=", rows, "cols=", cols, "wzgledny blad max:", worst)
    # Wyjście jest f16, więc próg to zaokrąglenie f16 (2^-11) z zapasem na
    # kolejność sumowania; referencja liczy w f32 w innej kolejności.
    if worst > 0.002:
        raise Error("gemm_f16_dot2 poza tolerancja f16")


def bench[BM: Int, BN: Int, TM: Int, TN: Int](label: String, ctx: DeviceContext, tokens: Int, rows: Int, cols: Int) raises:
    var xd = ctx.enqueue_create_buffer[DType.float16](tokens * cols)
    var wd = ctx.enqueue_create_buffer[DType.float16](rows * cols)
    var yd = ctx.enqueue_create_buffer[DType.float16](tokens * rows)
    ctx.synchronize()
    grid_x = (rows + BN - 1) // BN
    grid_y = (tokens + BM - 1) // BM
    for _ in range(3):
        ctx.enqueue_function[gemm_f16_dot2_impl[BM, BN, TM, TN]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xd.unsafe_ptr(), cols, rows, tokens,
            grid_dim=(grid_x, grid_y), block_dim=(BM // TM) * (BN // TN),
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_f16_dot2_impl[BM, BN, TM, TN]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xd.unsafe_ptr(), cols, rows, tokens,
            grid_dim=(grid_x, grid_y), block_dim=(BM // TM) * (BN // TN),
        )
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    flops = 2.0 * Float64(tokens) * Float64(rows) * Float64(cols)
    print(
        label, "T=", tokens, "rows=", rows, "cols=", cols,
        Int(flops / dt / 1e12), "TFLOPS", Int(dt * 1e6), "us",
    )


def main() raises:
    var ctx = DeviceContext()
    print("arch:", ctx.arch_name())
    check[64, 64, 4, 4](ctx, 64, 64, 64)
    check[64, 64, 4, 4](ctx, 37, 70, 104)
    check[64, 64, 4, 4](ctx, 65, 129, 4096)
    check[128, 64, 8, 4](ctx, 200, 130, 1024)
    check[128, 128, 8, 4](ctx, 300, 300, 1024)
    check[128, 128, 8, 8](ctx, 300, 300, 1024)
    check[256, 64, 8, 8](ctx, 300, 130, 1024)
    check[256, 128, 8, 8](ctx, 300, 300, 1024)
    print("--- kafle na kształtach warstw ---")
    # qwen0.6B: 1024/3072. 7B: 4096/14336.
    bench[64, 64, 4, 4]("64x64  ", ctx, 1024, 4096, 4096)
    bench[128, 64, 8, 4]("128x64 ", ctx, 1024, 4096, 4096)
    bench[128, 128, 8, 8]("128x128", ctx, 1024, 4096, 4096)
    bench[256, 64, 8, 8]("256x64 ", ctx, 1024, 4096, 4096)
    bench[128, 128, 8, 4]("128x128/512w", ctx, 1024, 4096, 4096)
    bench[256, 128, 8, 8]("256x128/512w", ctx, 1024, 4096, 4096)
    bench[128, 256, 8, 8]("128x256/512w", ctx, 1024, 4096, 4096)
    bench[64, 128, 4, 4]("64x128/512w ", ctx, 1024, 4096, 4096)
    bench[128, 128, 4, 4]("128x128/1024w", ctx, 1024, 4096, 4096)
    bench[128, 64, 4, 4]("128x64/512w ", ctx, 1024, 4096, 4096)
    bench[192, 128, 8, 4]("192x128/768w", ctx, 1024, 4096, 4096)
    print("--- najlepszy kafel na pozostalych kształtach ---")
    bench[128, 128, 8, 8]("128x128", ctx, 512, 1024, 1024)
    bench[128, 128, 8, 8]("128x128", ctx, 512, 3072, 1024)
    bench[128, 128, 8, 8]("128x128", ctx, 1024, 14336, 4096)
