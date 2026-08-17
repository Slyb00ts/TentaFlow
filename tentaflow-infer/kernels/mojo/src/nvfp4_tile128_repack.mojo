# =============================================================================
# Plik: nvfp4_tile128_repack.mojo
# Opis: Przepakowuje wagi NVFP4 RowMajor36 do ukladu tile-major N128/K64 na GPU.
# Przyklad: nvfp4_repack_tile128 przetwarza jeden etap K32 na blok CUDA.
# =============================================================================

from std.gpu import block_idx, thread_idx

comptime TILE_ROWS = 128
comptime TILE_BLOCK_BYTES = TILE_ROWS * 36
comptime STAGE_BYTES = TILE_ROWS * 18


def nvfp4_repack_tile128(
    target: UnsafePointer[UInt8, MutAnyOrigin],
    source: UnsafePointer[UInt8, MutAnyOrigin],
    blocks_per_row: Int,
):
    layout_thread = Int(thread_idx.x)
    stage_linear = Int(block_idx.x)
    stage = stage_linear % 2
    tile_block = stage_linear // 2
    block = tile_block % blocks_per_row
    tile = tile_block // blocks_per_row
    row = layout_thread // 2
    subblock = layout_thread % 2
    group = stage * 2 + subblock
    raw_base = ((tile * TILE_ROWS + row) * blocks_per_row + block) * 36
    target_base = tile_block * TILE_BLOCK_BYTES + stage * STAGE_BYTES
    target[target_base + layout_thread] = source[raw_base + group]
    codes = (source + raw_base + 4 + group * 8).load[width=8, alignment=4]()
    (target + target_base + 256 + layout_thread * 8).store[
        width=8, alignment=8
    ](codes)

