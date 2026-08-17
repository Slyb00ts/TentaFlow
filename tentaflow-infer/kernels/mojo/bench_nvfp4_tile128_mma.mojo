# =============================================================================
# Plik: bench_nvfp4_tile128_mma.mojo
# Opis: Sprawdza parity i wydajnosc prefill MMA dla ukladu tile-major N128/K64.
# Przyklad: pixi run mojo bench_nvfp4_tile128_mma.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.time import perf_counter_ns
from src.nvfp4_gguf_mma_bn128 import (
    gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1,
    gemm_nvfp4_gguf_mma_f16_bm128_bn128,
)
from src.nvfp4_tile128_mma import (
    gemm_nvfp4_tile128_mma_f16_bm128_bn64,
    gemm_nvfp4_tile128_mma_f16_bm128_bn128,
)

comptime GUARD = 64
comptime GUARD_VALUE = Float16(19.0)
comptime TILE_ROWS = 128
comptime TILE_BLOCK_BYTES = TILE_ROWS * 36
comptime STAGE_BYTES = TILE_ROWS * 18
comptime ROUNDS = 5


def _repack[rows: Int, cols: Int](
    raw: DeviceBuffer[DType.uint8], packed: DeviceBuffer[DType.uint8]
) raises:
    comptime blocks = cols // 64
    comptime tiles = rows // TILE_ROWS
    with raw.map_to_host() as source, packed.map_to_host() as target:
        for tile in range(tiles):
            for block in range(blocks):
                tile_block = (tile * blocks + block) * TILE_BLOCK_BYTES
                for stage in range(2):
                    stage_base = tile_block + stage * STAGE_BYTES
                    for row in range(TILE_ROWS):
                        raw_base = ((tile * TILE_ROWS + row) * blocks + block) * 36
                        for subblock in range(2):
                            layout_thread = row * 2 + subblock
                            group = stage * 2 + subblock
                            target[stage_base + layout_thread] = source[raw_base + group]
                            code_target = stage_base + 256 + layout_thread * 8
                            code_source = raw_base + 4 + group * 8
                            for element in range(8):
                                target[code_target + element] = source[code_source + element]


def _row_bn64[rows: Int, cols: Int, tokens: Int](
    ctx: DeviceContext, mut y: DeviceBuffer[DType.float16],
    mut weights: DeviceBuffer[DType.uint8], mut x: DeviceBuffer[DType.float16],
) raises:
    ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1](
        y.unsafe_ptr() + GUARD, weights.unsafe_ptr(), x.unsafe_ptr(),
        cols, rows, tokens, Float32(0.625),
        grid_dim=((rows + 63) // 64, (tokens + 127) // 128), block_dim=256,
    )


def _row_bn128[rows: Int, cols: Int, tokens: Int](
    ctx: DeviceContext, mut y: DeviceBuffer[DType.float16],
    mut weights: DeviceBuffer[DType.uint8], mut x: DeviceBuffer[DType.float16],
) raises:
    ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128_bn128](
        y.unsafe_ptr() + GUARD, weights.unsafe_ptr(), x.unsafe_ptr(),
        cols, rows, tokens, Float32(0.625),
        grid_dim=((rows + 127) // 128, (tokens + 127) // 128), block_dim=256,
    )


def _tile_bn128[rows: Int, cols: Int, tokens: Int](
    ctx: DeviceContext, mut y: DeviceBuffer[DType.float16],
    mut weights: DeviceBuffer[DType.uint8], mut x: DeviceBuffer[DType.float16],
) raises:
    ctx.enqueue_function[gemm_nvfp4_tile128_mma_f16_bm128_bn128](
        y.unsafe_ptr() + GUARD, weights.unsafe_ptr(), x.unsafe_ptr(),
        cols, rows, tokens, Float32(0.625),
        grid_dim=((rows + 127) // 128, (tokens + 127) // 128), block_dim=256,
    )


def _tile_bn64[rows: Int, cols: Int, tokens: Int](
    ctx: DeviceContext, mut y: DeviceBuffer[DType.float16],
    mut weights: DeviceBuffer[DType.uint8], mut x: DeviceBuffer[DType.float16],
) raises:
    ctx.enqueue_function[gemm_nvfp4_tile128_mma_f16_bm128_bn64](
        y.unsafe_ptr() + GUARD, weights.unsafe_ptr(), x.unsafe_ptr(),
        cols, rows, tokens, Float32(0.625),
        grid_dim=((rows + 63) // 64, (tokens + 127) // 128), block_dim=256,
    )


def _case[rows: Int, cols: Int, tokens: Int, iterations: Int](
    ctx: DeviceContext,
) raises:
    comptime weight_bytes = rows * (cols // 64) * 36
    comptime output_elements = tokens * rows + 2 * GUARD
    var raw = ctx.enqueue_create_buffer[DType.uint8](weight_bytes)
    var packed = ctx.enqueue_create_buffer[DType.uint8](weight_bytes)
    var x = ctx.enqueue_create_buffer[DType.float16](tokens * cols)
    var row64 = ctx.enqueue_create_buffer[DType.float16](output_elements)
    var row128 = ctx.enqueue_create_buffer[DType.float16](output_elements)
    var tile64 = ctx.enqueue_create_buffer[DType.float16](output_elements)
    var tile128 = ctx.enqueue_create_buffer[DType.float16](output_elements)
    with raw.map_to_host() as values:
        for i in range(len(values)):
            values[i] = UInt8((i * 29 + 11) & 0xFF)
        for i in range(0, len(values), 36):
            values[i] = UInt8(0x38)
            values[i + 1] = UInt8(0x30)
            values[i + 2] = UInt8(0x40)
            values[i + 3] = UInt8(0x28)
    _repack[rows, cols](raw, packed)
    with x.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32((i * 17 % 31) - 15) * 0.03125)
    with row128.map_to_host() as expected, tile64.map_to_host() as actual64, tile128.map_to_host() as actual:
        for i in range(output_elements):
            expected[i] = GUARD_VALUE
            actual64[i] = GUARD_VALUE
            actual[i] = GUARD_VALUE
    _row_bn128[rows, cols, tokens](ctx, row128, raw, x)
    _tile_bn64[rows, cols, tokens](ctx, tile64, packed, x)
    _tile_bn128[rows, cols, tokens](ctx, tile128, packed, x)
    ctx.synchronize()
    with row128.map_to_host() as expected, tile64.map_to_host() as actual64, tile128.map_to_host() as actual:
        for i in range(output_elements):
            if (expected[i].to_bits() != actual64[i].to_bits()
                or expected[i].to_bits() != actual[i].to_bits()):
                raise Error(
                    "tile MMA roznica bitowa lub canary M" + String(tokens)
                    + " N" + String(rows) + " K" + String(cols)
                    + " przy " + String(i)
                )

    for _ in range(3):
        _row_bn64[rows, cols, tokens](ctx, row64, raw, x)
        _row_bn128[rows, cols, tokens](ctx, row128, raw, x)
        _tile_bn64[rows, cols, tokens](ctx, tile64, packed, x)
        _tile_bn128[rows, cols, tokens](ctx, tile128, packed, x)
    ctx.synchronize()
    var row64_times = InlineArray[Float64, ROUNDS](fill=0.0)
    var row128_times = InlineArray[Float64, ROUNDS](fill=0.0)
    var tile64_times = InlineArray[Float64, ROUNDS](fill=0.0)
    var tile_times = InlineArray[Float64, ROUNDS](fill=0.0)
    for round in range(ROUNDS):
        var started = perf_counter_ns()
        for _ in range(iterations):
            _row_bn64[rows, cols, tokens](ctx, row64, raw, x)
        ctx.synchronize()
        row64_times[round] = Float64(perf_counter_ns() - started) / 1e3 / Float64(iterations)
        started = perf_counter_ns()
        for _ in range(iterations):
            _tile_bn64[rows, cols, tokens](ctx, tile64, packed, x)
        ctx.synchronize()
        tile64_times[round] = Float64(perf_counter_ns() - started) / 1e3 / Float64(iterations)
        started = perf_counter_ns()
        for _ in range(iterations):
            _tile_bn128[rows, cols, tokens](ctx, tile128, packed, x)
        ctx.synchronize()
        tile_times[round] = Float64(perf_counter_ns() - started) / 1e3 / Float64(iterations)
        started = perf_counter_ns()
        for _ in range(iterations):
            _row_bn128[rows, cols, tokens](ctx, row128, raw, x)
        ctx.synchronize()
        row128_times[round] = Float64(perf_counter_ns() - started) / 1e3 / Float64(iterations)
    for i in range(ROUNDS):
        for j in range(i + 1, ROUNDS):
            if row64_times[j] < row64_times[i]:
                row64_times[i], row64_times[j] = row64_times[j], row64_times[i]
            if row128_times[j] < row128_times[i]:
                row128_times[i], row128_times[j] = row128_times[j], row128_times[i]
            if tile_times[j] < tile_times[i]:
                tile_times[i], tile_times[j] = tile_times[j], tile_times[i]
            if tile64_times[j] < tile64_times[i]:
                tile64_times[i], tile64_times[j] = tile64_times[j], tile64_times[i]
    best_row = min(row64_times[2], row128_times[2])
    best_tile = min(tile64_times[2], tile_times[2])
    print(
        "PASS M", tokens, "N", rows, "K", cols,
        "row64_us", row64_times[2], "row128_us", row128_times[2],
        "tile64_us", tile64_times[2], "tile128_us", tile_times[2],
        "speedup", best_row / best_tile,
    )


def _shape[rows: Int, cols: Int](ctx: DeviceContext) raises:
    _case[rows, cols, 32, 12](ctx)
    _case[rows, cols, 64, 12](ctx)
    _case[rows, cols, 128, 10](ctx)
    _case[rows, cols, 1024, 3](ctx)
    _case[rows, cols, 2048, 2](ctx)


def main() raises:
    var ctx = DeviceContext()
    _shape[12288, 5120](ctx)
    _shape[1024, 5120](ctx)
    _shape[5120, 6144](ctx)
    _shape[17408, 5120](ctx)
    _shape[5120, 17408](ctx)
