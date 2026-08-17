# =============================================================================
# Plik: test_nvfp4_ct_marlin_layout.mojo
# Opis: Sprawdza każdy bajt kanonicznego przepakowania kodów i skal Marlin.
# Przykład: pixi run mojo test_nvfp4_ct_marlin_layout.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from src.nvfp4_ct_layout import nvfp4_ct_s0_from_e4m3
from src.nvfp4_ct_marlin_layout import (
    repack_nvfp4_ct_marlin_codes,
    repack_nvfp4_ct_marlin_scales,
)

comptime ROWS = 128
comptime COLS = 128
comptime PACKED_BYTES = ROWS * COLS // 2
comptime SCALE_BYTES = ROWS * COLS // 16


def _nibble(
    packed: UnsafePointer[UInt8, MutUntrackedOrigin],
    row: Int,
    col: Int,
) -> UInt32:
    raw = packed[row * (COLS // 2) + col // 2]
    return UInt32((raw >> UInt8((col & 1) * 4)) & 0x0F)


def main() raises:
    var ctx = DeviceContext()
    var packed = ctx.enqueue_create_buffer[DType.uint8](PACKED_BYTES)
    var scales = ctx.enqueue_create_buffer[DType.uint8](SCALE_BYTES)
    var output = ctx.enqueue_create_buffer[DType.uint8](
        PACKED_BYTES + SCALE_BYTES
    )
    with packed.map_to_host() as values:
        for index in range(len(values)):
            values[index] = UInt8((index * 37 + 11) & 0xFF)
    with scales.map_to_host() as values:
        for index in range(len(values)):
            values[index] = UInt8((index * 13 + 1) % 0x70)
    ctx.enqueue_function[repack_nvfp4_ct_marlin_codes](
        output.unsafe_ptr(),
        packed.unsafe_ptr(),
        COLS,
        ROWS,
        grid_dim=(COLS // 16 * (ROWS // 64),),
        block_dim=128,
    )
    ctx.enqueue_function[repack_nvfp4_ct_marlin_scales](
        output.unsafe_ptr() + PACKED_BYTES,
        scales.unsafe_ptr(),
        COLS,
        ROWS,
        grid_dim=((SCALE_BYTES + 255) // 256,),
        block_dim=256,
    )
    ctx.synchronize()

    with packed.map_to_host() as source, output.map_to_host() as actual:
        comptime n_tiles = ROWS // 64
        comptime for k_tile in range(COLS // 16):
            comptime for n_tile in range(n_tiles):
                comptime tile = k_tile * n_tiles + n_tile
                comptime for tid in range(128):
                    comptime thread_in_warp = tid // 4
                    comptime tile_warp = tid % 4
                    comptime tensor_col = thread_in_warp // 4
                    comptime tensor_row = (thread_in_warp % 4) * 2
                    comptime row = n_tile * 64 + tile_warp * 16 + tensor_col
                    comptime col = k_tile * 16 + tensor_row
                    expected = (
                        _nibble(source.unsafe_ptr(), row, col)
                        | _nibble(source.unsafe_ptr(), row, col + 8) << 4
                        | _nibble(source.unsafe_ptr(), row + 8, col) << 8
                        | _nibble(source.unsafe_ptr(), row + 8, col + 8) << 12
                        | _nibble(source.unsafe_ptr(), row, col + 1) << 16
                        | _nibble(source.unsafe_ptr(), row, col + 9) << 20
                        | _nibble(source.unsafe_ptr(), row + 8, col + 1) << 24
                        | _nibble(source.unsafe_ptr(), row + 8, col + 9) << 28
                    )
                    offset = tile * 512 + tid * 4
                    found = (
                        UInt32(actual[offset])
                        | UInt32(actual[offset + 1]) << 8
                        | UInt32(actual[offset + 2]) << 16
                        | UInt32(actual[offset + 3]) << 24
                    )
                    if found != expected:
                        raise Error("niepoprawne przepakowanie kodów Marlin")

    with scales.map_to_host() as source, output.map_to_host() as actual:
        for output_index in range(SCALE_BYTES):
            group4_base = (output_index // 4) * 4
            index4 = output_index % 4
            var permuted_index = group4_base
            if index4 == 0:
                permuted_index += 0
            elif index4 == 1:
                permuted_index += 2
            elif index4 == 2:
                permuted_index += 1
            else:
                permuted_index += 3
            chunk_base = (permuted_index // 64) * 64
            chunk_index = permuted_index % 64
            transposed_index = (
                chunk_base + chunk_index // 8 + (chunk_index % 8) * 8
            )
            source_group = transposed_index // ROWS
            source_row = transposed_index % ROWS
            expected_scale = nvfp4_ct_s0_from_e4m3(
                source[source_row * (COLS // 16) + source_group]
            )
            if actual[PACKED_BYTES + output_index] != expected_scale:
                raise Error("niepoprawne przepakowanie skal Marlin")
    print("nvfp4 canonical Marlin layout: ok")
