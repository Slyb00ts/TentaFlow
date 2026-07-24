# =============================================================================
# Plik: bench_nvfp4_mma_bn128.mojo
# Opis: Porownuje dokladne raw pipeline NVFP4 BN64 i BN128 na ksztaltach modeli.
# Przyklad: pixi run mojo bench_nvfp4_mma_bn128.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.nvfp4_gguf_mma import gemm_nvfp4_gguf_mma_f16_bm128_prefetch
from src.nvfp4_gguf_mma_bn128 import (
    gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1,
    gemm_nvfp4_gguf_mma_f16_bm128_bn128,
)

comptime WARMUP = 5


def _run[rows: Int, cols: Int, tokens: Int, iterations: Int](
    ctx: DeviceContext
) raises:
    var weights = ctx.enqueue_create_buffer[DType.uint8](
        rows * (cols // 64) * 36
    )
    var x = ctx.enqueue_create_buffer[DType.float16](tokens * cols)
    var y = ctx.enqueue_create_buffer[DType.float16](tokens * rows)

    for _ in range(WARMUP):
        ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128_prefetch](
            y.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            tokens, Float32(1.0),
            grid_dim=((rows + 63) // 64, (tokens + 127) // 128), block_dim=256,
        )
        ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128_bn128](
            y.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            tokens, Float32(1.0),
            grid_dim=((rows + 127) // 128, (tokens + 127) // 128), block_dim=256,
        )
        ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1](
            y.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            tokens, Float32(1.0),
            grid_dim=((rows + 63) // 64, (tokens + 127) // 128), block_dim=256,
        )
    ctx.synchronize()

    var started = perf_counter_ns()
    for _ in range(iterations):
        ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128_prefetch](
            y.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            tokens, Float32(1.0),
            grid_dim=((rows + 63) // 64, (tokens + 127) // 128), block_dim=256,
        )
    ctx.synchronize()
    bn64_us = Float64(perf_counter_ns() - started) / 1e3 / Float64(iterations)

    started = perf_counter_ns()
    for _ in range(iterations):
        ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128_bn128](
            y.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            tokens, Float32(1.0),
            grid_dim=((rows + 127) // 128, (tokens + 127) // 128), block_dim=256,
        )
    ctx.synchronize()
    bn128_us = Float64(perf_counter_ns() - started) / 1e3 / Float64(iterations)

    started = perf_counter_ns()
    for _ in range(iterations):
        ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1](
            y.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            tokens, Float32(1.0),
            grid_dim=((rows + 63) // 64, (tokens + 127) // 128), block_dim=256,
        )
    ctx.synchronize()
    sync1_us = Float64(perf_counter_ns() - started) / 1e3 / Float64(iterations)
    print(
        "M=", tokens, "N=", rows, "K=", cols, "BN64", bn64_us,
        "us; sync1", sync1_us, "us; BN128", bn128_us,
        "us; best speedup", bn64_us / min(sync1_us, bn128_us),
    )


def main() raises:
    var ctx = DeviceContext()
    _run[12288, 5120, 128, 30](ctx)
    _run[1024, 5120, 128, 50](ctx)
    _run[5120, 6144, 128, 30](ctx)
    _run[17408, 5120, 128, 20](ctx)
    _run[5120, 17408, 128, 20](ctx)
    _run[12288, 5120, 2048, 6](ctx)
    _run[1024, 5120, 2048, 10](ctx)
    _run[5120, 6144, 2048, 8](ctx)
    _run[17408, 5120, 2048, 5](ctx)
    _run[5120, 17408, 2048, 5](ctx)
