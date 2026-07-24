# =============================================================================
# Plik: test_attention_decode_hd256_split8.mojo
# Opis: Sprawdza zgodność produkcyjnego flash-decode split8 HD256 z referencją.
# Przykład: pixi run mojo test_attention_decode_hd256_split8.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.math import abs, sqrt
from src.attention import (
    attn_decode_f16,
    attn_decode_split8_combine_f16_hd256,
    attn_decode_split8_f16_hd256,
)

comptime NQH = 24
comptime NKVH = 4
comptime NSEQ = 2
comptime HD = 256
comptime PAGE = 32
comptime MAX_PAGES = 136
comptime PARTITIONS = 8
comptime PARTIAL_STRIDE = 260
comptime GUARD = 40
comptime GUARD_VALUE = Float16(19.25)
comptime REL_L2_LIMIT: Float64 = 1e-4
comptime MAX_ULP_LIMIT = 16
comptime attn_decode_f16_hd256_reference = attn_decode_f16[256]


def _fill(
    mut query: DeviceBuffer[DType.float16],
    mut key: DeviceBuffer[DType.float16],
    mut value: DeviceBuffer[DType.float16],
    mut pages: DeviceBuffer[DType.int32],
) raises:
    with query.map_to_host() as host:
        for index in range(len(host)):
            host[index] = Float16(Float32((index * 37 + 3) % 41 - 20) * 0.0078125)
    with key.map_to_host() as keys, value.map_to_host() as values:
        for index in range(len(keys)):
            keys[index] = Float16(Float32((index * 17 + 5) % 47 - 23) * 0.00390625)
            values[index] = Float16(Float32((index * 29 + 7) % 43 - 21) * 0.00390625)
    with pages.map_to_host() as host:
        for index in range(len(host)):
            host[index] = Int32(index % MAX_PAGES)


def _prepare_output(mut output: DeviceBuffer[DType.float16]) raises:
    with output.map_to_host() as host:
        for index in range(len(host)):
            host[index] = GUARD_VALUE


def _launch_reference(
    ctx: DeviceContext,
    mut output: DeviceBuffer[DType.float16],
    mut query: DeviceBuffer[DType.float16],
    mut key: DeviceBuffer[DType.float16],
    mut value: DeviceBuffer[DType.float16],
    mut pages: DeviceBuffer[DType.int32],
    mut lengths: DeviceBuffer[DType.int32],
) raises:
    ctx.enqueue_function[attn_decode_f16_hd256_reference](
        output.unsafe_ptr() + GUARD, query.unsafe_ptr(), key.unsafe_ptr(),
        value.unsafe_ptr(), pages.unsafe_ptr(), lengths.unsafe_ptr(),
        NQH, NKVH, PAGE, MAX_PAGES, Float32(1.0) / sqrt(Float32(HD)),
        grid_dim=(NSEQ, NQH), block_dim=128,
    )


def _launch_split8(
    ctx: DeviceContext,
    mut output: DeviceBuffer[DType.float16],
    mut partial: DeviceBuffer[DType.float32],
    mut query: DeviceBuffer[DType.float16],
    mut key: DeviceBuffer[DType.float16],
    mut value: DeviceBuffer[DType.float16],
    mut pages: DeviceBuffer[DType.int32],
    mut lengths: DeviceBuffer[DType.int32],
) raises:
    ctx.enqueue_function[attn_decode_split8_f16_hd256](
        partial.unsafe_ptr(), query.unsafe_ptr(), key.unsafe_ptr(),
        value.unsafe_ptr(), pages.unsafe_ptr(), lengths.unsafe_ptr(),
        NQH, NKVH, PAGE, MAX_PAGES, Float32(1.0) / sqrt(Float32(HD)),
        grid_dim=(NSEQ, NQH, PARTITIONS), block_dim=256,
    )
    ctx.enqueue_function[attn_decode_split8_combine_f16_hd256](
        output.unsafe_ptr() + GUARD, partial.unsafe_ptr(), NQH,
        grid_dim=(NSEQ, NQH), block_dim=32,
    )


def _ulp_distance(left: Float16, right: Float16) -> Int:
    left_bits = Int(left.to_bits())
    right_bits = Int(right.to_bits())
    if ((left_bits ^ right_bits) & 0x8000) == 0:
        return abs(left_bits - right_bits)
    return (left_bits & 0x7fff) + (right_bits & 0x7fff)


def _verify[first_context: Int, second_context: Int](
    ctx: DeviceContext,
    mut reference: DeviceBuffer[DType.float16],
    mut split8: DeviceBuffer[DType.float16],
    mut partial: DeviceBuffer[DType.float32],
    mut query: DeviceBuffer[DType.float16],
    mut key: DeviceBuffer[DType.float16],
    mut value: DeviceBuffer[DType.float16],
    mut pages: DeviceBuffer[DType.int32],
    mut lengths: DeviceBuffer[DType.int32],
) raises:
    _prepare_output(reference)
    _prepare_output(split8)
    with lengths.map_to_host() as host:
        host[0] = Int32(first_context)
        host[1] = Int32(second_context)
    _launch_reference(ctx, reference, query, key, value, pages, lengths)
    _launch_split8(ctx, split8, partial, query, key, value, pages, lengths)
    ctx.synchronize()

    var reference_norm: Float64 = 0.0
    var error_norm: Float64 = 0.0
    var maximum_ulp = 0
    var bitwise = 0
    with reference.map_to_host() as expected, split8.map_to_host() as actual:
        for index in range(GUARD):
            if expected[index].to_bits() != GUARD_VALUE.to_bits() or actual[index].to_bits() != GUARD_VALUE.to_bits():
                raise Error("naruszony początkowy canary")
        for index in range(NSEQ * NQH * HD):
            expected_value = expected[GUARD + index]
            actual_value = actual[GUARD + index]
            if expected_value.to_bits() == actual_value.to_bits():
                bitwise += 1
            difference = Float64(expected_value) - Float64(actual_value)
            reference_norm += Float64(expected_value) * Float64(expected_value)
            error_norm += difference * difference
            maximum_ulp = max(
                maximum_ulp, _ulp_distance(expected_value, actual_value)
            )
        for index in range(GUARD + NSEQ * NQH * HD, GUARD * 2 + NSEQ * NQH * HD):
            if expected[index].to_bits() != GUARD_VALUE.to_bits() or actual[index].to_bits() != GUARD_VALUE.to_bits():
                raise Error("naruszony końcowy canary")

    relative_l2 = sqrt(error_norm / reference_norm)
    if relative_l2 > REL_L2_LIMIT:
        raise Error("przekroczona tolerancja względna L2")
    if maximum_ulp > MAX_ULP_LIMIT:
        raise Error("przekroczona tolerancja ULP")
    print(
        "PASS P=", first_context, ",", second_context, " rel_l2=", relative_l2,
        " max_ulp=", maximum_ulp, " bitwise=", bitwise, "/", NSEQ * NQH * HD,
    )


def main() raises:
    var ctx = DeviceContext()
    var query = ctx.enqueue_create_buffer[DType.float16](NSEQ * NQH * HD)
    var key = ctx.enqueue_create_buffer[DType.float16](MAX_PAGES * NKVH * PAGE * HD)
    var value = ctx.enqueue_create_buffer[DType.float16](MAX_PAGES * NKVH * PAGE * HD)
    var pages = ctx.enqueue_create_buffer[DType.int32](NSEQ * MAX_PAGES)
    var lengths = ctx.enqueue_create_buffer[DType.int32](NSEQ)
    var reference = ctx.enqueue_create_buffer[DType.float16](NSEQ * NQH * HD + 2 * GUARD)
    var split8 = ctx.enqueue_create_buffer[DType.float16](NSEQ * NQH * HD + 2 * GUARD)
    var partial = ctx.enqueue_create_buffer[DType.float32](
        NSEQ * NQH * PARTITIONS * PARTIAL_STRIDE
    )
    _fill(query, key, value, pages)
    _verify[1, 31](
        ctx, reference, split8, partial, query, key, value, pages, lengths,
    )
    _verify[33, 128](
        ctx, reference, split8, partial, query, key, value, pages, lengths,
    )
    _verify[2048, 2049](
        ctx, reference, split8, partial, query, key, value, pages, lengths,
    )
    _verify[2174, 4096](
        ctx, reference, split8, partial, query, key, value, pages, lengths,
    )
