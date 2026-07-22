# =============================================================================
# Plik: bench_nvfp4_gguf_mma.mojo
# Opis: Mierzy produkcyjny GEMM MMA GGUF NVFP4 na kształtach prefill Qwen3.6.
# Przykład: pixi run mojo bench_nvfp4_gguf_mma.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.nvfp4_gguf_mma import (
    gemm_nvfp4_gguf_mma_f16_bm128,
    gemm_nvfp4_gguf_mma_f16_bm128_bn32,
)

comptime TOKENS = 128
comptime ITERS = 40
comptime WARMUP = 10


def run_shape[rows: Int, cols: Int](ctx: DeviceContext) raises:
    var y = ctx.enqueue_create_buffer[DType.float16](TOKENS * rows)
    var y32 = ctx.enqueue_create_buffer[DType.float16](TOKENS * rows)
    var x = ctx.enqueue_create_buffer[DType.float16](TOKENS * cols)
    var weights = ctx.enqueue_create_buffer[DType.uint8](
        rows * (cols // 64) * 36
    )
    for _ in range(WARMUP):
        ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128](
            y.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            TOKENS, Float32(1.0), grid_dim=((rows + 63) // 64, 1),
            block_dim=256,
        )
        ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128_bn32](
            y32.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            TOKENS, Float32(1.0), grid_dim=((rows + 31) // 32, 1),
            block_dim=128,
        )
    ctx.synchronize()
    var started = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128](
            y.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            TOKENS, Float32(1.0), grid_dim=((rows + 63) // 64, 1),
            block_dim=256,
        )
    ctx.synchronize()
    var elapsed64 = Float64(perf_counter_ns() - started) / 1e3 / ITERS
    started = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128_bn32](
            y32.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            TOKENS, Float32(1.0), grid_dim=((rows + 31) // 32, 1),
            block_dim=128,
        )
    ctx.synchronize()
    var elapsed32 = Float64(perf_counter_ns() - started) / 1e3 / ITERS
    print(rows, "x", cols, "T=128 BN64:", elapsed64, "us; BN32:", elapsed32, "us")


def main() raises:
    var ctx = DeviceContext()
    run_shape[12288, 5120](ctx)
    run_shape[1024, 5120](ctx)
    run_shape[5120, 6144](ctx)
    run_shape[17408, 5120](ctx)
    run_shape[5120, 17408](ctx)
