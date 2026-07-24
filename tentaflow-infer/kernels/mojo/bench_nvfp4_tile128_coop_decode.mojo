# =============================================================================
# Plik: bench_nvfp4_tile128_coop_decode.mojo
# Opis: Sprawdza i mierzy kooperacyjny M1 tile128 na pieciu realnych ksztaltach.
# Przyklad: pixi run mojo bench_nvfp4_tile128_coop_decode.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.time import perf_counter_ns
from src.nvfp4_gguf_dp4a import gemv_nvfp4_gguf_q8_1_f16
from src.nvfp4_gguf_batch import (
    gemm_nvfp4_gguf_f16_b2,
    gemm_nvfp4_gguf_f16_b3_nvidia,
    gemm_nvfp4_gguf_f16_b4_nvidia,
    gemm_nvfp4_gguf_f16_b8_nvidia,
)
from src.nvfp4_tile128_decode import gemv_nvfp4_tile128_coop_q8_1_f16
from scratch.nvfp4_tile128_coop_decode import (
    gemm_nvfp4_tile128_coop_f16_b2,
    gemm_nvfp4_tile128_coop_f16_b3,
    gemm_nvfp4_tile128_coop_f16_b4,
    gemm_nvfp4_tile128_coop_f16_b6,
    gemm_nvfp4_tile128_coop_f16_b8,
)
from src.nvfp4_tile128_repack import nvfp4_repack_tile128

comptime GUARD = 64
comptime GUARD_VALUE = Float16(19.0)
comptime TILE_ROWS = 128
comptime TILE_BLOCK_BYTES = TILE_ROWS * 36
comptime STAGE_BYTES = TILE_ROWS * 18
comptime ITERATIONS = 20
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
                            thread = row * 2 + subblock
                            group = stage * 2 + subblock
                            target[stage_base + thread] = source[raw_base + group]
                            code_target = stage_base + 256 + thread * 8
                            code_source = raw_base + 4 + group * 8
                            for element in range(8):
                                target[code_target + element] = source[code_source + element]


def _launch_raw[rows: Int, cols: Int](
    ctx: DeviceContext,
    mut y: DeviceBuffer[DType.float16],
    mut weights: DeviceBuffer[DType.uint8],
    mut xq: DeviceBuffer[DType.int8],
    mut xd: DeviceBuffer[DType.float32],
) raises:
    ctx.enqueue_function[gemv_nvfp4_gguf_q8_1_f16](
        y.unsafe_ptr() + GUARD, weights.unsafe_ptr(), xq.unsafe_ptr(),
        xd.unsafe_ptr(), cols, rows, Float32(0.625),
        grid_dim=(rows // 8,), block_dim=256,
    )


def _launch_tile[rows: Int, cols: Int](
    ctx: DeviceContext,
    mut y: DeviceBuffer[DType.float16],
    mut weights: DeviceBuffer[DType.uint8],
    mut xq: DeviceBuffer[DType.int8],
    mut xd: DeviceBuffer[DType.float32],
) raises:
    ctx.enqueue_function[gemv_nvfp4_tile128_coop_q8_1_f16](
        y.unsafe_ptr() + GUARD, weights.unsafe_ptr(), xq.unsafe_ptr(),
        xd.unsafe_ptr(), cols, rows, Float32(0.625),
        grid_dim=(rows // 4,), block_dim=128,
    )


def _launch_small_raw[rows: Int, cols: Int, tokens: Int](
    ctx: DeviceContext,
    mut y: DeviceBuffer[DType.float16],
    mut weights: DeviceBuffer[DType.uint8],
    mut x: DeviceBuffer[DType.float16],
) raises:
    comptime if tokens == 2:
        ctx.enqueue_function[gemm_nvfp4_gguf_f16_b2](
            y.unsafe_ptr() + GUARD, weights.unsafe_ptr(), x.unsafe_ptr(),
            cols, rows, tokens, Float32(0.625), grid_dim=(rows,), block_dim=32,
        )
    elif tokens == 3:
        ctx.enqueue_function[gemm_nvfp4_gguf_f16_b3_nvidia](
            y.unsafe_ptr() + GUARD, weights.unsafe_ptr(), x.unsafe_ptr(),
            cols, rows, tokens, Float32(0.625),
            grid_dim=((rows + 1) // 2,), block_dim=64,
        )
    elif tokens == 4:
        ctx.enqueue_function[gemm_nvfp4_gguf_f16_b4_nvidia](
            y.unsafe_ptr() + GUARD, weights.unsafe_ptr(), x.unsafe_ptr(),
            cols, rows, tokens, Float32(0.625),
            grid_dim=((rows + 1) // 2,), block_dim=64,
        )
    else:
        ctx.enqueue_function[gemm_nvfp4_gguf_f16_b8_nvidia](
            y.unsafe_ptr() + GUARD, weights.unsafe_ptr(), x.unsafe_ptr(),
            cols, rows, tokens, Float32(0.625),
            grid_dim=((rows + 1) // 2,), block_dim=64,
        )


def _launch_small_tile[rows: Int, cols: Int, tokens: Int](
    ctx: DeviceContext,
    mut y: DeviceBuffer[DType.float16],
    mut weights: DeviceBuffer[DType.uint8],
    mut x: DeviceBuffer[DType.float16],
) raises:
    comptime if tokens == 2:
        ctx.enqueue_function[gemm_nvfp4_tile128_coop_f16_b2](
            y.unsafe_ptr() + GUARD, weights.unsafe_ptr(), x.unsafe_ptr(),
            cols, rows, tokens, Float32(0.625),
            grid_dim=(rows // 8,), block_dim=256,
        )
    elif tokens == 3:
        ctx.enqueue_function[gemm_nvfp4_tile128_coop_f16_b3](
            y.unsafe_ptr() + GUARD, weights.unsafe_ptr(), x.unsafe_ptr(),
            cols, rows, tokens, Float32(0.625),
            grid_dim=(rows // 8,), block_dim=256,
        )
    elif tokens == 4:
        ctx.enqueue_function[gemm_nvfp4_tile128_coop_f16_b4](
            y.unsafe_ptr() + GUARD, weights.unsafe_ptr(), x.unsafe_ptr(),
            cols, rows, tokens, Float32(0.625),
            grid_dim=(rows // 8,), block_dim=256,
        )
    elif tokens == 6:
        ctx.enqueue_function[gemm_nvfp4_tile128_coop_f16_b6](
            y.unsafe_ptr() + GUARD, weights.unsafe_ptr(), x.unsafe_ptr(),
            cols, rows, tokens, Float32(0.625),
            grid_dim=(rows // 8,), block_dim=256,
        )
    else:
        ctx.enqueue_function[gemm_nvfp4_tile128_coop_f16_b8](
            y.unsafe_ptr() + GUARD, weights.unsafe_ptr(), x.unsafe_ptr(),
            cols, rows, tokens, Float32(0.625),
            grid_dim=(rows // 8,), block_dim=256,
        )


def _small_case[rows: Int, cols: Int, tokens: Int](
    ctx: DeviceContext,
    mut raw: DeviceBuffer[DType.uint8],
    mut packed: DeviceBuffer[DType.uint8],
    mut x: DeviceBuffer[DType.float16],
) raises:
    var reference = ctx.enqueue_create_buffer[DType.float16](tokens * rows + 2 * GUARD)
    var candidate = ctx.enqueue_create_buffer[DType.float16](tokens * rows + 2 * GUARD)
    with reference.map_to_host() as expected, candidate.map_to_host() as actual:
        for i in range(len(expected)):
            expected[i] = GUARD_VALUE
            actual[i] = GUARD_VALUE
    _launch_small_raw[rows, cols, tokens](ctx, reference, raw, x)
    _launch_small_tile[rows, cols, tokens](ctx, candidate, packed, x)
    ctx.synchronize()
    with reference.map_to_host() as expected, candidate.map_to_host() as actual:
        for i in range(len(expected)):
            if expected[i].to_bits() != actual[i].to_bits():
                raise Error(
                    "M" + String(tokens) + " tile128 roznica bitowa lub canary "
                    + String(rows) + "x" + String(cols) + " przy " + String(i)
                )
    for _ in range(5):
        _launch_small_raw[rows, cols, tokens](ctx, reference, raw, x)
        _launch_small_tile[rows, cols, tokens](ctx, candidate, packed, x)
    ctx.synchronize()
    var raw_times = InlineArray[Float64, ROUNDS](fill=0.0)
    var tile_times = InlineArray[Float64, ROUNDS](fill=0.0)
    for round in range(ROUNDS):
        var started = perf_counter_ns()
        if round % 2 == 0:
            for _ in range(ITERATIONS):
                _launch_small_raw[rows, cols, tokens](ctx, reference, raw, x)
            ctx.synchronize()
            raw_times[round] = Float64(perf_counter_ns() - started) / 1e3 / ITERATIONS
            started = perf_counter_ns()
            for _ in range(ITERATIONS):
                _launch_small_tile[rows, cols, tokens](ctx, candidate, packed, x)
            ctx.synchronize()
            tile_times[round] = Float64(perf_counter_ns() - started) / 1e3 / ITERATIONS
        else:
            for _ in range(ITERATIONS):
                _launch_small_tile[rows, cols, tokens](ctx, candidate, packed, x)
            ctx.synchronize()
            tile_times[round] = Float64(perf_counter_ns() - started) / 1e3 / ITERATIONS
            started = perf_counter_ns()
            for _ in range(ITERATIONS):
                _launch_small_raw[rows, cols, tokens](ctx, reference, raw, x)
            ctx.synchronize()
            raw_times[round] = Float64(perf_counter_ns() - started) / 1e3 / ITERATIONS
    for i in range(ROUNDS):
        for j in range(i + 1, ROUNDS):
            if raw_times[j] < raw_times[i]:
                raw_times[i], raw_times[j] = raw_times[j], raw_times[i]
            if tile_times[j] < tile_times[i]:
                tile_times[i], tile_times[j] = tile_times[j], tile_times[i]
    print(
        "PASS M", tokens, "N", rows, "K", cols, "raw_us", raw_times[2],
        "tile_us", tile_times[2], "speedup", raw_times[2] / tile_times[2],
    )


def _case[rows: Int, cols: Int](ctx: DeviceContext) raises:
    comptime blocks = cols // 64
    weight_bytes = rows * blocks * 36
    var raw = ctx.enqueue_create_buffer[DType.uint8](weight_bytes)
    var packed = ctx.enqueue_create_buffer[DType.uint8](weight_bytes)
    var xq = ctx.enqueue_create_buffer[DType.int8](cols)
    var xd = ctx.enqueue_create_buffer[DType.float32](cols // 32)
    var x = ctx.enqueue_create_buffer[DType.float16](8 * cols)
    var reference = ctx.enqueue_create_buffer[DType.float16](rows + 2 * GUARD)
    var candidate = ctx.enqueue_create_buffer[DType.float16](rows + 2 * GUARD)
    with raw.map_to_host() as values:
        for i in range(len(values)):
            values[i] = UInt8((i * 29 + 11) & 0xFF)
        for i in range(0, len(values), 36):
            values[i] = UInt8(0x38)
            values[i + 1] = UInt8(0x30)
            values[i + 2] = UInt8(0x40)
            values[i + 3] = UInt8(0x28)
    repack_started = perf_counter_ns()
    _repack[rows, cols](raw, packed)
    repack_ms = Float64(perf_counter_ns() - repack_started) / 1e6
    ctx.enqueue_function[nvfp4_repack_tile128](
        packed.unsafe_ptr(), raw.unsafe_ptr(), blocks,
        grid_dim=(2 * (rows // TILE_ROWS) * blocks,), block_dim=256,
    )
    ctx.synchronize()
    gpu_started = perf_counter_ns()
    ctx.enqueue_function[nvfp4_repack_tile128](
        packed.unsafe_ptr(), raw.unsafe_ptr(), blocks,
        grid_dim=(2 * (rows // TILE_ROWS) * blocks,), block_dim=256,
    )
    ctx.synchronize()
    gpu_ms = Float64(perf_counter_ns() - gpu_started) / 1e6
    print(
        "REPACK N", rows, "K", cols, "bytes", weight_bytes,
        "host_ms", repack_ms, "gpu_ms", gpu_ms,
    )
    with xq.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Int8((i * 13 % 127) - 63)
    with xd.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float32((i % 7) + 1) * 0.0009765625
    with x.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32((i * 17 % 31) - 15) * 0.03125)
    with reference.map_to_host() as expected, candidate.map_to_host() as actual:
        for i in range(len(expected)):
            expected[i] = GUARD_VALUE
            actual[i] = GUARD_VALUE
    _launch_raw[rows, cols](ctx, reference, raw, xq, xd)
    _launch_tile[rows, cols](ctx, candidate, packed, xq, xd)
    ctx.synchronize()
    with reference.map_to_host() as expected, candidate.map_to_host() as actual:
        for i in range(len(expected)):
            if expected[i].to_bits() != actual[i].to_bits():
                raise Error(
                    "M1 tile128 roznica bitowa lub canary "
                    + String(rows) + "x" + String(cols) + " przy " + String(i)
                )

    for _ in range(5):
        _launch_raw[rows, cols](ctx, reference, raw, xq, xd)
        _launch_tile[rows, cols](ctx, candidate, packed, xq, xd)
    ctx.synchronize()
    var raw_times = InlineArray[Float64, ROUNDS](fill=0.0)
    var tile_times = InlineArray[Float64, ROUNDS](fill=0.0)
    for round in range(ROUNDS):
        var started = perf_counter_ns()
        if round % 2 == 0:
            for _ in range(ITERATIONS):
                _launch_raw[rows, cols](ctx, reference, raw, xq, xd)
            ctx.synchronize()
            raw_times[round] = Float64(perf_counter_ns() - started) / 1e3 / ITERATIONS
            started = perf_counter_ns()
            for _ in range(ITERATIONS):
                _launch_tile[rows, cols](ctx, candidate, packed, xq, xd)
            ctx.synchronize()
            tile_times[round] = Float64(perf_counter_ns() - started) / 1e3 / ITERATIONS
        else:
            for _ in range(ITERATIONS):
                _launch_tile[rows, cols](ctx, candidate, packed, xq, xd)
            ctx.synchronize()
            tile_times[round] = Float64(perf_counter_ns() - started) / 1e3 / ITERATIONS
            started = perf_counter_ns()
            for _ in range(ITERATIONS):
                _launch_raw[rows, cols](ctx, reference, raw, xq, xd)
            ctx.synchronize()
            raw_times[round] = Float64(perf_counter_ns() - started) / 1e3 / ITERATIONS
    for i in range(ROUNDS):
        for j in range(i + 1, ROUNDS):
            if raw_times[j] < raw_times[i]:
                raw_times[i], raw_times[j] = raw_times[j], raw_times[i]
            if tile_times[j] < tile_times[i]:
                tile_times[i], tile_times[j] = tile_times[j], tile_times[i]
    print(
        "PASS M1 N", rows, "K", cols, "raw_us", raw_times[2],
        "tile_us", tile_times[2], "speedup", raw_times[2] / tile_times[2],
    )
    _small_case[rows, cols, 2](ctx, raw, packed, x)
    _small_case[rows, cols, 3](ctx, raw, packed, x)
    _small_case[rows, cols, 4](ctx, raw, packed, x)
    _small_case[rows, cols, 6](ctx, raw, packed, x)
    _small_case[rows, cols, 8](ctx, raw, packed, x)


def main() raises:
    var ctx = DeviceContext()
    _case[12288, 5120](ctx)
    _case[1024, 5120](ctx)
    _case[5120, 6144](ctx)
    _case[17408, 5120](ctx)
    _case[5120, 17408](ctx)
