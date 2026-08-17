# =============================================================================
# Plik: bench_decode_qwen35.mojo
# Opis: Pasmo kazdego kernela GEMV kroku dekodowania Qwen3.6-27B Q4_K_M na
#       DOKLADNYCH ksztaltach tego checkpointu, plus sweep liczby blokow
#       roboczych przy stalej liczbie bajtow — zeby rozstrzygnac, czy kernele
#       stojace na ~450-570 GB/s ogranicza siatka, czy uklad odczytu.
# Przyklad: pixi run mojo run -I . bench-amd/bench_decode_qwen35.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.decode_dp4a import (
    gemv_q4_k_dp4a_f16,
    gemv_q6_k_dp4a_f16,
    gemv_q4_k_dp4a_group4_f16,
)
from src.gemv2 import gemv_q4_k_f16_v2, gemv_q6_k_f16_v2

comptime ITERS = 30
comptime WARMUP = 5


def q4k_bytes(rows: Int, cols: Int) -> Float64:
    return Float64(rows) * Float64(cols // 256) * 144.0


def q6k_bytes(rows: Int, cols: Int) -> Float64:
    return Float64(rows) * Float64(cols // 256) * 210.0


def bench_q4k_dp4a(label: String, ctx: DeviceContext, rows: Int, cols: Int) raises:
    var y = ctx.enqueue_create_buffer[DType.float16](rows)
    var x = ctx.enqueue_create_buffer[DType.float16](cols)
    var w = ctx.enqueue_create_buffer[DType.uint8](rows * (cols // 256) * 144)
    ctx.synchronize()
    grid = (rows + 7) // 8
    for _ in range(WARMUP):
        ctx.enqueue_function[gemv_q4_k_dp4a_f16](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q4_k_dp4a_f16](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256)
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    b = q4k_bytes(rows, cols)
    print(label, "N=", rows, "K=", cols, "blocks=", grid,
          "->", Int(dt * 1e6), "us", Int(b / 1e6), "MB", Int(b / dt / 1e9), "GB/s")


def bench_q6k_dp4a(label: String, ctx: DeviceContext, rows: Int, cols: Int) raises:
    var y = ctx.enqueue_create_buffer[DType.float16](rows)
    var x = ctx.enqueue_create_buffer[DType.float16](cols)
    var w = ctx.enqueue_create_buffer[DType.uint8](rows * (cols // 256) * 210)
    ctx.synchronize()
    grid = (rows + 7) // 8
    for _ in range(WARMUP):
        ctx.enqueue_function[gemv_q6_k_dp4a_f16](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q6_k_dp4a_f16](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256)
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    b = q6k_bytes(rows, cols)
    print(label, "N=", rows, "K=", cols, "blocks=", grid,
          "->", Int(dt * 1e6), "us", Int(b / 1e6), "MB", Int(b / dt / 1e9), "GB/s")


def bench_q4k_f16(label: String, ctx: DeviceContext, rows: Int, cols: Int) raises:
    var y = ctx.enqueue_create_buffer[DType.float16](rows)
    var x = ctx.enqueue_create_buffer[DType.float16](cols)
    var w = ctx.enqueue_create_buffer[DType.uint8](rows * (cols // 256) * 144)
    ctx.synchronize()
    grid = (rows + 7) // 8
    for _ in range(WARMUP):
        ctx.enqueue_function[gemv_q4_k_f16_v2](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q4_k_f16_v2](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256)
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    b = q4k_bytes(rows, cols)
    print(label, "N=", rows, "K=", cols, "blocks=", grid,
          "->", Int(dt * 1e6), "us", Int(b / 1e6), "MB", Int(b / dt / 1e9), "GB/s")


def bench_q6k_f16(label: String, ctx: DeviceContext, rows: Int, cols: Int) raises:
    var y = ctx.enqueue_create_buffer[DType.float16](rows)
    var x = ctx.enqueue_create_buffer[DType.float16](cols)
    var w = ctx.enqueue_create_buffer[DType.uint8](rows * (cols // 256) * 210)
    ctx.synchronize()
    grid = (rows + 7) // 8
    for _ in range(WARMUP):
        ctx.enqueue_function[gemv_q6_k_f16_v2](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q6_k_f16_v2](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256)
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    b = q6k_bytes(rows, cols)
    print(label, "N=", rows, "K=", cols, "blocks=", grid,
          "->", Int(dt * 1e6), "us", Int(b / 1e6), "MB", Int(b / dt / 1e9), "GB/s")


def bench_group2_q4k(label: String, ctx: DeviceContext, rows: Int, cols: Int) raises:
    """Grupa dwoch projekcji Q4_K na wspolnej aktywacji (gate + up)."""
    var y0 = ctx.enqueue_create_buffer[DType.float16](rows)
    var y1 = ctx.enqueue_create_buffer[DType.float16](rows)
    var y2 = ctx.enqueue_create_buffer[DType.float16](8)
    var y3 = ctx.enqueue_create_buffer[DType.float16](8)
    var x = ctx.enqueue_create_buffer[DType.float16](cols)
    var w0 = ctx.enqueue_create_buffer[DType.uint8](rows * (cols // 256) * 144)
    var w1 = ctx.enqueue_create_buffer[DType.uint8](rows * (cols // 256) * 144)
    var w2 = ctx.enqueue_create_buffer[DType.uint8](8)
    var w3 = ctx.enqueue_create_buffer[DType.uint8](8)
    ctx.synchronize()
    grid = 2 * ((rows + 7) // 8)
    for _ in range(WARMUP):
        ctx.enqueue_function[gemv_q4_k_dp4a_group4_f16](
            y0.unsafe_ptr(), w0.unsafe_ptr(), rows,
            y1.unsafe_ptr(), w1.unsafe_ptr(), rows,
            y2.unsafe_ptr(), w2.unsafe_ptr(), 0,
            y3.unsafe_ptr(), w3.unsafe_ptr(), 0,
            x.unsafe_ptr(), cols,
            grid_dim=grid, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q4_k_dp4a_group4_f16](
            y0.unsafe_ptr(), w0.unsafe_ptr(), rows,
            y1.unsafe_ptr(), w1.unsafe_ptr(), rows,
            y2.unsafe_ptr(), w2.unsafe_ptr(), 0,
            y3.unsafe_ptr(), w3.unsafe_ptr(), 0,
            x.unsafe_ptr(), cols,
            grid_dim=grid, block_dim=256)
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    b = 2.0 * q4k_bytes(rows, cols)
    print(label, "N=2x", rows, "K=", cols, "blocks=", grid,
          "->", Int(dt * 1e6), "us", Int(b / 1e6), "MB", Int(b / dt / 1e9), "GB/s")


def main() raises:
    var ctx = DeviceContext()
    print("arch:", ctx.arch_name())

    print("\n== realne ksztalty kroku dekodowania Qwen3.6-27B Q4_K_M ==")
    bench_group2_q4k("ffn_gate+up  ", ctx, 17408, 5120)
    bench_q6k_f16("ffn_down Q6_K", ctx, 5120, 17408)
    bench_q4k_f16("ffn_down Q4_K", ctx, 5120, 17408)
    bench_q4k_dp4a("ssm_out      ", ctx, 5120, 6144)
    bench_q4k_dp4a("attn_qkv Q4_K", ctx, 10240, 5120)
    bench_q6k_dp4a("attn_qkv Q6_K", ctx, 10240, 5120)
    bench_q4k_dp4a("attn_gate    ", ctx, 6144, 5120)
    bench_q4k_dp4a("attn_q       ", ctx, 12288, 5120)
    bench_q6k_dp4a("lm_head Q6_K ", ctx, 248320, 5120)

    print("\n== sweep liczby blokow przy STALEJ dlugosci wiersza (K=6144, Q4_K) ==")
    bench_q4k_dp4a("rows   5120  ", ctx, 5120, 6144)
    bench_q4k_dp4a("rows  10240  ", ctx, 10240, 6144)
    bench_q4k_dp4a("rows  20480  ", ctx, 20480, 6144)
    bench_q4k_dp4a("rows  40960  ", ctx, 40960, 6144)
    bench_q4k_dp4a("rows  81920  ", ctx, 81920, 6144)

    print("\n== sweep dlugosci wiersza przy STALEJ liczbie bajtow (~17,7 MB, Q4_K) ==")
    bench_q4k_dp4a("K= 1024      ", ctx, 30720, 1024)
    bench_q4k_dp4a("K= 2048      ", ctx, 15360, 2048)
    bench_q4k_dp4a("K= 4096      ", ctx, 7680, 4096)
    bench_q4k_dp4a("K= 6144      ", ctx, 5120, 6144)
    bench_q4k_dp4a("K=12288      ", ctx, 2560, 12288)
