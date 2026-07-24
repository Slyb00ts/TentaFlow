# =============================================================================
# Plik: test_nvfp4_ct_prefill_exact.mojo
# Opis: Porównuje bitowo row-major i S0 GEMM dla okien K/V oraz T64-T1024.
# Przykład: pixi run mojo test_nvfp4_ct_prefill_exact.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.python import Python
from src.gemm import gemm_nvfp4_impl
from src.nvfp4_ct_layout import repack_nvfp4_ct_s0_n64k128_into
from src.nvfp4_ct_prefill import gemm_nvfp4_ct_s0_impl

comptime ROWS = 6144
comptime COLS = 4096
comptime WINDOW_ROWS = 1024
comptime PACKED_BYTES = ROWS * COLS // 2
comptime SCALE_BYTES = ROWS * COLS // 16
comptime RESIDENT_BYTES = PACKED_BYTES + SCALE_BYTES
comptime DATA_ROOT = (
    "/home/critix/.cache/tentaflow-profiles/"
    "nvfp4-marlin-v2-study/v2/data/gate/"
)


def _load(mut buffer: DeviceBuffer[DType.uint8], path: String) raises:
    builtins = Python.import_module("builtins")
    ctypes = Python.import_module("ctypes")
    data = builtins.open(path, "rb").read()
    if Int(data.__len__()) < len(buffer):
        raise Error("za mało danych w pliku " + path)
    with buffer.map_to_host() as target:
        _ = ctypes.memmove(Int(target.unsafe_ptr()), data, len(buffer))


def _case[tokens: Int, BM: Int, NW: Int, source_row_offset: Int](
    ctx: DeviceContext,
    name: String,
    mut packed: DeviceBuffer[DType.uint8],
    mut scales: DeviceBuffer[DType.uint8],
    mut resident: DeviceBuffer[DType.uint8],
) raises:
    comptime output_elements = tokens * WINDOW_ROWS
    var x = ctx.enqueue_create_buffer[DType.float16](tokens * COLS)
    var reference = ctx.enqueue_create_buffer[DType.float16](output_elements)
    var candidate = ctx.enqueue_create_buffer[DType.float16](output_elements)
    with x.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(
                Float32((i * 17 + 13) % 127 - 63) * 0.00390625
            )

    ctx.enqueue_function[gemm_nvfp4_impl[BM, NW]](
        reference.unsafe_ptr(),
        packed.unsafe_ptr() + source_row_offset * (COLS // 2),
        scales.unsafe_ptr() + source_row_offset * (COLS // 16),
        x.unsafe_ptr(),
        COLS,
        WINDOW_ROWS,
        tokens,
        Float32(1.0 / 11648.0),
        grid_dim=((WINDOW_ROWS + 63) // 64, (tokens + BM - 1) // BM),
        block_dim=NW * 32,
    )
    ctx.enqueue_function[gemm_nvfp4_ct_s0_impl[BM, NW]](
        candidate.unsafe_ptr(),
        resident.unsafe_ptr(),
        x.unsafe_ptr(),
        COLS,
        WINDOW_ROWS,
        tokens,
        source_row_offset,
        Float32(1.0 / 11648.0),
        grid_dim=((WINDOW_ROWS + 63) // 64, (tokens + BM - 1) // BM),
        block_dim=NW * 32,
    )
    ctx.synchronize()

    var mismatches = 0
    var max_abs = 0.0
    with reference.map_to_host() as expected, candidate.map_to_host() as actual:
        for i in range(output_elements):
            if expected[i] != actual[i]:
                mismatches += 1
                max_abs = max(
                    max_abs,
                    abs(Float64(expected[i]) - Float64(actual[i])),
                )
    print(
        name,
        "T", tokens,
        "mismatches", mismatches,
        "max_abs", max_abs,
    )
    if mismatches != 0:
        raise Error("wynik S0 prefill nie jest bitowo zgodny z row-major")


def main() raises:
    var ctx = DeviceContext()
    var packed = ctx.enqueue_create_buffer[DType.uint8](PACKED_BYTES)
    var scales = ctx.enqueue_create_buffer[DType.uint8](SCALE_BYTES)
    var resident = ctx.enqueue_create_buffer[DType.uint8](RESIDENT_BYTES)
    _load(packed, DATA_ROOT + "weight_packed.bin")
    _load(scales, DATA_ROOT + "weight_scale.bin")
    ctx.enqueue_function[repack_nvfp4_ct_s0_n64k128_into](
        resident.unsafe_ptr(),
        packed.unsafe_ptr(),
        scales.unsafe_ptr(),
        COLS,
        ROWS,
        0,
        grid_dim=(ROWS // 64 * (COLS // 128),),
        block_dim=128,
    )
    _case[64, 64, 4, 4096](ctx, "K", packed, scales, resident)
    _case[128, 128, 8, 4096](ctx, "K", packed, scales, resident)
    _case[1024, 128, 8, 4096](ctx, "K", packed, scales, resident)
    _case[64, 64, 4, 5120](ctx, "V", packed, scales, resident)
    _case[128, 128, 8, 5120](ctx, "V", packed, scales, resident)
    _case[1024, 128, 8, 5120](ctx, "V", packed, scales, resident)
