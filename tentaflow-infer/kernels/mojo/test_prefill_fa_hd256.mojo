# =============================================================================
# Plik: test_prefill_fa_hd256.mojo
# Opis: Porownuje Flash Attention HD256 ze skalarnym wzorcem i chroni canary.
# Przyklad: pixi run mojo test_prefill_fa_hd256.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.math import sqrt
from src.prefill import attn_prefill_f16_hd256, QT
from src.prefill_fa_hd256 import (
    attn_prefill_fa_mojo_f16_hd256,
    attn_prefill_fa_mojo_device_pos_f16_hd256,
    attn_prefill_fa_mojo_f16_hd256_bk32,
    attn_prefill_fa_mojo_f16_hd256_vtrans,
    attn_prefill_fa_mojo_device_pos_f16_hd256_vtrans,
    QUERY_TILE,
)

comptime HD = 256
comptime NKVH = 1
comptime NQH = 2
comptime GUARD = 64
comptime CANARY = Float16(19.0)


def _value(index: Int, salt: Int) -> Float16:
    raw = (index * 37 + salt * 53 + 17) % 251
    return Float16(Float32(raw - 125) * 0.00390625)


def _ordered_f16(value: Float16) -> Int:
    bits = Int(value.to_bits())
    if bits >= 0x8000:
        return 0x8000 - (bits & 0x7FFF)
    return 0x8000 + bits


def _check_case[
    tokens: Int, page_size: Int, base: Int, device_position: Bool
](ctx: DeviceContext) raises:
    comptime pages = (base + tokens + page_size - 1) // page_size
    comptime cache_elements = pages * NKVH * page_size * HD
    comptime output_elements = tokens * NQH * HD
    var key_cache = ctx.enqueue_create_buffer[DType.float16](cache_elements)
    var value_cache = ctx.enqueue_create_buffer[DType.float16](cache_elements)
    var page_table = ctx.enqueue_create_buffer[DType.int32](pages)
    var query = ctx.enqueue_create_buffer[DType.float16](output_elements)
    var scalar = ctx.enqueue_create_buffer[DType.float16](output_elements + 2 * GUARD)
    var flash = ctx.enqueue_create_buffer[DType.float16](output_elements + 2 * GUARD)
    var tuned = ctx.enqueue_create_buffer[DType.float16](output_elements + 2 * GUARD)
    var bk32 = ctx.enqueue_create_buffer[DType.float16](output_elements + 2 * GUARD)
    var base_position = ctx.enqueue_create_buffer[DType.int32](1)

    with page_table.map_to_host() as host:
        for page in range(pages):
            host[page] = Int32(pages - page - 1)
    with base_position.map_to_host() as host:
        host[0] = Int32(base)
    with key_cache.map_to_host() as keys, value_cache.map_to_host() as values:
        for index in range(cache_elements):
            keys[index] = Float16(-7.0)
            values[index] = Float16(7.0)
        for position in range(base + tokens):
            physical_page = pages - position // page_size - 1
            slot = position % page_size
            cache_base = (physical_page * page_size + slot) * HD
            for column in range(HD):
                keys[cache_base + column] = _value(position * HD + column, 3)
                values[cache_base + column] = _value(position * HD + column, 11)
    with query.map_to_host() as host:
        for index in range(output_elements):
            host[index] = _value(index, 23)
    with scalar.map_to_host() as reference, flash.map_to_host() as candidate, tuned.map_to_host() as optimized, bk32.map_to_host() as wider:
        for index in range(output_elements + 2 * GUARD):
            reference[index] = CANARY
            candidate[index] = CANARY
            optimized[index] = CANARY
            wider[index] = CANARY

    scale = Float32(1.0) / sqrt(Float32(HD))
    ctx.enqueue_function[attn_prefill_f16_hd256](
        scalar.unsafe_ptr() + GUARD,
        query.unsafe_ptr(),
        key_cache.unsafe_ptr(),
        value_cache.unsafe_ptr(),
        page_table.unsafe_ptr(),
        base,
        NQH,
        NKVH,
        page_size,
        scale,
        tokens,
        grid_dim=((tokens + QT - 1) // QT, NQH),
        block_dim=256,
    )
    comptime if device_position:
        ctx.enqueue_function[attn_prefill_fa_mojo_device_pos_f16_hd256](
            flash.unsafe_ptr() + GUARD,
            query.unsafe_ptr(),
            key_cache.unsafe_ptr(),
            value_cache.unsafe_ptr(),
            page_table.unsafe_ptr(),
            base_position.unsafe_ptr(),
            NQH,
            NKVH,
            page_size,
            scale,
            tokens,
            grid_dim=((tokens + QUERY_TILE - 1) // QUERY_TILE, NQH),
            block_dim=128,
        )
    else:
        ctx.enqueue_function[attn_prefill_fa_mojo_f16_hd256](
            flash.unsafe_ptr() + GUARD,
            query.unsafe_ptr(),
            key_cache.unsafe_ptr(),
            value_cache.unsafe_ptr(),
            page_table.unsafe_ptr(),
            base,
            NQH,
            NKVH,
            page_size,
            scale,
            tokens,
            grid_dim=((tokens + QUERY_TILE - 1) // QUERY_TILE, NQH),
            block_dim=128,
        )
    comptime if device_position:
        ctx.enqueue_function[attn_prefill_fa_mojo_device_pos_f16_hd256_vtrans](
            tuned.unsafe_ptr() + GUARD,
            query.unsafe_ptr(),
            key_cache.unsafe_ptr(),
            value_cache.unsafe_ptr(),
            page_table.unsafe_ptr(),
            base_position.unsafe_ptr(),
            NQH,
            NKVH,
            page_size,
            scale,
            tokens,
            grid_dim=((tokens + QUERY_TILE - 1) // QUERY_TILE, NQH),
            block_dim=128,
        )
    else:
        ctx.enqueue_function[attn_prefill_fa_mojo_f16_hd256_vtrans](
            tuned.unsafe_ptr() + GUARD,
            query.unsafe_ptr(),
            key_cache.unsafe_ptr(),
            value_cache.unsafe_ptr(),
            page_table.unsafe_ptr(),
            base,
            NQH,
            NKVH,
            page_size,
            scale,
            tokens,
            grid_dim=((tokens + QUERY_TILE - 1) // QUERY_TILE, NQH),
            block_dim=128,
        )
    ctx.enqueue_function[attn_prefill_fa_mojo_f16_hd256_bk32](
        bk32.unsafe_ptr() + GUARD,
        query.unsafe_ptr(),
        key_cache.unsafe_ptr(),
        value_cache.unsafe_ptr(),
        page_table.unsafe_ptr(),
        base,
        NQH,
        NKVH,
        page_size,
        scale,
        tokens,
        grid_dim=((tokens + QUERY_TILE - 1) // QUERY_TILE, NQH),
        block_dim=128,
    )
    ctx.synchronize()

    var max_absolute: Float32 = 0.0
    var mean_absolute: Float32 = 0.0
    var argmax_mismatches = 0
    var max_ulp = 0
    var bk32_max_absolute: Float32 = 0.0
    with scalar.map_to_host() as reference, flash.map_to_host() as candidate, tuned.map_to_host() as optimized, bk32.map_to_host() as wider:
        for index in range(GUARD):
            if reference[index].to_bits() != CANARY.to_bits():
                raise Error("skalarny kernel naruszyl canary przed wynikiem")
            if candidate[index].to_bits() != CANARY.to_bits():
                raise Error("Flash Attention naruszyl canary przed wynikiem")
            if optimized[index].to_bits() != CANARY.to_bits():
                raise Error("zoptymalizowany Flash Attention naruszyl canary przed wynikiem")
            if wider[index].to_bits() != CANARY.to_bits():
                raise Error("Flash Attention BK32 naruszyl canary przed wynikiem")
            suffix = GUARD + output_elements + index
            if reference[suffix].to_bits() != CANARY.to_bits():
                raise Error("skalarny kernel naruszyl canary za wynikiem")
            if candidate[suffix].to_bits() != CANARY.to_bits():
                raise Error("Flash Attention naruszyl canary za wynikiem")
            if optimized[suffix].to_bits() != CANARY.to_bits():
                raise Error("zoptymalizowany Flash Attention naruszyl canary za wynikiem")
            if wider[suffix].to_bits() != CANARY.to_bits():
                raise Error("Flash Attention BK32 naruszyl canary za wynikiem")
        for row in range(tokens * NQH):
            var reference_argmax = 0
            var candidate_argmax = 0
            var reference_max = Float32(reference[GUARD + row * HD])
            var candidate_max = Float32(candidate[GUARD + row * HD])
            for column in range(HD):
                index = GUARD + row * HD + column
                expected = Float32(reference[index])
                actual = Float32(candidate[index])
                tuned_value = Float32(optimized[index])
                wider_value = Float32(wider[index])
                if actual != actual:
                    raise Error("Flash Attention zwrocil NaN")
                if tuned_value != tuned_value:
                    raise Error("zoptymalizowany Flash Attention zwrocil NaN")
                if wider_value != wider_value:
                    raise Error("Flash Attention BK32 zwrocil NaN")
                ulp = abs(_ordered_f16(optimized[index]) - _ordered_f16(candidate[index]))
                if ulp > max_ulp:
                    max_ulp = ulp
                error = abs(actual - expected)
                mean_absolute += error
                if error > max_absolute:
                    max_absolute = error
                wider_error = abs(wider_value - expected)
                if wider_error > bk32_max_absolute:
                    bk32_max_absolute = wider_error
                if expected > reference_max:
                    reference_max = expected
                    reference_argmax = column
                if actual > candidate_max:
                    candidate_max = actual
                    candidate_argmax = column
            if reference_argmax != candidate_argmax:
                argmax_mismatches += 1
    mean_absolute /= Float32(output_elements)
    print(
        "HD256 T",
        tokens,
        "base",
        base,
        "page",
        page_size,
        "device_pos",
        device_position,
        "max_abs",
        max_absolute,
        "mean_abs",
        mean_absolute,
        "argmax_mismatch",
        argmax_mismatches,
        "max_ulp",
        max_ulp,
    )
    if max_absolute > 0.03 or mean_absolute > 0.003:
        raise Error("Flash Attention HD256 przekroczyl limit bledu")
    if bk32_max_absolute > 0.03:
        raise Error("Flash Attention HD256 BK32 przekroczyl limit bledu")
    if max_ulp > 1:
        raise Error("zoptymalizowany Flash Attention HD256 przekroczyl 1 ULP")


def main() raises:
    var ctx = DeviceContext()
    _check_case[1, 17, 16, False](ctx)
    _check_case[17, 17, 16, True](ctx)
    _check_case[128, 32, 31, False](ctx)
    _check_case[129, 31, 32, True](ctx)
    _check_case[2048, 17, 19, True](ctx)
    print("FLASH ATTENTION HD256: PASS")
