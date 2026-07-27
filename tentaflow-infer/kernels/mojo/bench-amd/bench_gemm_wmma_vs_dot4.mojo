# =============================================================================
# Plik: bench_gemm_wmma_vs_dot4.mojo
# Opis: A/B prefillowego GEMM-u Q8_0 — kafel na jednostce macierzowej RDNA3
#       (WMMA) wobec kafla na instrukcji dot (`v_dot4_i32_i8`), na tych samych
#       kształtach i tych samych danych.
# Przykład: pixi run mojo run -I . bench-amd/bench_gemm_wmma_vs_dot4.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns

from src.gemm_dot import gemm_q8_0_dot4_128x128
from src.gemm_wmma import gemm_q8_0_wmma_16x64, gemm_q8_0_wmma_64x128

comptime QBYTES = 34
comptime WARMUP = 3
comptime ITERS = 10


def run(ctx: DeviceContext, n_tokens: Int, n_rows: Int, n_cols: Int) raises:
    blocks = n_cols // 32
    var wd = ctx.enqueue_create_buffer[DType.uint8](n_rows * blocks * QBYTES)
    var xqd = ctx.enqueue_create_buffer[DType.int8](n_tokens * n_cols)
    var xdd = ctx.enqueue_create_buffer[DType.float32](blocks * n_tokens)
    var xsm = ctx.enqueue_create_buffer[DType.float32](blocks * n_tokens)
    var yd = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_rows)
    ctx.synchronize()

    # 2 * T * rows * cols operacji (mnożenie i dodawanie).
    ops = 2.0 * Float64(n_tokens) * Float64(n_rows) * Float64(n_cols)

    var t0: Int = 0
    var t1: Int = 0
    var t2: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t0 = perf_counter_ns()
        ctx.enqueue_function[gemm_q8_0_wmma_64x128](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xqd.unsafe_ptr(), xdd.unsafe_ptr(),
            xsm.unsafe_ptr(), n_cols, n_rows, n_tokens,
            grid_dim=((n_rows + 127) // 128, (n_tokens + 63) // 64), block_dim=128,
        )
    ctx.synchronize()
    big_s = Float64(perf_counter_ns() - t0) / 1e9 / Float64(ITERS)

    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t2 = perf_counter_ns()
        ctx.enqueue_function[gemm_q8_0_wmma_16x64](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xqd.unsafe_ptr(), xdd.unsafe_ptr(),
            xsm.unsafe_ptr(), n_cols, n_rows, n_tokens,
            grid_dim=((n_rows + 63) // 64, (n_tokens + 15) // 16), block_dim=128,
        )
    ctx.synchronize()
    small_s = Float64(perf_counter_ns() - t2) / 1e9 / Float64(ITERS)

    # Kafel dot4 128x128: TM=8, TN=4, KB=2, blok (128/8)*(128/4) = 512 wątków.
    dg_x = (n_rows + 127) // 128
    dg_y = (n_tokens + 127) // 128
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t1 = perf_counter_ns()
        ctx.enqueue_function[gemm_q8_0_dot4_128x128](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xqd.unsafe_ptr(), xdd.unsafe_ptr(),
            xsm.unsafe_ptr(), n_cols, n_rows, n_tokens,
            grid_dim=(dg_x, dg_y), block_dim=512,
        )
    ctx.synchronize()
    dot_s = Float64(perf_counter_ns() - t1) / 1e9 / Float64(ITERS)

    best = big_s
    if small_s < best:
        best = small_s
    print(
        "T=", n_tokens, "rows=", n_rows, "cols=", n_cols,
        "| wmma 64x128", Int(ops / big_s / 1e12),
        "| wmma 16x64", Int(ops / small_s / 1e12),
        "| dot4", Int(ops / dot_s / 1e12), "TOPS",
        "| najlepszy wmma / dot4", Int(dot_s / best * 100.0), "%",
    )


def main() raises:
    var ctx = DeviceContext()
    # Kształty projekcji z realnych modeli: 7B (4096) i mniejszy hybrydowy (1024).
    run(ctx, 128, 4096, 4096)
    run(ctx, 512, 4096, 4096)
    run(ctx, 1024, 4096, 4096)
    run(ctx, 1024, 1024, 1024)
    run(ctx, 2048, 4096, 4096)
