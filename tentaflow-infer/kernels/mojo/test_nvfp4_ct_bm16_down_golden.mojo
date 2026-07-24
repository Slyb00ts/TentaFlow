# =============================================================================
# Plik: test_nvfp4_ct_bm16_down_golden.mojo
# Opis: Porównuje produkcyjny wrapper down BM16 z referencją na realnych wagach.
# Przykład: mojo test_nvfp4_ct_bm16_down_golden.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.math import sqrt
from std.python import Python
from std.time import perf_counter_ns
from src.nvfp4_batch import (
    gemv_batch_nvfp4_f16_b4,
    gemv_batch_nvfp4_f16_b8,
    gemv_batch_nvfp4_f16_b16,
)
from src.nvfp4_ct_bm16 import (
    gemm_nvfp4_ct_bm16_down_m4,
    gemm_nvfp4_ct_bm16_down_m8,
    gemm_nvfp4_ct_bm16_down_m16,
    reduce_nvfp4_direct_down,
)
from src.nvfp4_ct_layout import repack_nvfp4_ct_s0_n64k128_into

comptime ROWS = 4096
comptime COLS = 11264
comptime PHYSICAL_M = 16
comptime CANARY = 128
comptime OUTER_SPLIT = 4
comptime PARTS = OUTER_SPLIT
comptime PACKED_BYTES = ROWS * COLS // 2
comptime SCALE_BYTES = ROWS * COLS // 16
comptime RESIDENT_BYTES = PACKED_BYTES + SCALE_BYTES
comptime INV_GLOBAL_SCALE = 1.0 / 11072.0
comptime WARMUP = 20
comptime ITERS = 100
comptime ROUNDS = 7
comptime DATA = (
    "/home/critix/.cache/tentaflow-profiles/"
    "nvfp4-marlin-v2-study/v2/data/down/"
)


def _load[
    dtype: DType, size: Int
](mut buffer: DeviceBuffer[dtype], path: String) raises:
    builtins = Python.import_module("builtins")
    ctypes = Python.import_module("ctypes")
    data = builtins.open(path, "rb").read()
    with buffer.map_to_host() as target:
        _ = ctypes.memmove(Int(target.unsafe_ptr()), data, size)


def _candidate[logical_m: Int](
    ctx: DeviceContext,
    mut output: DeviceBuffer[DType.float16],
    mut partial: DeviceBuffer[DType.float32],
    mut resident: DeviceBuffer[DType.uint8],
    mut x: DeviceBuffer[DType.float16],
) raises:
    _gemm[logical_m](ctx, partial, resident, x)
    _reduce(ctx, output, partial)


def _gemm[logical_m: Int](
    ctx: DeviceContext,
    mut partial: DeviceBuffer[DType.float32],
    mut resident: DeviceBuffer[DType.uint8],
    mut x: DeviceBuffer[DType.float16],
) raises:
    comptime if logical_m == 4:
        ctx.enqueue_function[gemm_nvfp4_ct_bm16_down_m4](
            partial.unsafe_ptr(), resident.unsafe_ptr(), x.unsafe_ptr(),
            Float32(INV_GLOBAL_SCALE),
            grid_dim=(ROWS // 64 * OUTER_SPLIT,), block_dim=128,
        )
    elif logical_m == 8:
        ctx.enqueue_function[gemm_nvfp4_ct_bm16_down_m8](
            partial.unsafe_ptr(), resident.unsafe_ptr(), x.unsafe_ptr(),
            Float32(INV_GLOBAL_SCALE),
            grid_dim=(ROWS // 64 * OUTER_SPLIT,), block_dim=128,
        )
    else:
        ctx.enqueue_function[gemm_nvfp4_ct_bm16_down_m16](
            partial.unsafe_ptr(), resident.unsafe_ptr(), x.unsafe_ptr(),
            Float32(INV_GLOBAL_SCALE),
            grid_dim=(ROWS // 64 * OUTER_SPLIT,), block_dim=128,
        )


def _reduce(
    ctx: DeviceContext,
    mut output: DeviceBuffer[DType.float16],
    mut partial: DeviceBuffer[DType.float32],
) raises:
    ctx.enqueue_function[reduce_nvfp4_direct_down](
        output.unsafe_ptr(), partial.unsafe_ptr(),
        ROWS, PHYSICAL_M, PARTS,
        grid_dim=((ROWS * PHYSICAL_M + 255) // 256,), block_dim=256,
    )


def _native[logical_m: Int](
    ctx: DeviceContext,
    mut output: DeviceBuffer[DType.float16],
    mut packed: DeviceBuffer[DType.uint8],
    mut scales: DeviceBuffer[DType.uint8],
    mut x: DeviceBuffer[DType.float16],
) raises:
    comptime if logical_m == 4:
        ctx.enqueue_function[gemv_batch_nvfp4_f16_b4](
            output.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(),
            x.unsafe_ptr(), COLS, ROWS, logical_m, Float32(INV_GLOBAL_SCALE),
            grid_dim=(ROWS // 8,), block_dim=256,
        )
    elif logical_m == 8:
        ctx.enqueue_function[gemv_batch_nvfp4_f16_b8](
            output.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(),
            x.unsafe_ptr(), COLS, ROWS, logical_m, Float32(INV_GLOBAL_SCALE),
            grid_dim=(ROWS // 8,), block_dim=256,
        )
    else:
        ctx.enqueue_function[gemv_batch_nvfp4_f16_b16](
            output.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(),
            x.unsafe_ptr(), COLS, ROWS, logical_m, Float32(INV_GLOBAL_SCALE),
            grid_dim=(ROWS // 8,), block_dim=256,
        )


def _bench[logical_m: Int]() raises:
    var ctx = DeviceContext()
    var packed = ctx.enqueue_create_buffer[DType.uint8](PACKED_BYTES)
    var scales = ctx.enqueue_create_buffer[DType.uint8](SCALE_BYTES)
    var resident = ctx.enqueue_create_buffer[DType.uint8](RESIDENT_BYTES)
    var x = ctx.enqueue_create_buffer[DType.float16](
        PHYSICAL_M * COLS + CANARY
    )
    var reference = ctx.enqueue_create_buffer[DType.float16](logical_m * ROWS)
    var output = ctx.enqueue_create_buffer[DType.float16](
        PHYSICAL_M * ROWS + CANARY
    )
    var partial = ctx.enqueue_create_buffer[DType.float32](
        PARTS * PHYSICAL_M * ROWS
    )
    _load[DType.uint8, PACKED_BYTES](packed, DATA + "weight_packed.bin")
    _load[DType.uint8, SCALE_BYTES](scales, DATA + "weight_scale.bin")
    with x.map_to_host() as values:
        for i in range(logical_m * COLS):
            values[i] = Float16(
                Float32((i * 17 + (i // COLS) * 13) % 127 - 63) * 0.00390625
            )
        for i in range(logical_m * COLS, PHYSICAL_M * COLS):
            values[i] = Float16(91.0)
        for i in range(PHYSICAL_M * COLS, len(values)):
            values[i] = Float16(-77.0)
    with output.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(-91.0)
    ctx.enqueue_function[repack_nvfp4_ct_s0_n64k128_into](
        resident.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(),
        COLS, ROWS, 0,
        grid_dim=(ROWS // 64 * (COLS // 128),), block_dim=128,
    )
    _native[logical_m](ctx, reference, packed, scales, x)
    _candidate[logical_m](ctx, output, partial, resident, x)
    ctx.synchronize()

    var sum_sq = 0.0
    var diff_sq = 0.0
    var max_abs = 0.0
    var top1_equal = 0
    with reference.map_to_host() as expected, output.map_to_host() as actual:
        for token in range(logical_m):
            var expected_top = 0
            var actual_top = 0
            for row in range(ROWS):
                i = token * ROWS + row
                left = Float64(expected[i])
                right = Float64(actual[i])
                diff = abs(left - right)
                sum_sq += left * left
                diff_sq += diff * diff
                max_abs = max(max_abs, diff)
                if expected[i] > expected[token * ROWS + expected_top]:
                    expected_top = row
                if actual[i] > actual[token * ROWS + actual_top]:
                    actual_top = row
            if expected_top == actual_top:
                top1_equal += 1
        for i in range(logical_m * ROWS, PHYSICAL_M * ROWS):
            if actual[i] != Float16(0.0):
                print(
                    "tail_failure", "m", logical_m,
                    "index", i, "value", actual[i],
                )
                raise Error("BM16 down nie wyzerował fizycznego ogona")
        for i in range(PHYSICAL_M * ROWS, PHYSICAL_M * ROWS + CANARY):
            if actual[i] != Float16(-91.0):
                raise Error("BM16 down zapisał za fizycznym wyjściem")

    for _ in range(WARMUP):
        _native[logical_m](ctx, reference, packed, scales, x)
        _candidate[logical_m](ctx, output, partial, resident, x)
    ctx.synchronize()
    var native_times = InlineArray[Float64, ROUNDS](fill=0.0)
    for round in range(ROUNDS):
        started = perf_counter_ns()
        for _ in range(ITERS):
            _native[logical_m](ctx, reference, packed, scales, x)
        ctx.synchronize()
        native_times[round] = (
            Float64(perf_counter_ns() - started) / 1e3 / ITERS
        )
    var times = InlineArray[Float64, ROUNDS](fill=0.0)
    for round in range(ROUNDS):
        started = perf_counter_ns()
        for _ in range(ITERS):
            _candidate[logical_m](ctx, output, partial, resident, x)
        ctx.synchronize()
        times[round] = Float64(perf_counter_ns() - started) / 1e3 / ITERS
    for i in range(ROUNDS):
        for j in range(i + 1, ROUNDS):
            if times[j] < times[i]:
                times[i], times[j] = times[j], times[i]
            if native_times[j] < native_times[i]:
                native_times[i], native_times[j] = (
                    native_times[j], native_times[i]
                )

    var gemm_times = InlineArray[Float64, ROUNDS](fill=0.0)
    for round in range(ROUNDS):
        started = perf_counter_ns()
        for _ in range(ITERS):
            _gemm[logical_m](ctx, partial, resident, x)
        ctx.synchronize()
        gemm_times[round] = Float64(perf_counter_ns() - started) / 1e3 / ITERS
    for i in range(ROUNDS):
        for j in range(i + 1, ROUNDS):
            if gemm_times[j] < gemm_times[i]:
                gemm_times[i], gemm_times[j] = gemm_times[j], gemm_times[i]

    var reduce_times = InlineArray[Float64, ROUNDS](fill=0.0)
    for round in range(ROUNDS):
        started = perf_counter_ns()
        for _ in range(ITERS):
            _reduce(ctx, output, partial)
        ctx.synchronize()
        reduce_times[round] = (
            Float64(perf_counter_ns() - started) / 1e3 / ITERS
        )
    for i in range(ROUNDS):
        for j in range(i + 1, ROUNDS):
            if reduce_times[j] < reduce_times[i]:
                reduce_times[i], reduce_times[j] = (
                    reduce_times[j], reduce_times[i]
                )
    print(
        "m", logical_m,
        "native_us", native_times[ROUNDS // 2],
        "direct_down_us", times[ROUNDS // 2],
        "gemm_us", gemm_times[ROUNDS // 2],
        "reduce_us", reduce_times[ROUNDS // 2],
        "rel_l2", sqrt(diff_sq / sum_sq),
        "max_abs", max_abs, "top1", top1_equal, "/", logical_m,
        "canary", True,
    )


def main() raises:
    _bench[4]()
    _bench[8]()
