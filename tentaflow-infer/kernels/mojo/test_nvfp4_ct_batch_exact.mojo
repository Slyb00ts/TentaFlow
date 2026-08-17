# =============================================================================
# Plik: test_nvfp4_ct_batch_exact.mojo
# Opis: Porównuje bitowo małe batche row-major i S0 na rzeczywistych kształtach.
# Przykład: pixi run mojo test_nvfp4_ct_batch_exact.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.python import Python
from src.nvfp4_batch import gemv_batch_nvfp4_f16_impl
from src.nvfp4_ct_layout import repack_nvfp4_ct_s0_n64k128_into
from src.nvfp4_ct_decode import (
    gemv_batch_nvfp4_ct_s0_n64k128_f16_impl,
)

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
    batch: Int,
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
    comptime output_elements = batch * rows
    var packed = ctx.enqueue_create_buffer[DType.uint8](packed_bytes)
    var scales = ctx.enqueue_create_buffer[DType.uint8](scale_bytes)
    var resident_with_canary = ctx.enqueue_create_buffer[DType.uint8](
        resident_bytes + 256
    )
    var x = ctx.enqueue_create_buffer[DType.float16](batch * cols)
    var reference = ctx.enqueue_create_buffer[DType.float16](output_elements)
    var candidate = ctx.enqueue_create_buffer[DType.float16](output_elements)
    _load_repeated[source_packed_bytes, repeats](
        packed, DATA_ROOT + source + "/weight_packed.bin"
    )
    _load_repeated[source_scale_bytes, repeats](
        scales, DATA_ROOT + source + "/weight_scale.bin"
    )
    with resident_with_canary.map_to_host() as values:
        for i in range(len(values)):
            values[i] = UInt8(0xA5)
    with x.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(
                Float32((i * 17 + 13) % 127 - 63) * 0.00390625
            )

    ctx.enqueue_function[repack_nvfp4_ct_s0_n64k128_into](
        resident_with_canary.unsafe_ptr(),
        packed.unsafe_ptr(),
        scales.unsafe_ptr(),
        cols,
        rows,
        0,
        grid_dim=(rows // 64 * (cols // 128),),
        block_dim=128,
    )
    ctx.enqueue_function[gemv_batch_nvfp4_f16_impl[batch]](
        reference.unsafe_ptr(),
        packed.unsafe_ptr(),
        scales.unsafe_ptr(),
        x.unsafe_ptr(),
        cols,
        rows,
        batch,
        inv_global_scale,
        grid_dim=((rows + 7) // 8,),
        block_dim=256,
    )
    ctx.enqueue_function[
        gemv_batch_nvfp4_ct_s0_n64k128_f16_impl[batch]
    ](
        candidate.unsafe_ptr(),
        resident_with_canary.unsafe_ptr(),
        x.unsafe_ptr(),
        cols,
        rows,
        batch,
        0,
        inv_global_scale,
        grid_dim=((rows + 7) // 8,),
        block_dim=256,
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
    var canary_ok = True
    with resident_with_canary.map_to_host() as values:
        for i in range(resident_bytes, resident_bytes + 256):
            if values[i] != UInt8(0xA5):
                canary_ok = False
    print(
        name,
        "M", batch,
        "mismatches", mismatches,
        "max_abs", max_abs,
        "canary", canary_ok,
    )
    if mismatches != 0 or not canary_ok:
        raise Error("wynik S0 nie jest bitowo zgodny z row-major")


def _all_batches[
    rows: Int,
    cols: Int,
    source_rows: Int,
    repeats: Int,
](
    ctx: DeviceContext,
    name: String,
    source: String,
    inv_global_scale: Float32,
) raises:
    _shape[rows, cols, source_rows, repeats, 1](
        ctx, name, source, inv_global_scale
    )
    _shape[rows, cols, source_rows, repeats, 4](
        ctx, name, source, inv_global_scale
    )
    _shape[rows, cols, source_rows, repeats, 8](
        ctx, name, source, inv_global_scale
    )
    _shape[rows, cols, source_rows, repeats, 16](
        ctx, name, source, inv_global_scale
    )


def main() raises:
    var ctx = DeviceContext()
    _all_batches[6144, 4096, 6144, 1](
        ctx, "qkv", "gate", Float32(1.0 / 11648.0)
    )
    _all_batches[4096, 4096, 4096, 1](
        ctx, "o", "gate", Float32(1.0 / 11648.0)
    )
    _all_batches[22528, 4096, 11264, 2](
        ctx, "gateup", "gate", Float32(1.0 / 11648.0)
    )
    _all_batches[4096, 11264, 4096, 1](
        ctx, "down", "down", Float32(1.0 / 11072.0)
    )
