# =============================================================================
# Plik: bench_nvfp4_gguf_dp4a.mojo
# Opis: Porównuje decode GEMV GGUF NVFP4 w domenie F16 i Q8_1/dp4a dla
#       kształtów targetu Qwen3.6-27B.
# Przykład: pixi run mojo bench_nvfp4_gguf_dp4a.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.gemm import quantize_act_q8_1
from src.nvfp4 import gemv_nvfp4_gguf_f16
from src.nvfp4_gguf_dp4a import gemv_nvfp4_gguf_q8_1_f16

comptime COLS = 5120
comptime ITERS = 100
comptime WARMUP = 200


def run_shape[rows: Int](ctx: DeviceContext) raises:
    var y = ctx.enqueue_create_buffer[DType.float16](rows)
    var x = ctx.enqueue_create_buffer[DType.float16](COLS)
    var xq = ctx.enqueue_create_buffer[DType.int8](COLS)
    var xd = ctx.enqueue_create_buffer[DType.float32](COLS // 32)
    var xsm = ctx.enqueue_create_buffer[DType.float32](COLS // 32)
    var weights = ctx.enqueue_create_buffer[DType.uint8](
        rows * (COLS // 64) * 36
    )
    ctx.enqueue_function[quantize_act_q8_1](
        xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), x.unsafe_ptr(),
        COLS, 1, grid_dim=(COLS // 32 + 255) // 256, block_dim=256,
    )
    ctx.synchronize()
    var started = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[quantize_act_q8_1](
            xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), x.unsafe_ptr(),
            COLS, 1, grid_dim=(COLS // 32 + 255) // 256, block_dim=256,
        )
    ctx.synchronize()
    var quant_ms = Float64(perf_counter_ns() - started) / 1e6 / ITERS
    print(rows, "x", COLS, " q8_1 prepass:", quant_ms, "ms")
    for _ in range(WARMUP):
        ctx.enqueue_function[gemv_nvfp4_gguf_f16](
            y.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), COLS,
            Float32(1.0),
            grid_dim=rows, block_dim=256,
        )
    ctx.synchronize()
    started = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_nvfp4_gguf_f16](
            y.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), COLS,
            Float32(1.0),
            grid_dim=rows, block_dim=256,
        )
    ctx.synchronize()
    var elapsed = Float64(perf_counter_ns() - started) / 1e6 / ITERS
    print(rows, "x", COLS, " f16:", elapsed, "ms")

    for _ in range(WARMUP):
        ctx.enqueue_function[gemv_nvfp4_gguf_q8_1_f16](
            y.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(),
            xd.unsafe_ptr(), COLS, rows, Float32(1.0),
            grid_dim=(rows + 7) // 8, block_dim=256,
        )
    ctx.synchronize()
    started = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_nvfp4_gguf_q8_1_f16](
            y.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(),
            xd.unsafe_ptr(), COLS, rows, Float32(1.0),
            grid_dim=(rows + 7) // 8, block_dim=256,
        )
    ctx.synchronize()
    elapsed = Float64(perf_counter_ns() - started) / 1e6 / ITERS
    print(rows, "x", COLS, " q8_1/dp4a:", elapsed, "ms")


def main() raises:
    var ctx = DeviceContext()
    run_shape[5120](ctx)
    run_shape[17408](ctx)
