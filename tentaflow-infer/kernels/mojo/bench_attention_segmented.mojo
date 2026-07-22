# =============================================================================
# Plik: bench_attention_segmented.mojo
# Opis: Porównuje przenośną i jednowarpową atencję segmentowaną dla MTP B2.
# Przykład: pixi run mojo bench_attention_segmented.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.math import sqrt
from std.time import perf_counter_ns
from src.attention import (
    attn_verify_segmented_f16_hd256,
    attn_verify_segmented_f16_hd256_warp32,
)

comptime BATCH = 2
comptime TOKENS = 4
comptime Q_HEADS = 24
comptime KV_HEADS = 4
comptime HEAD_DIM = 256
comptime PAGE_SIZE = 32
comptime MAX_PAGES = 64
comptime WARMUP = 10
comptime ITERS = 30
comptime SAMPLES = 7


def median(mut values: InlineArray[Float64, SAMPLES]) -> Float64:
    for i in range(1, SAMPLES):
        value = values[i]
        var j = i
        while j > 0 and values[j - 1] > value:
            values[j] = values[j - 1]
            j -= 1
        values[j] = value
    return values[SAMPLES // 2]


def run_context[context: Int](ctx: DeviceContext) raises:
    comptime total = BATCH * TOKENS
    comptime query_elements = total * Q_HEADS * HEAD_DIM
    comptime cache_pages = BATCH * MAX_PAGES
    comptime cache_elements = cache_pages * KV_HEADS * PAGE_SIZE * HEAD_DIM
    var q = ctx.enqueue_create_buffer[DType.float16](query_elements)
    var k = ctx.enqueue_create_buffer[DType.float16](cache_elements)
    var v = ctx.enqueue_create_buffer[DType.float16](cache_elements)
    var page_tables = ctx.enqueue_create_buffer[DType.int32](BATCH * MAX_PAGES)
    var visible = ctx.enqueue_create_buffer[DType.int32](total)
    var portable = ctx.enqueue_create_buffer[DType.float16](query_elements)
    var warp32 = ctx.enqueue_create_buffer[DType.float16](query_elements)

    with q.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32((i * 13 % 29) - 14) * 0.0078125)
    with k.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32((i * 17 % 31) - 15) * 0.00390625)
    with v.map_to_host() as values:
        for i in range(len(values)):
            page = i // (KV_HEADS * PAGE_SIZE * HEAD_DIM)
            sequence = page // MAX_PAGES
            values[i] = Float16(
                Float32((i * 19 % 37) - 18) * 0.00390625
                + Float32(sequence) * 0.25
            )
    with page_tables.map_to_host() as values:
        for sequence in range(BATCH):
            for page in range(MAX_PAGES):
                values[sequence * MAX_PAGES + page] = Int32(
                    sequence * MAX_PAGES + page
                )
    with visible.map_to_host() as values:
        for sequence in range(BATCH):
            for token in range(TOKENS):
                values[sequence * TOKENS + token] = Int32(
                    context - TOKENS + token + 1
                )

    for _ in range(WARMUP):
        ctx.enqueue_function[attn_verify_segmented_f16_hd256](
            portable.unsafe_ptr(), q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(),
            page_tables.unsafe_ptr(), visible.unsafe_ptr(), TOKENS, Q_HEADS,
            KV_HEADS, PAGE_SIZE, MAX_PAGES, Float32(0.0625),
            grid_dim=(total, Q_HEADS), block_dim=256,
        )
        ctx.enqueue_function[attn_verify_segmented_f16_hd256_warp32](
            warp32.unsafe_ptr(), q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(),
            page_tables.unsafe_ptr(), visible.unsafe_ptr(), TOKENS, Q_HEADS,
            KV_HEADS, PAGE_SIZE, MAX_PAGES, Float32(0.0625),
            grid_dim=(total, Q_HEADS), block_dim=32,
        )
    ctx.synchronize()

    var portable_samples = InlineArray[Float64, SAMPLES](fill=0.0)
    var warp_samples = InlineArray[Float64, SAMPLES](fill=0.0)
    for sample in range(SAMPLES):
        var started = perf_counter_ns()
        for _ in range(ITERS):
            ctx.enqueue_function[attn_verify_segmented_f16_hd256](
                portable.unsafe_ptr(), q.unsafe_ptr(), k.unsafe_ptr(),
                v.unsafe_ptr(), page_tables.unsafe_ptr(), visible.unsafe_ptr(),
                TOKENS, Q_HEADS, KV_HEADS, PAGE_SIZE, MAX_PAGES, Float32(0.0625),
                grid_dim=(total, Q_HEADS), block_dim=256,
            )
        ctx.synchronize()
        portable_samples[sample] = Float64(perf_counter_ns() - started) / 1e3 / ITERS
        started = perf_counter_ns()
        for _ in range(ITERS):
            ctx.enqueue_function[attn_verify_segmented_f16_hd256_warp32](
                warp32.unsafe_ptr(), q.unsafe_ptr(), k.unsafe_ptr(),
                v.unsafe_ptr(), page_tables.unsafe_ptr(), visible.unsafe_ptr(),
                TOKENS, Q_HEADS, KV_HEADS, PAGE_SIZE, MAX_PAGES, Float32(0.0625),
                grid_dim=(total, Q_HEADS), block_dim=32,
            )
        ctx.synchronize()
        warp_samples[sample] = Float64(perf_counter_ns() - started) / 1e3 / ITERS

    var error = Float64(0.0)
    var norm = Float64(0.0)
    var maximum = Float64(0.0)
    with portable.map_to_host() as expected, warp32.map_to_host() as actual:
        for i in range(len(expected)):
            delta = Float64(actual[i]) - Float64(expected[i])
            error += delta * delta
            norm += Float64(expected[i]) * Float64(expected[i])
            if abs(delta) > maximum:
                maximum = abs(delta)
    relative = sqrt(error / norm)
    if relative > 0.002:
        raise Error("względny błąd atencji przekracza 0.002")
    print(
        "ctx", context, "portable", median(portable_samples), "us; warp32",
        median(warp_samples), "us; L2", relative, "max", maximum,
    )


def main() raises:
    var ctx = DeviceContext()
    run_context[128](ctx)
    run_context[512](ctx)
    run_context[2048](ctx)
