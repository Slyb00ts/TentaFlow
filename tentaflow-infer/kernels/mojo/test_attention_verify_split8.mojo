# =============================================================================
# Plik: test_attention_verify_split8.mojo
# Opis: Weryfikuje i mierzy izolowany split8 verifiera T3/T4 na stronicowanym KV.
# Przykład: pixi run mojo bench_attention_verify_split8_probe.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.math import abs, sqrt
from std.time import perf_counter_ns
from src.attention import attn_decode_batch_exact_f16_hd256
from src.attention_verify_split8 import (
    PARTIAL_STRIDE,
    SPLITS,
    attn_verify_split8_combine_f16_hd256,
    attn_verify_split8_f16_hd256_t3,
    attn_verify_split8_f16_hd256_t4,
)

comptime BATCH = 2
comptime Q_HEADS = 24
comptime KV_HEADS = 4
comptime HEAD_DIM = 256
comptime PAGE_SIZE = 32
comptime MAX_PAGES = 136
comptime BLOCK = 256
comptime WARMUP = 20
comptime ITERS = 100
comptime GUARD = 64
comptime CANARY = Float16(17.5)
comptime REL_L2_LIMIT: Float64 = 1e-4
comptime MAX_ULP_LIMIT = 16


def _fill[tokens: Int](
    mut query: DeviceBuffer[DType.float16],
    mut key: DeviceBuffer[DType.float16],
    mut value: DeviceBuffer[DType.float16],
    mut pages: DeviceBuffer[DType.int32],
) raises:
    with query.map_to_host() as host:
        for index in range(len(host)):
            host[index] = Float16(
                Float32((index * 37 + 3) % 61 - 30) / 128.0
            )
    with key.map_to_host() as keys, value.map_to_host() as values:
        for index in range(len(keys)):
            keys[index] = Float16(
                Float32((index * 17 + 5) % 67 - 33) / 128.0
            )
            values[index] = Float16(
                Float32((index * 29 + 7) % 71 - 35) / 64.0
            )
    with pages.map_to_host() as host:
        for sequence in range(BATCH):
            for logical in range(MAX_PAGES):
                host[sequence * MAX_PAGES + logical] = Int32(
                    logical * BATCH + (1 - sequence)
                )


def _prepare(mut output: DeviceBuffer[DType.float16]) raises:
    with output.map_to_host() as host:
        for index in range(len(host)):
            host[index] = CANARY


def _prepare_partial(mut partial: DeviceBuffer[DType.float32]) raises:
    with partial.map_to_host() as host:
        for index in range(len(host)):
            host[index] = Float32(CANARY)


def _launch_reference[tokens: Int](
    ctx: DeviceContext,
    mut output: DeviceBuffer[DType.float16],
    mut query: DeviceBuffer[DType.float16],
    mut key: DeviceBuffer[DType.float16],
    mut value: DeviceBuffer[DType.float16],
    mut pages: DeviceBuffer[DType.int32],
    mut lengths: DeviceBuffer[DType.int32],
) raises:
    comptime for sequence in range(BATCH):
        ctx.enqueue_function[attn_decode_batch_exact_f16_hd256](
            output.unsafe_ptr()
            + GUARD
            + sequence * tokens * Q_HEADS * HEAD_DIM,
            query.unsafe_ptr()
            + sequence * tokens * Q_HEADS * HEAD_DIM,
            key.unsafe_ptr(),
            value.unsafe_ptr(),
            pages.unsafe_ptr() + sequence * MAX_PAGES,
            lengths.unsafe_ptr() + sequence * tokens,
            Q_HEADS,
            KV_HEADS,
            PAGE_SIZE,
            MAX_PAGES,
            Float32(1.0) / sqrt(Float32(HEAD_DIM)),
            grid_dim=(tokens, Q_HEADS),
            block_dim=128,
        )


def _launch_probe[tokens: Int](
    ctx: DeviceContext,
    mut output: DeviceBuffer[DType.float16],
    mut partial: DeviceBuffer[DType.float32],
    mut query: DeviceBuffer[DType.float16],
    mut key: DeviceBuffer[DType.float16],
    mut value: DeviceBuffer[DType.float16],
    mut pages: DeviceBuffer[DType.int32],
    mut lengths: DeviceBuffer[DType.int32],
) raises:
    comptime if tokens == 3:
        ctx.enqueue_function[attn_verify_split8_f16_hd256_t3](
            partial.unsafe_ptr() + GUARD,
            query.unsafe_ptr(),
            key.unsafe_ptr(),
            value.unsafe_ptr(),
            pages.unsafe_ptr(),
            lengths.unsafe_ptr(),
            Q_HEADS,
            KV_HEADS,
            PAGE_SIZE,
            MAX_PAGES,
            Float32(1.0) / sqrt(Float32(HEAD_DIM)),
            grid_dim=(BATCH, Q_HEADS, SPLITS),
            block_dim=BLOCK,
        )
        ctx.enqueue_function[attn_verify_split8_combine_f16_hd256](
            output.unsafe_ptr() + GUARD,
            partial.unsafe_ptr() + GUARD,
            tokens,
            Q_HEADS,
            grid_dim=(BATCH, tokens * Q_HEADS),
            block_dim=32,
        )
    else:
        ctx.enqueue_function[attn_verify_split8_f16_hd256_t4](
            partial.unsafe_ptr() + GUARD,
            query.unsafe_ptr(),
            key.unsafe_ptr(),
            value.unsafe_ptr(),
            pages.unsafe_ptr(),
            lengths.unsafe_ptr(),
            Q_HEADS,
            KV_HEADS,
            PAGE_SIZE,
            MAX_PAGES,
            Float32(1.0) / sqrt(Float32(HEAD_DIM)),
            grid_dim=(BATCH, Q_HEADS, SPLITS),
            block_dim=BLOCK,
        )
        ctx.enqueue_function[attn_verify_split8_combine_f16_hd256](
            output.unsafe_ptr() + GUARD,
            partial.unsafe_ptr() + GUARD,
            tokens,
            Q_HEADS,
            grid_dim=(BATCH, tokens * Q_HEADS),
            block_dim=32,
        )


def _ulp_distance(left: Float16, right: Float16) -> Int:
    left_bits = Int(left.to_bits())
    right_bits = Int(right.to_bits())
    if ((left_bits ^ right_bits) & 0x8000) == 0:
        return abs(left_bits - right_bits)
    return (left_bits & 0x7fff) + (right_bits & 0x7fff)


def _verify[tokens: Int, context: Int](
    ctx: DeviceContext,
    mut reference: DeviceBuffer[DType.float16],
    mut probe: DeviceBuffer[DType.float16],
    mut partial: DeviceBuffer[DType.float32],
    mut query: DeviceBuffer[DType.float16],
    mut key: DeviceBuffer[DType.float16],
    mut value: DeviceBuffer[DType.float16],
    mut pages: DeviceBuffer[DType.int32],
    mut lengths: DeviceBuffer[DType.int32],
) raises:
    _prepare(reference)
    _prepare(probe)
    _prepare_partial(partial)
    with lengths.map_to_host() as host:
        for sequence in range(BATCH):
            for token in range(tokens):
                host[sequence * tokens + token] = Int32(
                    context - tokens + token + 1
                )
    _launch_reference[tokens](
        ctx, reference, query, key, value, pages, lengths
    )
    _launch_probe[tokens](
        ctx, probe, partial, query, key, value, pages, lengths
    )
    ctx.synchronize()

    comptime elements = BATCH * tokens * Q_HEADS * HEAD_DIM
    var reference_norm: Float64 = 0.0
    var error_norm: Float64 = 0.0
    var maximum_ulp = 0
    with reference.map_to_host() as expected, probe.map_to_host() as actual:
        for index in range(GUARD):
            if (
                expected[index].to_bits() != CANARY.to_bits()
                or actual[index].to_bits() != CANARY.to_bits()
            ):
                raise Error("naruszony początkowy canary")
        for index in range(elements):
            expected_value = expected[GUARD + index]
            actual_value = actual[GUARD + index]
            difference = Float64(expected_value) - Float64(actual_value)
            reference_norm += Float64(expected_value) * Float64(expected_value)
            error_norm += difference * difference
            maximum_ulp = max(
                maximum_ulp, _ulp_distance(expected_value, actual_value)
            )
        for index in range(GUARD + elements, 2 * GUARD + elements):
            if (
                expected[index].to_bits() != CANARY.to_bits()
                or actual[index].to_bits() != CANARY.to_bits()
            ):
                raise Error("naruszony końcowy canary")
    comptime partial_elements = elements // HEAD_DIM * SPLITS * PARTIAL_STRIDE
    with partial.map_to_host() as host:
        for index in range(GUARD):
            if host[index] != Float32(CANARY):
                raise Error("naruszony początkowy canary partial")
        for index in range(GUARD + partial_elements, 2 * GUARD + partial_elements):
            if host[index] != Float32(CANARY):
                raise Error("naruszony końcowy canary partial")
    relative_l2 = sqrt(error_norm / reference_norm)
    if relative_l2 > REL_L2_LIMIT:
        raise Error("przekroczona tolerancja względna L2")
    if maximum_ulp > MAX_ULP_LIMIT:
        raise Error("przekroczona tolerancja ULP")
    print(
        "PASS T=", tokens, " ctx=", context,
        " rel_l2=", relative_l2, " max_ulp=", maximum_ulp,
    )


def _verify_future_invariance[tokens: Int, context: Int](
    ctx: DeviceContext,
    mut before: DeviceBuffer[DType.float16],
    mut after: DeviceBuffer[DType.float16],
    mut partial: DeviceBuffer[DType.float32],
    mut query: DeviceBuffer[DType.float16],
    mut key: DeviceBuffer[DType.float16],
    mut value: DeviceBuffer[DType.float16],
    mut pages: DeviceBuffer[DType.int32],
    mut lengths: DeviceBuffer[DType.int32],
) raises:
    _prepare(before)
    _prepare(after)
    _launch_probe[tokens](
        ctx, before, partial, query, key, value, pages, lengths
    )
    ctx.synchronize()
    future_position = context - tokens + 1
    with key.map_to_host() as host:
        for sequence in range(BATCH):
            physical_page = (
                (future_position // PAGE_SIZE) * BATCH + (1 - sequence)
            )
            page_offset = future_position % PAGE_SIZE
            for kv_head in range(KV_HEADS):
                base = (
                    (physical_page * KV_HEADS + kv_head) * PAGE_SIZE
                    + page_offset
                ) * HEAD_DIM
                for element in range(HEAD_DIM):
                    host[base + element] = Float16(
                        Float32(host[base + element]) - 0.5
                    )
    with value.map_to_host() as host:
        for sequence in range(BATCH):
            physical_page = (
                (future_position // PAGE_SIZE) * BATCH + (1 - sequence)
            )
            page_offset = future_position % PAGE_SIZE
            for kv_head in range(KV_HEADS):
                base = (
                    (physical_page * KV_HEADS + kv_head) * PAGE_SIZE
                    + page_offset
                ) * HEAD_DIM
                for element in range(HEAD_DIM):
                    host[base + element] = Float16(
                        Float32(host[base + element]) + 1.0
                    )
    _launch_probe[tokens](
        ctx, after, partial, query, key, value, pages, lengths
    )
    ctx.synchronize()

    comptime token_elements = Q_HEADS * HEAD_DIM
    var later_changed = False
    with before.map_to_host() as original, after.map_to_host() as mutated:
        for sequence in range(BATCH):
            sequence_base = GUARD + sequence * tokens * token_elements
            for index in range(token_elements):
                if (
                    original[sequence_base + index].to_bits()
                    != mutated[sequence_base + index].to_bits()
                ):
                    raise Error("przyszły KV zmienił wcześniejszy token")
            for index in range(token_elements, tokens * token_elements):
                if (
                    original[sequence_base + index].to_bits()
                    != mutated[sequence_base + index].to_bits()
                ):
                    later_changed = True
    if not later_changed:
        raise Error("mutacja przyszłego KV nie wpłynęła na późniejsze tokeny")
    print("PASS causal T=", tokens, " ctx=", context)


def _benchmark[tokens: Int, context: Int](
    ctx: DeviceContext,
    mut reference: DeviceBuffer[DType.float16],
    mut probe: DeviceBuffer[DType.float16],
    mut partial: DeviceBuffer[DType.float32],
    mut query: DeviceBuffer[DType.float16],
    mut key: DeviceBuffer[DType.float16],
    mut value: DeviceBuffer[DType.float16],
    mut pages: DeviceBuffer[DType.int32],
    mut lengths: DeviceBuffer[DType.int32],
) raises:
    for _ in range(WARMUP):
        _launch_reference[tokens](
            ctx, reference, query, key, value, pages, lengths
        )
        _launch_probe[tokens](
            ctx, probe, partial, query, key, value, pages, lengths
        )
    ctx.synchronize()
    started = perf_counter_ns()
    for _ in range(ITERS):
        _launch_reference[tokens](
            ctx, reference, query, key, value, pages, lengths
        )
    ctx.synchronize()
    reference_us = Float64(perf_counter_ns() - started) / 1e3 / ITERS
    started = perf_counter_ns()
    for _ in range(ITERS):
        _launch_probe[tokens](
            ctx, probe, partial, query, key, value, pages, lengths
        )
    ctx.synchronize()
    probe_us = Float64(perf_counter_ns() - started) / 1e3 / ITERS
    print(
        "BENCH T=", tokens, " ctx=", context,
        " exact_us=", reference_us, " split8_us=", probe_us,
        " speedup=", reference_us / probe_us,
    )


def _case[tokens: Int, context: Int](ctx: DeviceContext) raises:
    comptime elements = BATCH * tokens * Q_HEADS * HEAD_DIM
    comptime cache_elements = (
        BATCH * MAX_PAGES * KV_HEADS * PAGE_SIZE * HEAD_DIM
    )
    var query = ctx.enqueue_create_buffer[DType.float16](elements)
    var key = ctx.enqueue_create_buffer[DType.float16](cache_elements)
    var value = ctx.enqueue_create_buffer[DType.float16](cache_elements)
    var pages = ctx.enqueue_create_buffer[DType.int32](BATCH * MAX_PAGES)
    var lengths = ctx.enqueue_create_buffer[DType.int32](BATCH * tokens)
    var reference = ctx.enqueue_create_buffer[DType.float16](
        elements + 2 * GUARD
    )
    var probe = ctx.enqueue_create_buffer[DType.float16](
        elements + 2 * GUARD
    )
    var partial = ctx.enqueue_create_buffer[DType.float32](
        elements // HEAD_DIM * SPLITS * PARTIAL_STRIDE + 2 * GUARD
    )
    _fill[tokens](query, key, value, pages)
    _verify[tokens, context](
        ctx, reference, probe, partial, query, key, value, pages, lengths
    )
    _verify_future_invariance[tokens, context](
        ctx, reference, probe, partial, query, key, value, pages, lengths
    )
    _benchmark[tokens, context](
        ctx, reference, probe, partial, query, key, value, pages, lengths
    )


def main() raises:
    var ctx = DeviceContext()
    _case[3, 2049](ctx)
    _case[4, 2049](ctx)
    _case[3, 2174](ctx)
    _case[4, 2174](ctx)
    _case[3, 4096](ctx)
    _case[4, 4096](ctx)
