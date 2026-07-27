# =============================================================================
# Plik: bench_gemv_nvfp4_wave.mojo
# Opis: A/B GEMV NVFP4 dla decode — wariant „workgroup na wiersz" wobec „fala na
#       wiersz", na kształtach projekcji Qwen3.6-27B. Sprawdza też zgodność
#       obu wariantów.
# Przykład: pixi run mojo run -I . bench-amd/bench_gemv_nvfp4_wave.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.random import random_si64, seed
from std.time import perf_counter_ns

from src.nvfp4 import gemv_nvfp4_gguf_f16, gemv_nvfp4_gguf_f16_wave, GEMV_WAVE_ROWS

comptime WARMUP = 3
comptime ITERS = 20


def run(ctx: DeviceContext, n_rows: Int, n_cols: Int) raises:
    blocks = n_cols // 64
    var wb = ctx.enqueue_create_host_buffer[DType.uint8](n_rows * blocks * 36)
    var xb = ctx.enqueue_create_host_buffer[DType.float16](n_cols)
    ctx.synchronize()
    for i in range(n_rows * blocks * 36):
        wb[i] = UInt8(Int(random_si64(0, 255)))
    for i in range(n_cols):
        xb[i] = Float16(Float64(Int(random_si64(-4, 4))) * 0.25)

    var wd = ctx.enqueue_create_buffer[DType.uint8](n_rows * blocks * 36)
    var xd = ctx.enqueue_create_buffer[DType.float16](n_cols)
    var y0 = ctx.enqueue_create_buffer[DType.float16](n_rows)
    var y1 = ctx.enqueue_create_buffer[DType.float16](n_rows)
    ctx.enqueue_copy(wd, wb)
    ctx.enqueue_copy(xd, xb)
    ctx.synchronize()

    scale = Float32(1.0)
    var t0: Int = 0
    var t1: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t0 = perf_counter_ns()
        ctx.enqueue_function[gemv_nvfp4_gguf_f16](
            y0.unsafe_ptr(), wd.unsafe_ptr(), xd.unsafe_ptr(), n_cols, scale,
            grid_dim=(n_rows,), block_dim=256,
        )
    ctx.synchronize()
    block_s = Float64(perf_counter_ns() - t0) / 1e9 / Float64(ITERS)

    grid = (n_rows + GEMV_WAVE_ROWS - 1) // GEMV_WAVE_ROWS
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t1 = perf_counter_ns()
        ctx.enqueue_function[gemv_nvfp4_gguf_f16_wave](
            y1.unsafe_ptr(), wd.unsafe_ptr(), xd.unsafe_ptr(), n_cols, n_rows, scale,
            grid_dim=(grid,), block_dim=GEMV_WAVE_ROWS * 32,
        )
    ctx.synchronize()
    wave_s = Float64(perf_counter_ns() - t1) / 1e9 / Float64(ITERS)

    var h0 = ctx.enqueue_create_host_buffer[DType.float16](n_rows)
    var h1 = ctx.enqueue_create_host_buffer[DType.float16](n_rows)
    ctx.enqueue_copy(h0, y0)
    ctx.enqueue_copy(h1, y1)
    ctx.synchronize()
    var worst: Float64 = 0.0
    for r in range(n_rows):
        a = Float64(h0[r])
        b = Float64(h1[r])
        denom = abs(a)
        if denom < 1.0:
            denom = 1.0
        rel = abs(a - b) / denom
        if rel > worst:
            worst = rel
    if worst > 5e-3:
        raise Error("warianty GEMV NVFP4 nie zgadzaja sie: " + String(worst))

    bytes = Float64(n_rows * blocks * 36)
    print(
        "rows=", n_rows, "cols=", n_cols,
        "| blok/wiersz", Int(block_s * 1e6), "us =", Int(bytes / block_s / 1e9), "GB/s",
        "| fala/wiersz", Int(wave_s * 1e6), "us =", Int(bytes / wave_s / 1e9), "GB/s",
        "| przyspieszenie", Int(block_s / wave_s * 100.0), "%",
    )


def main() raises:
    seed(20260727)
    var ctx = DeviceContext()
    # Kształty projekcji Qwen3.6-27B: FFN gate/up, FFN down, QKV/O.
    run(ctx, 17408, 5120)
    run(ctx, 5120, 17408)
    run(ctx, 5120, 5120)
    run(ctx, 6144, 5120)
