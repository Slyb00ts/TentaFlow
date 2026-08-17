# =============================================================================
# Plik: bench_nvfp4_ct_marlin_bm16.mojo
# Opis: Porównuje canonical Marlin i S0 dla fizycznego BM16 przy M4/M8/M16.
# Przykład: pixi run mojo bench_nvfp4_ct_marlin_bm16.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.python import Python
from std.time import perf_counter_ns
from std.math import sqrt
from src.nvfp4_ct_layout import repack_nvfp4_ct_s0_n64k128_into
from src.nvfp4_ct_marlin_layout import (
    repack_nvfp4_ct_marlin_codes,
    repack_nvfp4_ct_marlin_scales,
)
from src.nvfp4_ct_marlin_highm import (
    gemm_nvfp4_ct_marlin_bm16_bn64_bk128,
)
from src.nvfp4_ct_prefill import gemm_nvfp4_ct_s0_impl

comptime ROWS = 11264
comptime COLS = 4096
comptime PACKED_BYTES = ROWS * COLS // 2
comptime SCALE_BYTES = ROWS * COLS // 16
comptime ITERS = 20
comptime ROUNDS = 7
comptime INV_GLOBAL_SCALE = 1.0 / 11648.0
comptime ROOT = (
    "/home/critix/.cache/tentaflow-profiles/"
    "nvfp4-marlin-v2-study/v2/data/gate/"
)


def _load(mut buffer: DeviceBuffer[DType.uint8], path: String) raises:
    builtins = Python.import_module("builtins")
    ctypes = Python.import_module("ctypes")
    data = builtins.open(path, "rb").read()
    with buffer.map_to_host() as target:
        _ = ctypes.memmove(Int(target.unsafe_ptr()), data, len(buffer))


def _median(mut values: InlineArray[Float64, ROUNDS]) -> Float64:
    for left in range(ROUNDS):
        for right in range(left + 1, ROUNDS):
            if values[right] < values[left]:
                values[left], values[right] = values[right], values[left]
    return values[ROUNDS // 2]


def _case[valid_tokens: Int](
    ctx: DeviceContext,
    mut s0: DeviceBuffer[DType.uint8],
    mut canonical: DeviceBuffer[DType.uint8],
) raises:
    var x = ctx.enqueue_create_buffer[DType.float16](16 * COLS)
    var reference = ctx.enqueue_create_buffer[DType.float16](
        valid_tokens * ROWS
    )
    var candidate = ctx.enqueue_create_buffer[DType.float16](
        valid_tokens * ROWS
    )
    with x.map_to_host() as values:
        for index in range(len(values)):
            values[index] = Float16(
                Float32((index * 17 + 13) % 127 - 63) * 0.00390625
            )

    ctx.enqueue_function[gemm_nvfp4_ct_s0_impl[64, 4]](
        reference.unsafe_ptr(),
        s0.unsafe_ptr(),
        x.unsafe_ptr(),
        COLS,
        ROWS,
        valid_tokens,
        0,
        Float32(INV_GLOBAL_SCALE),
        grid_dim=(ROWS // 64, 1),
        block_dim=128,
    )
    ctx.enqueue_function[
        gemm_nvfp4_ct_marlin_bm16_bn64_bk128[
            COLS, ROWS, valid_tokens
        ]
    ](
        candidate.unsafe_ptr(),
        canonical.unsafe_ptr(),
        x.unsafe_ptr(),
        Float32(INV_GLOBAL_SCALE),
        grid_dim=(ROWS // 64,),
        block_dim=128,
        shared_mem_bytes=35840,
    )
    ctx.synchronize()

    var sum_sq = 0.0
    var actual_sq = 0.0
    var cross = 0.0
    var diff_sq = 0.0
    var max_abs = 0.0
    var top1_equal = 0
    with reference.map_to_host() as expected, candidate.map_to_host() as actual:
        comptime for token in range(valid_tokens):
            var expected_top = 0
            var actual_top = 0
            for row in range(ROWS):
                index = token * ROWS + row
                left = Float64(expected[index])
                right = Float64(actual[index])
                diff = abs(left - right)
                sum_sq += left * left
                actual_sq += right * right
                cross += left * right
                diff_sq += diff * diff
                max_abs = max(max_abs, diff)
                if expected[index] > expected[token * ROWS + expected_top]:
                    expected_top = row
                if actual[index] > actual[token * ROWS + actual_top]:
                    actual_top = row
            if expected_top == actual_top:
                top1_equal += 1

    for _ in range(5):
        ctx.enqueue_function[gemm_nvfp4_ct_s0_impl[64, 4]](
            reference.unsafe_ptr(), s0.unsafe_ptr(), x.unsafe_ptr(),
            COLS, ROWS, valid_tokens, 0, Float32(INV_GLOBAL_SCALE),
            grid_dim=(ROWS // 64, 1), block_dim=128,
        )
        ctx.enqueue_function[
            gemm_nvfp4_ct_marlin_bm16_bn64_bk128[
                COLS, ROWS, valid_tokens
            ]
        ](
            candidate.unsafe_ptr(), canonical.unsafe_ptr(), x.unsafe_ptr(),
            Float32(INV_GLOBAL_SCALE),
            grid_dim=(ROWS // 64,), block_dim=128,
            shared_mem_bytes=35840,
        )
    ctx.synchronize()

    var reference_times = InlineArray[Float64, ROUNDS](uninitialized=True)
    var candidate_times = InlineArray[Float64, ROUNDS](uninitialized=True)
    for round_index in range(ROUNDS):
        started = perf_counter_ns()
        for _ in range(ITERS):
            ctx.enqueue_function[gemm_nvfp4_ct_s0_impl[64, 4]](
                reference.unsafe_ptr(), s0.unsafe_ptr(), x.unsafe_ptr(),
                COLS, ROWS, valid_tokens, 0, Float32(INV_GLOBAL_SCALE),
                grid_dim=(ROWS // 64, 1), block_dim=128,
            )
        ctx.synchronize()
        reference_times[round_index] = (
            Float64(perf_counter_ns() - started) / Float64(ITERS)
        )
        started = perf_counter_ns()
        for _ in range(ITERS):
            ctx.enqueue_function[
                gemm_nvfp4_ct_marlin_bm16_bn64_bk128[
                    COLS, ROWS, valid_tokens
                ]
            ](
                candidate.unsafe_ptr(), canonical.unsafe_ptr(), x.unsafe_ptr(),
                Float32(INV_GLOBAL_SCALE),
                grid_dim=(ROWS // 64,), block_dim=128,
                shared_mem_bytes=35840,
            )
        ctx.synchronize()
        candidate_times[round_index] = (
            Float64(perf_counter_ns() - started) / Float64(ITERS)
        )
    reference_ns = _median(reference_times)
    candidate_ns = _median(candidate_times)
    print(
        "M", valid_tokens,
        "s0_generic_us", reference_ns / 1000.0,
        "canonical_us", candidate_ns / 1000.0,
        "speedup", reference_ns / candidate_ns,
        "relative_l2", sqrt(diff_sq / sum_sq),
        "norm_ratio", sqrt(actual_sq / sum_sq),
        "cosine", cross / sqrt(actual_sq * sum_sq),
        "max_abs", max_abs,
        "top1", top1_equal, "/", valid_tokens,
    )


def main() raises:
    var ctx = DeviceContext()
    var packed = ctx.enqueue_create_buffer[DType.uint8](PACKED_BYTES)
    var scales = ctx.enqueue_create_buffer[DType.uint8](SCALE_BYTES)
    var s0 = ctx.enqueue_create_buffer[DType.uint8](
        PACKED_BYTES + SCALE_BYTES
    )
    var canonical = ctx.enqueue_create_buffer[DType.uint8](
        PACKED_BYTES + SCALE_BYTES
    )
    _load(packed, ROOT + "weight_packed.bin")
    _load(scales, ROOT + "weight_scale.bin")
    ctx.enqueue_function[repack_nvfp4_ct_s0_n64k128_into](
        s0.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(),
        COLS, ROWS, 0,
        grid_dim=(ROWS // 64 * (COLS // 128),), block_dim=128,
    )
    ctx.enqueue_function[repack_nvfp4_ct_marlin_codes](
        canonical.unsafe_ptr(), packed.unsafe_ptr(), COLS, ROWS,
        grid_dim=(COLS // 16 * (ROWS // 64),), block_dim=128,
    )
    ctx.enqueue_function[repack_nvfp4_ct_marlin_scales](
        canonical.unsafe_ptr() + PACKED_BYTES,
        scales.unsafe_ptr(), COLS, ROWS,
        grid_dim=((SCALE_BYTES + 255) // 256,), block_dim=256,
    )
    ctx.synchronize()
    _case[4](ctx, s0, canonical)
    _case[8](ctx, s0, canonical)
    _case[16](ctx, s0, canonical)
