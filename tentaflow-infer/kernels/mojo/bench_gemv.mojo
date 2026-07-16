from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.nvfp4 import gemv_nvfp4_f16
from src.gemv import gemv_q8_0_f16

def main() raises:
    var ctx = DeviceContext()
    comptime ROWS = 11264
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

    comptime ROWSQ = 11264
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
