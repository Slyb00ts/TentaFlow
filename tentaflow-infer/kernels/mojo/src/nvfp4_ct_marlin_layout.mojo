# =============================================================================
# Plik: nvfp4_ct_marlin_layout.mojo
# Opis: Przepakowuje NVFP4 E2M1 i skale E4M3 do kanonicznego układu Marlin.
# Przykład: benchmark tworzy jeden bufor kodów i skal bez duplikowania wag.
# =============================================================================

from std.gpu import block_idx, block_dim, thread_idx
from src.nvfp4_ct_layout import nvfp4_ct_s0_from_e4m3

comptime MARLIN_TILE_K = 16
comptime MARLIN_TILE_N = 64
comptime MARLIN_CODE_TILE_BYTES = 512


def _nvfp4_nibble(
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    n_cols: Int,
    row: Int,
    col: Int,
) -> UInt32:
    raw = packed[row * (n_cols // 2) + col // 2]
    return UInt32((raw >> UInt8((col & 1) * 4)) & 0x0F)


def repack_nvfp4_ct_marlin_codes(
    target: UnsafePointer[UInt8, MutAnyOrigin],
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Zapisuje kafle K16/N64 zgodnie z gptq_marlin_repack dla W4A16."""
    tid = Int(thread_idx.x)
    if tid >= 128:
        return
    n_tiles = n_rows // MARLIN_TILE_N
    tile = Int(block_idx.x)
    k_tile = tile // n_tiles
    n_tile = tile % n_tiles
    thread_in_warp = tid // 4
    tile_warp = tid % 4
    tensor_col = thread_in_warp // 4
    tensor_row = (thread_in_warp % 4) * 2
    row = n_tile * MARLIN_TILE_N + tile_warp * 16 + tensor_col
    col = k_tile * MARLIN_TILE_K + tensor_row

    value0 = _nvfp4_nibble(packed, n_cols, row, col)
    value1 = _nvfp4_nibble(packed, n_cols, row, col + 8)
    value2 = _nvfp4_nibble(packed, n_cols, row + 8, col)
    value3 = _nvfp4_nibble(packed, n_cols, row + 8, col + 8)
    value4 = _nvfp4_nibble(packed, n_cols, row, col + 1)
    value5 = _nvfp4_nibble(packed, n_cols, row, col + 9)
    value6 = _nvfp4_nibble(packed, n_cols, row + 8, col + 1)
    value7 = _nvfp4_nibble(packed, n_cols, row + 8, col + 9)
    result = (
        value0
        | value1 << 4
        | value2 << 8
        | value3 << 12
        | value4 << 16
        | value5 << 20
        | value6 << 24
        | value7 << 28
    )
    output = target.bitcast[UInt32]()
    output[tile * 128 + tid] = result


def repack_nvfp4_ct_marlin_scales(
    target: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[UInt8, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Stosuje scale_perm Marlin i kodowanie E4M3 do S0E5M3."""
    output_index = (
        Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    )
    scale_groups = n_cols // 16
    scale_bytes = n_rows * scale_groups
    if output_index >= scale_bytes:
        return
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
    source_group = transposed_index // n_rows
    source_row = transposed_index % n_rows
    target[output_index] = nvfp4_ct_s0_from_e4m3(
        scales[source_row * scale_groups + source_group]
    )
