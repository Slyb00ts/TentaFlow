# =============================================================================
# Plik: test_nvfp4_ct_fp8_golden.mojo
# Opis: Porównuje tile->FP8 z istniejącym row->FP8 dla czterech projekcji.
# Przykład: mojo test_nvfp4_ct_fp8_golden.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.memory import bitcast
from std.python import Python
from src.gemm_fp8 import gemm_fp8_f16_bm64, quantize_act_fp8
from src.nvfp4 import pack_nvfp4_fp8
from src.nvfp4_ct_fp8 import pack_nvfp4_ct_s0_fp8
from src.nvfp4_ct_layout import repack_nvfp4_ct_s0_n64k128_into

comptime WINDOW_ROWS = 64
comptime TOKENS = 64
comptime CANARY_BYTES = 256
comptime DATA_ROOT = (
    "/home/critix/.cache/tentaflow-profiles/"
    "nvfp4-marlin-v2-study/v2/data/"
)


def _load_repeated[
    size: Int, repeats: Int
](
    mut buffer: DeviceBuffer[DType.uint8],
    path: String,
) raises:
    builtins = Python.import_module("builtins")
    ctypes = Python.import_module("ctypes")
    data = builtins.open(path, "rb").read()
    if Int(data.__len__()) < size:
        raise Error("za mało danych w pliku " + path)
    with buffer.map_to_host() as target:
        for repeat in range(repeats):
            _ = ctypes.memmove(
                Int(target.unsafe_ptr()) + repeat * size,
                data,
                size,
            )


def _shape[
    rows: Int,
    cols: Int,
    source_rows: Int,
    repeats: Int,
    row_offset: Int,
](
    ctx: DeviceContext,
    name: String,
    source: String,
    inv_global_scale: Float32,
) raises:
    comptime source_packed_bytes = source_rows * cols // 2
    comptime source_scale_bytes = source_rows * cols // 16
    comptime packed_bytes = rows * cols // 2
    comptime scale_bytes = rows * cols // 16
    comptime resident_bytes = packed_bytes + scale_bytes
    comptime output_bytes = WINDOW_ROWS * cols
    var packed = ctx.enqueue_create_buffer[DType.uint8](packed_bytes)
    var scales = ctx.enqueue_create_buffer[DType.uint8](scale_bytes)
    var resident = ctx.enqueue_create_buffer[DType.uint8](resident_bytes)
    var reference = ctx.enqueue_create_buffer[DType.int8](
        output_bytes + CANARY_BYTES
    )
    var candidate = ctx.enqueue_create_buffer[DType.int8](
        output_bytes + CANARY_BYTES
    )
    var reference_scales = ctx.enqueue_create_buffer[DType.float32](
        WINDOW_ROWS + CANARY_BYTES // 4
    )
    var candidate_scales = ctx.enqueue_create_buffer[DType.float32](
        WINDOW_ROWS + CANARY_BYTES // 4
    )
    var x = ctx.enqueue_create_buffer[DType.float16](TOKENS * cols)
    var xq = ctx.enqueue_create_buffer[DType.int8](TOKENS * cols)
    var xs = ctx.enqueue_create_buffer[DType.float32](TOKENS)
    var reference_output = ctx.enqueue_create_buffer[DType.float16](
        TOKENS * WINDOW_ROWS + CANARY_BYTES // 2
    )
    var candidate_output = ctx.enqueue_create_buffer[DType.float16](
        TOKENS * WINDOW_ROWS + CANARY_BYTES // 2
    )
    _load_repeated[source_packed_bytes, repeats](
        packed, DATA_ROOT + source + "/weight_packed.bin"
    )
    _load_repeated[source_scale_bytes, repeats](
        scales, DATA_ROOT + source + "/weight_scale.bin"
    )
    with reference.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Int8(-91)
    with candidate.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Int8(-91)
    with reference_scales.map_to_host() as values:
        for i in range(len(values)):
            values[i] = bitcast[DType.float32, 1](
                SIMD[DType.uint32, 1](0x7F123456)
            )[0]
    with candidate_scales.map_to_host() as values:
        for i in range(len(values)):
            values[i] = bitcast[DType.float32, 1](
                SIMD[DType.uint32, 1](0x7F123456)
            )[0]
    with x.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(
                Float32((i * 17 + (i // cols) * 13) % 127 - 63) * 0.00390625
            )
    with reference_output.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(-91.0)
    with candidate_output.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(-91.0)

    ctx.enqueue_function[repack_nvfp4_ct_s0_n64k128_into](
        resident.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(),
        cols, rows, 0,
        grid_dim=(rows // 64 * (cols // 128),), block_dim=128,
    )
    ctx.enqueue_function[pack_nvfp4_fp8](
        reference.unsafe_ptr(), reference_scales.unsafe_ptr(),
        packed.unsafe_ptr(), scales.unsafe_ptr(),
        cols, row_offset, WINDOW_ROWS, inv_global_scale,
        grid_dim=(WINDOW_ROWS,), block_dim=256,
    )
    ctx.enqueue_function[pack_nvfp4_ct_s0_fp8](
        candidate.unsafe_ptr(), candidate_scales.unsafe_ptr(),
        resident.unsafe_ptr(),
        cols, row_offset, WINDOW_ROWS, inv_global_scale,
        grid_dim=(WINDOW_ROWS,), block_dim=256,
    )
    ctx.enqueue_function[quantize_act_fp8](
        xq.unsafe_ptr(), xs.unsafe_ptr(), x.unsafe_ptr(),
        cols, TOKENS,
        grid_dim=(TOKENS,), block_dim=256,
    )
    ctx.enqueue_function[gemm_fp8_f16_bm64](
        reference_output.unsafe_ptr(), reference.unsafe_ptr(),
        reference_scales.unsafe_ptr(), xq.unsafe_ptr(), xs.unsafe_ptr(),
        cols, WINDOW_ROWS, TOKENS,
        grid_dim=(1, 1), block_dim=256,
    )
    ctx.enqueue_function[gemm_fp8_f16_bm64](
        candidate_output.unsafe_ptr(), candidate.unsafe_ptr(),
        candidate_scales.unsafe_ptr(), xq.unsafe_ptr(), xs.unsafe_ptr(),
        cols, WINDOW_ROWS, TOKENS,
        grid_dim=(1, 1), block_dim=256,
    )
    ctx.synchronize()

    var byte_mismatches = 0
    var canary_ok = True
    var reference_top = 0
    var candidate_top = 0
    with reference.map_to_host() as expected, candidate.map_to_host() as actual:
        for i in range(output_bytes):
            if expected[i] != actual[i]:
                byte_mismatches += 1
        for row in range(WINDOW_ROWS):
            index = row * cols
            if expected[index] > expected[reference_top * cols]:
                reference_top = row
            if actual[index] > actual[candidate_top * cols]:
                candidate_top = row
        for i in range(output_bytes, output_bytes + CANARY_BYTES):
            if expected[i] != Int8(-91) or actual[i] != Int8(-91):
                canary_ok = False

    var max_scale_diff = 0.0
    with reference_scales.map_to_host() as expected, candidate_scales.map_to_host() as actual:
        for row in range(WINDOW_ROWS):
            max_scale_diff = max(
                max_scale_diff,
                abs(Float64(expected[row]) - Float64(actual[row])),
            )
        for row in range(WINDOW_ROWS, WINDOW_ROWS + CANARY_BYTES // 4):
            expected_bits = bitcast[DType.uint32, 1](
                SIMD[DType.float32, 1](expected[row])
            )[0]
            actual_bits = bitcast[DType.uint32, 1](
                SIMD[DType.float32, 1](actual[row])
            )[0]
            if expected_bits != 0x7F123456 or actual_bits != 0x7F123456:
                canary_ok = False
    var gemm_mismatches = 0
    var gemm_top1_ok = True
    with reference_output.map_to_host() as expected, candidate_output.map_to_host() as actual:
        for token in range(TOKENS):
            var expected_top = 0
            var actual_top = 0
            for row in range(WINDOW_ROWS):
                index = token * WINDOW_ROWS + row
                if expected[index] != actual[index]:
                    gemm_mismatches += 1
                if expected[index] > expected[token * WINDOW_ROWS + expected_top]:
                    expected_top = row
                if actual[index] > actual[token * WINDOW_ROWS + actual_top]:
                    actual_top = row
            if expected_top != actual_top:
                gemm_top1_ok = False
        for i in range(
            TOKENS * WINDOW_ROWS,
            TOKENS * WINDOW_ROWS + CANARY_BYTES // 2,
        ):
            if expected[i] != Float16(-91.0) or actual[i] != Float16(-91.0):
                canary_ok = False
    print(
        name,
        "offset", row_offset,
        "bytes", output_bytes,
        "mismatches", byte_mismatches,
        "max_scale_diff", max_scale_diff,
        "top1", reference_top == candidate_top,
        "gemm_mismatches", gemm_mismatches,
        "gemm_top1", gemm_top1_ok,
        "canary", canary_ok,
    )


def main() raises:
    var ctx = DeviceContext()
    _shape[6144, 4096, 6144, 1, 0](
        ctx, "q", "gate", Float32(1.0 / 11648.0)
    )
    _shape[6144, 4096, 6144, 1, 4096](
        ctx, "k", "gate", Float32(1.0 / 11648.0)
    )
    _shape[6144, 4096, 6144, 1, 5120](
        ctx, "v", "gate", Float32(1.0 / 11648.0)
    )
    _shape[4096, 4096, 4096, 1, 0](
        ctx, "o", "gate", Float32(1.0 / 11648.0)
    )
    _shape[22528, 4096, 11264, 2, 0](
        ctx, "gate", "gate", Float32(1.0 / 11648.0)
    )
    _shape[22528, 4096, 11264, 2, 11264](
        ctx, "up", "gate", Float32(1.0 / 11648.0)
    )
    _shape[4096, 11264, 4096, 1, 0](
        ctx, "down", "down", Float32(1.0 / 11072.0)
    )
