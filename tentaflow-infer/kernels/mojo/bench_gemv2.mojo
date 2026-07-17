# ===== File: bench_gemv2.mojo — GEMV bandwidth for the extended quant formats =====
# Warp-per-row v2 kernels: Q5_K / Q3_K / Q2_K superblocks and legacy
# Q4_0 / Q4_1 / Q5_0 / Q5_1. 300-launch warmup so the GPU reaches boost.

from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.gemv2 import gemv_q5_k_f16_v2, gemv_q3_k_f16_v2, gemv_q2_k_f16_v2
from src.gemv2 import gemv_q4_0_f16_v2, gemv_q4_1_f16_v2
from src.gemv2 import gemv_q5_0_f16_v2, gemv_q5_1_f16_v2

comptime ROWSQ = 45056
comptime COLS = 4096
comptime ITERS = 50


def main() raises:
    var ctx = DeviceContext()
    var y = ctx.enqueue_create_buffer[DType.float16](ROWSQ)
    var x = ctx.enqueue_create_buffer[DType.float16](COLS)

    var w5k = ctx.enqueue_create_buffer[DType.uint8](ROWSQ * (COLS // 256) * 176)
    for _ in range(300):
        ctx.enqueue_function[gemv_q5_k_f16_v2](y.unsafe_ptr(), w5k.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q5_k_f16_v2](y.unsafe_ptr(), w5k.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    print("q5_k gemv v2:", ms, "ms  ", Float64(ROWSQ) * Float64(COLS // 256) * 176.0 / (ms / 1e3) / 1e9, "GB/s")

    var w3k = ctx.enqueue_create_buffer[DType.uint8](ROWSQ * (COLS // 256) * 110)
    for _ in range(300):
        ctx.enqueue_function[gemv_q3_k_f16_v2](y.unsafe_ptr(), w3k.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q3_k_f16_v2](y.unsafe_ptr(), w3k.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    print("q3_k gemv v2:", ms, "ms  ", Float64(ROWSQ) * Float64(COLS // 256) * 110.0 / (ms / 1e3) / 1e9, "GB/s")

    var w2k = ctx.enqueue_create_buffer[DType.uint8](ROWSQ * (COLS // 256) * 84)
    for _ in range(300):
        ctx.enqueue_function[gemv_q2_k_f16_v2](y.unsafe_ptr(), w2k.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q2_k_f16_v2](y.unsafe_ptr(), w2k.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    print("q2_k gemv v2:", ms, "ms  ", Float64(ROWSQ) * Float64(COLS // 256) * 84.0 / (ms / 1e3) / 1e9, "GB/s")

    var w40 = ctx.enqueue_create_buffer[DType.uint8](ROWSQ * (COLS // 32) * 18)
    for _ in range(300):
        ctx.enqueue_function[gemv_q4_0_f16_v2](y.unsafe_ptr(), w40.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q4_0_f16_v2](y.unsafe_ptr(), w40.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    print("q4_0 gemv v2:", ms, "ms  ", Float64(ROWSQ) * Float64(COLS // 32) * 18.0 / (ms / 1e3) / 1e9, "GB/s")

    var w41 = ctx.enqueue_create_buffer[DType.uint8](ROWSQ * (COLS // 32) * 20)
    for _ in range(300):
        ctx.enqueue_function[gemv_q4_1_f16_v2](y.unsafe_ptr(), w41.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q4_1_f16_v2](y.unsafe_ptr(), w41.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    print("q4_1 gemv v2:", ms, "ms  ", Float64(ROWSQ) * Float64(COLS // 32) * 20.0 / (ms / 1e3) / 1e9, "GB/s")

    var w50 = ctx.enqueue_create_buffer[DType.uint8](ROWSQ * (COLS // 32) * 22)
    for _ in range(300):
        ctx.enqueue_function[gemv_q5_0_f16_v2](y.unsafe_ptr(), w50.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q5_0_f16_v2](y.unsafe_ptr(), w50.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    print("q5_0 gemv v2:", ms, "ms  ", Float64(ROWSQ) * Float64(COLS // 32) * 22.0 / (ms / 1e3) / 1e9, "GB/s")

    var w51 = ctx.enqueue_create_buffer[DType.uint8](ROWSQ * (COLS // 32) * 24)
    for _ in range(300):
        ctx.enqueue_function[gemv_q5_1_f16_v2](y.unsafe_ptr(), w51.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q5_1_f16_v2](y.unsafe_ptr(), w51.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    print("q5_1 gemv v2:", ms, "ms  ", Float64(ROWSQ) * Float64(COLS // 32) * 24.0 / (ms / 1e3) / 1e9, "GB/s")
