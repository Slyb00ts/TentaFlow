# =============================================================================
# Plik: bench_nvfp4_ct_highm.mojo
# Opis: Porównuje produkcyjny prefill z eksperymentalnym potokiem BM16/BK128.
# Przykład: pixi run mojo bench_nvfp4_ct_highm.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.python import Python
from std.time import perf_counter_ns
from src.nvfp4_ct_layout import (
    repack_nvfp4_ct_s0_n64k128_into,
)
from src.nvfp4_ct_marlin_layout import (
    repack_nvfp4_ct_marlin_codes,
    repack_nvfp4_ct_marlin_scales,
)
from src.nvfp4_ct_prefill import gemm_nvfp4_ct_s0_impl
from std.math import sqrt
from src.nvfp4_ct_marlin_highm import gemm_nvfp4_ct_marlin_bm64_bn256_bk64

comptime ROOT = (
    "/home/critix/.cache/tentaflow-profiles/"
    "nvfp4-marlin-v2-study/v2/data/"
)
comptime ITERS = 20
comptime ROUNDS = 7


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


def _case[rows: Int, cols: Int, tokens: Int](
    ctx: DeviceContext,
    name: String,
    path: String,
    inv_global_scale: Float32,
) raises:
    comptime packed_bytes = rows * cols // 2
    comptime scale_bytes = rows * cols // 16
    var packed = ctx.enqueue_create_buffer[DType.uint8](packed_bytes)
    var scales = ctx.enqueue_create_buffer[DType.uint8](scale_bytes)
    var resident_current = ctx.enqueue_create_buffer[DType.uint8](
        packed_bytes + scale_bytes
    )
    var resident_candidate = ctx.enqueue_create_buffer[DType.uint8](
        packed_bytes + scale_bytes
    )
    var x = ctx.enqueue_create_buffer[DType.float16](tokens * cols)
    var reference = ctx.enqueue_create_buffer[DType.float16](tokens * rows)
    var candidate = ctx.enqueue_create_buffer[DType.float16](tokens * rows + 256)
    _load(packed, path + "weight_packed.bin")
    _load(scales, path + "weight_scale.bin")
    with x.map_to_host() as values:
        for index in range(len(values)):
            values[index] = Float16(
                Float32((index * 17 + 13) % 127 - 63) * 0.00390625
            )
    with candidate.map_to_host() as values:
        for index in range(len(values)):
            values[index] = Float16(-91.0)
    ctx.enqueue_function[repack_nvfp4_ct_s0_n64k128_into](
        resident_current.unsafe_ptr(),
        packed.unsafe_ptr(),
        scales.unsafe_ptr(),
        cols,
        rows,
        0,
        grid_dim=(rows // 64 * (cols // 128),),
        block_dim=128,
    )
    ctx.enqueue_function[repack_nvfp4_ct_marlin_codes](
        resident_candidate.unsafe_ptr(),
        packed.unsafe_ptr(),
        cols,
        rows,
        grid_dim=(cols // 16 * (rows // 64),),
        block_dim=128,
    )
    ctx.enqueue_function[repack_nvfp4_ct_marlin_scales](
        resident_candidate.unsafe_ptr() + packed_bytes,
        scales.unsafe_ptr(),
        cols,
        rows,
        grid_dim=((scale_bytes + 255) // 256,),
        block_dim=256,
    )
    ctx.enqueue_function[gemm_nvfp4_ct_s0_impl[64, 4]](
        reference.unsafe_ptr(),
        resident_current.unsafe_ptr(),
        x.unsafe_ptr(),
        cols,
        rows,
        tokens,
        0,
        inv_global_scale,
        grid_dim=(rows // 64, tokens // 64),
        block_dim=128,
    )
    ctx.enqueue_function[
        gemm_nvfp4_ct_marlin_bm64_bn256_bk64[cols, rows]
    ](
        candidate.unsafe_ptr(),
        resident_candidate.unsafe_ptr(),
        x.unsafe_ptr(),
        tokens,
        inv_global_scale,
        grid_dim=(128,),
        block_dim=256,
        shared_mem_bytes=73728,
    )
    ctx.synchronize()
    var sum_sq = 0.0
    var diff_sq = 0.0
    var max_abs = 0.0
    var top1_equal = 0
    with reference.map_to_host() as expected, candidate.map_to_host() as actual:
        for token in range(tokens):
            var expected_top = 0
            var actual_top = 0
            for row in range(rows):
                index = token * rows + row
                left = Float64(expected[index])
                right = Float64(actual[index])
                diff = abs(left - right)
                sum_sq += left * left
                diff_sq += diff * diff
                max_abs = max(max_abs, diff)
                if expected[index] > expected[token * rows + expected_top]:
                    expected_top = row
                if actual[index] > actual[token * rows + actual_top]:
                    actual_top = row
            if expected_top == actual_top:
                top1_equal += 1
        for index in range(tokens * rows, tokens * rows + 256):
            if actual[index] != Float16(-91.0):
                raise Error("kernel zapisał za końcem bufora")
    for _ in range(5):
        ctx.enqueue_function[gemm_nvfp4_ct_s0_impl[64, 4]](
            reference.unsafe_ptr(), resident_current.unsafe_ptr(), x.unsafe_ptr(),
            cols, rows, tokens, 0, inv_global_scale,
            grid_dim=(rows // 64, tokens // 64), block_dim=128,
        )
        ctx.enqueue_function[
            gemm_nvfp4_ct_marlin_bm64_bn256_bk64[cols, rows]
        ](
            candidate.unsafe_ptr(), resident_candidate.unsafe_ptr(), x.unsafe_ptr(),
            tokens, inv_global_scale,
            grid_dim=(128,), block_dim=256, shared_mem_bytes=73728,
        )
    ctx.synchronize()
    var reference_times = InlineArray[Float64, ROUNDS](uninitialized=True)
    var candidate_times = InlineArray[Float64, ROUNDS](uninitialized=True)
    for round_index in range(ROUNDS):
        var started = perf_counter_ns()
        for _ in range(ITERS):
            ctx.enqueue_function[gemm_nvfp4_ct_s0_impl[64, 4]](
                reference.unsafe_ptr(), resident_current.unsafe_ptr(), x.unsafe_ptr(),
                cols, rows, tokens, 0, inv_global_scale,
                grid_dim=(rows // 64, tokens // 64), block_dim=128,
            )
        ctx.synchronize()
        reference_times[round_index] = (
            Float64(perf_counter_ns() - started) / Float64(ITERS)
        )
        started = perf_counter_ns()
        for _ in range(ITERS):
            ctx.enqueue_function[
                gemm_nvfp4_ct_marlin_bm64_bn256_bk64[cols, rows]
            ](
                candidate.unsafe_ptr(), resident_candidate.unsafe_ptr(), x.unsafe_ptr(),
                tokens, inv_global_scale,
                grid_dim=(128,), block_dim=256, shared_mem_bytes=73728,
            )
        ctx.synchronize()
        candidate_times[round_index] = (
            Float64(perf_counter_ns() - started) / Float64(ITERS)
        )
    reference_ns = _median(reference_times)
    candidate_ns = _median(candidate_times)
    print(
        name, "M", tokens,
        "current_us", reference_ns / 1000.0,
        "highm_us", candidate_ns / 1000.0,
        "speedup", reference_ns / candidate_ns,
        "relative_l2", sqrt(diff_sq / sum_sq),
        "max_abs", max_abs,
        "top1", top1_equal, "/", tokens,
    )


def main() raises:
    var ctx = DeviceContext()
    _case[11264, 4096, 64](ctx, "gate", ROOT + "gate/", 1.0 / 11648.0)
    _case[11264, 4096, 128](ctx, "gate", ROOT + "gate/", 1.0 / 11648.0)
    _case[11264, 4096, 1024](ctx, "gate", ROOT + "gate/", 1.0 / 11648.0)
    _case[4096, 11264, 64](ctx, "down", ROOT + "down/", 1.0 / 11072.0)
    _case[4096, 11264, 128](ctx, "down", ROOT + "down/", 1.0 / 11072.0)
    _case[4096, 11264, 1024](ctx, "down", ROOT + "down/", 1.0 / 11072.0)
