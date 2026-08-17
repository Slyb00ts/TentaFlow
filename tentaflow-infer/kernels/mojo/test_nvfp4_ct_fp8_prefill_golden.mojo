# =============================================================================
# Plik: test_nvfp4_ct_fp8_prefill_golden.mojo
# Opis: Sprawdza pełny łańcuch tile->FP8->GEMM dla kafli BM64 i BM128.
# Przykład: mojo test_nvfp4_ct_fp8_prefill_golden.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.python import Python
from src.gemm_fp8 import (
    gemm_fp8_f16,
    gemm_fp8_f16_bm64,
    quantize_act_fp8,
)
from src.nvfp4 import pack_nvfp4_fp8
from src.nvfp4_ct_fp8 import pack_nvfp4_ct_s0_fp8
from src.nvfp4_ct_layout import repack_nvfp4_ct_s0_n64k128_into

comptime TOKENS = 128
comptime ROWS = 4096
comptime COLS = 4096
comptime PACKED_BYTES = ROWS * COLS // 2
comptime SCALE_BYTES = ROWS * COLS // 16
comptime RESIDENT_BYTES = PACKED_BYTES + SCALE_BYTES
comptime OUTPUT_ELEMENTS = TOKENS * ROWS
comptime CANARY_ELEMENTS = 128
comptime INV_GLOBAL_SCALE = 1.0 / 11648.0
comptime DATA = (
    "/home/critix/.cache/tentaflow-profiles/"
    "nvfp4-marlin-v2-study/v2/data/gate/"
)


def _load[
    dtype: DType, size: Int
](mut buffer: DeviceBuffer[dtype], path: String) raises:
    builtins = Python.import_module("builtins")
    ctypes = Python.import_module("ctypes")
    data = builtins.open(path, "rb").read()
    with buffer.map_to_host() as target:
        _ = ctypes.memmove(Int(target.unsafe_ptr()), data, size)


def _fill_canary(mut output: DeviceBuffer[DType.float16]) raises:
    with output.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(-91.0)


def main() raises:
    var ctx = DeviceContext()
    var packed = ctx.enqueue_create_buffer[DType.uint8](PACKED_BYTES)
    var scales = ctx.enqueue_create_buffer[DType.uint8](SCALE_BYTES)
    var resident = ctx.enqueue_create_buffer[DType.uint8](RESIDENT_BYTES)
    var row_weights = ctx.enqueue_create_buffer[DType.int8](ROWS * COLS)
    var tile_weights = ctx.enqueue_create_buffer[DType.int8](ROWS * COLS)
    var row_scales = ctx.enqueue_create_buffer[DType.float32](ROWS)
    var tile_scales = ctx.enqueue_create_buffer[DType.float32](ROWS)
    var x = ctx.enqueue_create_buffer[DType.float16](TOKENS * COLS)
    var xq = ctx.enqueue_create_buffer[DType.int8](TOKENS * COLS)
    var xs = ctx.enqueue_create_buffer[DType.float32](TOKENS)
    var row_bm64 = ctx.enqueue_create_buffer[DType.float16](
        OUTPUT_ELEMENTS + CANARY_ELEMENTS
    )
    var tile_bm64 = ctx.enqueue_create_buffer[DType.float16](
        OUTPUT_ELEMENTS + CANARY_ELEMENTS
    )
    var row_bm128 = ctx.enqueue_create_buffer[DType.float16](
        OUTPUT_ELEMENTS + CANARY_ELEMENTS
    )
    var tile_bm128 = ctx.enqueue_create_buffer[DType.float16](
        OUTPUT_ELEMENTS + CANARY_ELEMENTS
    )
    _load[DType.uint8, PACKED_BYTES](packed, DATA + "weight_packed.bin")
    _load[DType.uint8, SCALE_BYTES](scales, DATA + "weight_scale.bin")
    with x.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(
                Float32((i * 17 + (i // COLS) * 13) % 127 - 63) * 0.00390625
            )
    _fill_canary(row_bm64)
    _fill_canary(tile_bm64)
    _fill_canary(row_bm128)
    _fill_canary(tile_bm128)

    ctx.enqueue_function[repack_nvfp4_ct_s0_n64k128_into](
        resident.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(),
        COLS, ROWS, 0,
        grid_dim=(ROWS // 64 * (COLS // 128),), block_dim=128,
    )
    ctx.enqueue_function[pack_nvfp4_fp8](
        row_weights.unsafe_ptr(), row_scales.unsafe_ptr(),
        packed.unsafe_ptr(), scales.unsafe_ptr(),
        COLS, 0, ROWS, Float32(INV_GLOBAL_SCALE),
        grid_dim=(ROWS,), block_dim=256,
    )
    ctx.enqueue_function[pack_nvfp4_ct_s0_fp8](
        tile_weights.unsafe_ptr(), tile_scales.unsafe_ptr(),
        resident.unsafe_ptr(),
        COLS, 0, ROWS, Float32(INV_GLOBAL_SCALE),
        grid_dim=(ROWS,), block_dim=256,
    )
    ctx.enqueue_function[quantize_act_fp8](
        xq.unsafe_ptr(), xs.unsafe_ptr(), x.unsafe_ptr(),
        COLS, TOKENS,
        grid_dim=(TOKENS,), block_dim=256,
    )
    ctx.enqueue_function[gemm_fp8_f16_bm64](
        row_bm64.unsafe_ptr(), row_weights.unsafe_ptr(),
        row_scales.unsafe_ptr(), xq.unsafe_ptr(), xs.unsafe_ptr(),
        COLS, ROWS, TOKENS,
        grid_dim=(ROWS // 64, TOKENS // 64), block_dim=256,
    )
    ctx.enqueue_function[gemm_fp8_f16_bm64](
        tile_bm64.unsafe_ptr(), tile_weights.unsafe_ptr(),
        tile_scales.unsafe_ptr(), xq.unsafe_ptr(), xs.unsafe_ptr(),
        COLS, ROWS, TOKENS,
        grid_dim=(ROWS // 64, TOKENS // 64), block_dim=256,
    )
    ctx.enqueue_function[gemm_fp8_f16](
        row_bm128.unsafe_ptr(), row_weights.unsafe_ptr(),
        row_scales.unsafe_ptr(), xq.unsafe_ptr(), xs.unsafe_ptr(),
        COLS, ROWS, TOKENS,
        grid_dim=(ROWS // 64, 1), block_dim=256,
    )
    ctx.enqueue_function[gemm_fp8_f16](
        tile_bm128.unsafe_ptr(), tile_weights.unsafe_ptr(),
        tile_scales.unsafe_ptr(), xq.unsafe_ptr(), xs.unsafe_ptr(),
        COLS, ROWS, TOKENS,
        grid_dim=(ROWS // 64, 1), block_dim=256,
    )
    ctx.synchronize()

    var mismatch64 = 0
    var mismatch128 = 0
    var tile_mismatch = 0
    var top1_ok = True
    var canary_ok = True
    with row_bm64.map_to_host() as expected64, tile_bm64.map_to_host() as actual64, row_bm128.map_to_host() as expected128, tile_bm128.map_to_host() as actual128:
        for token in range(TOKENS):
            var expected_top = 0
            var actual_top = 0
            for row in range(ROWS):
                index = token * ROWS + row
                if expected64[index] != actual64[index]:
                    mismatch64 += 1
                if expected128[index] != actual128[index]:
                    mismatch128 += 1
                if actual64[index] != actual128[index]:
                    tile_mismatch += 1
                if expected64[index] > expected64[token * ROWS + expected_top]:
                    expected_top = row
                if actual64[index] > actual64[token * ROWS + actual_top]:
                    actual_top = row
            if expected_top != actual_top:
                top1_ok = False
        for i in range(OUTPUT_ELEMENTS, OUTPUT_ELEMENTS + CANARY_ELEMENTS):
            if (
                expected64[i] != Float16(-91.0)
                or actual64[i] != Float16(-91.0)
                or expected128[i] != Float16(-91.0)
                or actual128[i] != Float16(-91.0)
            ):
                canary_ok = False
    print(
        "bm64_mismatches", mismatch64,
        "bm128_mismatches", mismatch128,
        "tile64_vs_tile128", tile_mismatch,
        "top1", top1_ok,
        "canary", canary_ok,
    )
