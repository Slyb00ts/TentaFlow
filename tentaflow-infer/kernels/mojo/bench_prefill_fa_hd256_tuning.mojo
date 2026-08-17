# =============================================================================
# Plik: bench_prefill_fa_hd256_tuning.mojo
# Opis: Porownuje bazowy i zoptymalizowany Flash Attention HD256 dla Qwen3.6.
# Przyklad: pixi run mojo bench_prefill_fa_hd256_tuning.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.math import sqrt
from std.time import perf_counter_ns
from src.prefill_fa_hd256 import (
    attn_prefill_fa_mojo_device_pos_f16_hd256,
    attn_prefill_fa_mojo_device_pos_f16_hd256_vtrans,
)

comptime HD = 256
comptime NQH = 24
comptime NKVH = 4
comptime PAGE = 32
comptime WARMUP = 30
comptime ITERS = 50


def _bench_case[tokens: Int, base: Int](ctx: DeviceContext) raises:
    comptime positions = base + tokens
    comptime pages = (positions + PAGE - 1) // PAGE
    comptime cache_elements = pages * NKVH * PAGE * HD
    comptime output_elements = tokens * NQH * HD
    var key_cache = ctx.enqueue_create_buffer[DType.float16](cache_elements)
    var value_cache = ctx.enqueue_create_buffer[DType.float16](cache_elements)
    var page_table = ctx.enqueue_create_buffer[DType.int32](pages)
    var query = ctx.enqueue_create_buffer[DType.float16](output_elements)
    var output = ctx.enqueue_create_buffer[DType.float16](output_elements)
    var base_position = ctx.enqueue_create_buffer[DType.int32](1)

    with page_table.map_to_host() as host:
        for page in range(pages):
            host[page] = Int32(page)
    with base_position.map_to_host() as host:
        host[0] = Int32(base)
    scale = Float32(1.0) / sqrt(Float32(HD))

    for _ in range(WARMUP):
        ctx.enqueue_function[attn_prefill_fa_mojo_device_pos_f16_hd256](
            output.unsafe_ptr(), query.unsafe_ptr(), key_cache.unsafe_ptr(),
            value_cache.unsafe_ptr(), page_table.unsafe_ptr(), base_position.unsafe_ptr(), NQH, NKVH,
            PAGE, scale, tokens,
            grid_dim=((tokens + 63) // 64, NQH), block_dim=128,
        )
        ctx.enqueue_function[attn_prefill_fa_mojo_device_pos_f16_hd256_vtrans](
            output.unsafe_ptr(), query.unsafe_ptr(), key_cache.unsafe_ptr(),
            value_cache.unsafe_ptr(), page_table.unsafe_ptr(), base_position.unsafe_ptr(), NQH, NKVH,
            PAGE, scale, tokens,
            grid_dim=((tokens + 63) // 64, NQH), block_dim=128,
        )
    ctx.synchronize()

    start = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[attn_prefill_fa_mojo_device_pos_f16_hd256](
            output.unsafe_ptr(), query.unsafe_ptr(), key_cache.unsafe_ptr(),
            value_cache.unsafe_ptr(), page_table.unsafe_ptr(), base_position.unsafe_ptr(), NQH, NKVH,
            PAGE, scale, tokens,
            grid_dim=((tokens + 63) // 64, NQH), block_dim=128,
        )
    ctx.synchronize()
    base_ms = Float64(perf_counter_ns() - start) / 1e6 / ITERS

    start = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[attn_prefill_fa_mojo_device_pos_f16_hd256_vtrans](
            output.unsafe_ptr(), query.unsafe_ptr(), key_cache.unsafe_ptr(),
            value_cache.unsafe_ptr(), page_table.unsafe_ptr(), base_position.unsafe_ptr(), NQH, NKVH,
            PAGE, scale, tokens,
            grid_dim=((tokens + 63) // 64, NQH), block_dim=128,
        )
    ctx.synchronize()
    tuned_ms = Float64(perf_counter_ns() - start) / 1e6 / ITERS

    print(
        "T", tokens, "base", base, "bazowy", base_ms, "ms",
        "V-transpose", tuned_ms, "ms", "przyspieszenie", base_ms / tuned_ms,
    )


def main() raises:
    var ctx = DeviceContext()
    _bench_case[1024, 0](ctx)
    _bench_case[1024, 1024](ctx)
    _bench_case[2048, 0](ctx)
