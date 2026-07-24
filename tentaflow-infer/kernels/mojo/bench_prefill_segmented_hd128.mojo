# =============================================================================
# Plik: bench_prefill_segmented_hd128.mojo
# Opis: Porównuje dokładną i kafelkową segmentowaną atencję dla kształtów Bielika.
# Przykład: pixi run mojo bench_prefill_segmented_hd128.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.math import sqrt
from std.time import perf_counter_ns
from src.attention import attn_verify_segmented_f16_hd128_warp32
from src.prefill import attn_prefill_segmented_f16_hd128

comptime Q_HEADS = 32
comptime KV_HEADS = 8
comptime HEAD_DIM = 128
comptime PAGE_SIZE = 32
comptime MAX_PAGES = 32
comptime CONTEXT = 1024
comptime WARMUP = 3
comptime ITERS = 10


def run_shape[batch: Int, tokens: Int](ctx: DeviceContext) raises:
    comptime total = batch * tokens
    comptime query_elements = total * Q_HEADS * HEAD_DIM
    comptime cache_pages = batch * MAX_PAGES
    comptime cache_elements = cache_pages * KV_HEADS * PAGE_SIZE * HEAD_DIM
    comptime base = CONTEXT - tokens
    var q = ctx.enqueue_create_buffer[DType.float16](query_elements)
    var k = ctx.enqueue_create_buffer[DType.float16](cache_elements)
    var v = ctx.enqueue_create_buffer[DType.float16](cache_elements)
    var page_tables = ctx.enqueue_create_buffer[DType.int32](batch * MAX_PAGES)
    var base_positions = ctx.enqueue_create_buffer[DType.int32](batch)
    var visible = ctx.enqueue_create_buffer[DType.int32](total)
    var exact = ctx.enqueue_create_buffer[DType.float16](query_elements)
    var tiled = ctx.enqueue_create_buffer[DType.float16](query_elements)

    with q.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32((i * 13 % 29) - 14) * 0.0078125)
    with k.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32((i * 17 % 31) - 15) * 0.00390625)
    with v.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32((i * 19 % 37) - 18) * 0.00390625)
    with page_tables.map_to_host() as values:
        for sequence in range(batch):
            for page in range(MAX_PAGES):
                values[sequence * MAX_PAGES + page] = Int32(
                    sequence * MAX_PAGES + page
                )
    with base_positions.map_to_host() as values:
        for sequence in range(batch):
            values[sequence] = Int32(base)
    with visible.map_to_host() as values:
        for sequence in range(batch):
            for token in range(tokens):
                values[sequence * tokens + token] = Int32(base + token + 1)

    for _ in range(WARMUP):
        ctx.enqueue_function[attn_verify_segmented_f16_hd128_warp32](
            exact.unsafe_ptr(), q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(),
            page_tables.unsafe_ptr(), visible.unsafe_ptr(), tokens, Q_HEADS,
            KV_HEADS, PAGE_SIZE, MAX_PAGES, Float32(0.0883883476),
            grid_dim=(total, Q_HEADS), block_dim=128,
        )
        ctx.enqueue_function[attn_prefill_segmented_f16_hd128](
            tiled.unsafe_ptr(), q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(),
            page_tables.unsafe_ptr(), base_positions.unsafe_ptr(), tokens,
            MAX_PAGES, Q_HEADS, KV_HEADS, PAGE_SIZE, Float32(0.0883883476),
            grid_dim=(batch * ((tokens + 15) // 16), Q_HEADS), block_dim=256,
        )
    ctx.synchronize()

    var started = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[attn_verify_segmented_f16_hd128_warp32](
            exact.unsafe_ptr(), q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(),
            page_tables.unsafe_ptr(), visible.unsafe_ptr(), tokens, Q_HEADS,
            KV_HEADS, PAGE_SIZE, MAX_PAGES, Float32(0.0883883476),
            grid_dim=(total, Q_HEADS), block_dim=128,
        )
    ctx.synchronize()
    exact_us = Float64(perf_counter_ns() - started) / 1e3 / ITERS
    started = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[attn_prefill_segmented_f16_hd128](
            tiled.unsafe_ptr(), q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(),
            page_tables.unsafe_ptr(), base_positions.unsafe_ptr(), tokens,
            MAX_PAGES, Q_HEADS, KV_HEADS, PAGE_SIZE, Float32(0.0883883476),
            grid_dim=(batch * ((tokens + 15) // 16), Q_HEADS), block_dim=256,
        )
    ctx.synchronize()
    tiled_us = Float64(perf_counter_ns() - started) / 1e3 / ITERS

    var error = Float64(0.0)
    var norm = Float64(0.0)
    var maximum = Float64(0.0)
    with exact.map_to_host() as expected, tiled.map_to_host() as actual:
        for i in range(len(expected)):
            delta = Float64(actual[i]) - Float64(expected[i])
            error += delta * delta
            norm += Float64(expected[i]) * Float64(expected[i])
            if abs(delta) > maximum:
                maximum = abs(delta)
    relative = sqrt(error / norm)
    if relative > 0.01 or maximum > 0.01:
        raise Error("kafelkowa atencja przekracza tolerancję")
    print(
        "B", batch, "T", tokens, "exact", exact_us, "us; tiled", tiled_us,
        "us; speedup", exact_us / tiled_us, "L2", relative, "max", maximum,
    )


def main() raises:
    var ctx = DeviceContext()
    run_shape[4, 256](ctx)
    run_shape[8, 128](ctx)
    run_shape[16, 64](ctx)
