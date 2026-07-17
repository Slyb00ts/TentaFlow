from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.nvfp4 import gemv_nvfp4_f16
from src.gemv import gemv_q8_0_f16, gemv_f16
from src.gemv2 import gemv_q8_0_f16_v2, gemv_nvfp4_f16_v2, gemv_f16_v2

def main() raises:
    var ctx = DeviceContext()
    comptime ROWS = 90112
    comptime COLS = 4096
    var y = ctx.enqueue_create_buffer[DType.float16](ROWS)
    var packed = ctx.enqueue_create_buffer[DType.uint8](ROWS * COLS // 2)
    var scales = ctx.enqueue_create_buffer[DType.uint8](ROWS * COLS // 16)
    var x = ctx.enqueue_create_buffer[DType.float16](COLS)
    # warmup
    for _ in range(3):
        ctx.enqueue_function[gemv_nvfp4_f16](y.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(), x.unsafe_ptr(), COLS, Float32(1.0), grid_dim=ROWS, block_dim=256)
    ctx.synchronize()
    comptime ITERS = 50
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_nvfp4_f16](y.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(), x.unsafe_ptr(), COLS, Float32(1.0), grid_dim=ROWS, block_dim=256)
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bytes_read = Float64(ROWS * COLS) * 0.5 + Float64(ROWS * COLS) / 16.0
    print("nvfp4 gemv:", ms, "ms  ", bytes_read / (ms / 1e3) / 1e9, "GB/s")

    comptime ROWSQ = 45056
    var wq = ctx.enqueue_create_buffer[DType.uint8](ROWSQ * (COLS // 32) * 34)
    for _ in range(3):
        ctx.enqueue_function[gemv_q8_0_f16](y.unsafe_ptr(), wq.unsafe_ptr(), x.unsafe_ptr(), COLS, grid_dim=ROWSQ, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q8_0_f16](y.unsafe_ptr(), wq.unsafe_ptr(), x.unsafe_ptr(), COLS, grid_dim=ROWSQ, block_dim=256)
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bytes_read = Float64(ROWSQ) * Float64(COLS // 32) * 34.0
    print("q8_0 gemv:", ms, "ms  ", bytes_read / (ms / 1e3) / 1e9, "GB/s")

    # ---- v2 variants ----
    for _ in range(3):
        ctx.enqueue_function[gemv_nvfp4_f16_v2](y.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS, Float32(1.0), grid_dim=(ROWS + 7) // 8, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_nvfp4_f16_v2](y.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS, Float32(1.0), grid_dim=(ROWS + 7) // 8, block_dim=256)
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bytes_read = Float64(ROWS * COLS) * 0.5 + Float64(ROWS * COLS) / 16.0
    print("nvfp4 gemv v2:", ms, "ms  ", bytes_read / (ms / 1e3) / 1e9, "GB/s")

    for _ in range(3):
        ctx.enqueue_function[gemv_q8_0_f16_v2](y.unsafe_ptr(), wq.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q8_0_f16_v2](y.unsafe_ptr(), wq.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bytes_read = Float64(ROWSQ) * Float64(COLS // 32) * 34.0
    print("q8_0 gemv v2:", ms, "ms  ", bytes_read / (ms / 1e3) / 1e9, "GB/s")

    # f16 v1 vs v2 (lm_head shape: 151936 x 1024 mimicked by 32000x4096)
    var wf = ctx.enqueue_create_buffer[DType.float16](ROWSQ * COLS)
    for _ in range(3):
        ctx.enqueue_function[gemv_f16](y.unsafe_ptr(), wf.unsafe_ptr(), x.unsafe_ptr(), COLS, grid_dim=ROWSQ, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_f16](y.unsafe_ptr(), wf.unsafe_ptr(), x.unsafe_ptr(), COLS, grid_dim=ROWSQ, block_dim=256)
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bytes_read = Float64(ROWSQ) * Float64(COLS) * 2.0
    print("f16 gemv v1:", ms, "ms  ", bytes_read / (ms / 1e3) / 1e9, "GB/s")

    for _ in range(3):
        ctx.enqueue_function[gemv_f16_v2](y.unsafe_ptr(), wf.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_f16_v2](y.unsafe_ptr(), wf.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWSQ, grid_dim=(ROWSQ + 7) // 8, block_dim=256)
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    print("f16 gemv v2:", ms, "ms  ", bytes_read / (ms / 1e3) / 1e9, "GB/s")
