# =============================================================================
# Plik: gemm_q8_triplet_variants.mojo
# Opis: Zawiera izolowane warianty tripletu Q8 z pojedynczym wywołaniem kafla
#       po wyborze projekcji oraz geometriami 64x64 i 128x128.
# Przykład: gemm_q8_0_i8mma_triplet_single_big obsługuje trzy projekcje Q8.
# =============================================================================

from std.gpu import block_idx
from src.gemm import gemm_i8mma_tile_impl


@always_inline
def _selected_triplet_tile[BN: Int](
    tile: Int,
    y0: UnsafePointer[Float16, MutAnyOrigin],
    w0: UnsafePointer[UInt8, ImmutAnyOrigin],
    n_rows0: Int,
    y1: UnsafePointer[Float16, MutAnyOrigin],
    w1: UnsafePointer[UInt8, ImmutAnyOrigin],
    n_rows1: Int,
    y2: UnsafePointer[Float16, MutAnyOrigin],
    w2: UnsafePointer[UInt8, ImmutAnyOrigin],
    n_rows2: Int,
) -> Tuple[
    UnsafePointer[Float16, MutAnyOrigin],
    UnsafePointer[UInt8, ImmutAnyOrigin],
    Int,
    Int,
]:
    """Wybiera projekcję i lokalny początek wierszy przed alokacją kafla."""
    blocks0 = (n_rows0 + BN - 1) // BN
    blocks1 = (n_rows1 + BN - 1) // BN
    if tile < blocks0:
        return (y0, w0, n_rows0, tile * BN)
    if tile < blocks0 + blocks1:
        return (y1, w1, n_rows1, (tile - blocks0) * BN)
    return (y2, w2, n_rows2, (tile - blocks0 - blocks1) * BN)


def gemm_q8_0_i8mma_triplet_single_bm64(
    y0: UnsafePointer[Float16, MutAnyOrigin],
    w0: UnsafePointer[UInt8, ImmutAnyOrigin],
    n_rows0: Int,
    y1: UnsafePointer[Float16, MutAnyOrigin],
    w1: UnsafePointer[UInt8, ImmutAnyOrigin],
    n_rows1: Int,
    y2: UnsafePointer[Float16, MutAnyOrigin],
    w2: UnsafePointer[UInt8, ImmutAnyOrigin],
    n_rows2: Int,
    xq: UnsafePointer[Int8, ImmutAnyOrigin],
    xd: UnsafePointer[Float32, ImmutAnyOrigin],
    xsm: UnsafePointer[Float32, ImmutAnyOrigin],
    n_cols: Int,
    n_tokens: Int,
):
    """Liczy triplet kaflem 64x64 po jednorazowym wyborze projekcji."""
    comptime BM = 64
    comptime BN = 64
    tile = Int(block_idx.x)
    selected = _selected_triplet_tile[BN](
        tile, y0, w0, n_rows0, y1, w1, n_rows1, y2, w2, n_rows2
    )
    gemm_i8mma_tile_impl[BM, BN, 8, 0](
        selected[0], selected[1].unsafe_mut_cast[True](),
        xq.unsafe_mut_cast[True](), xd.unsafe_mut_cast[True](),
        xsm.unsafe_mut_cast[True](), n_cols, selected[2], n_tokens,
        selected[3], Int(block_idx.y) * BM, n_tokens,
    )


def gemm_q8_0_i8mma_triplet_single_big(
    y0: UnsafePointer[Float16, MutAnyOrigin],
    w0: UnsafePointer[UInt8, ImmutAnyOrigin],
    n_rows0: Int,
    y1: UnsafePointer[Float16, MutAnyOrigin],
    w1: UnsafePointer[UInt8, ImmutAnyOrigin],
    n_rows1: Int,
    y2: UnsafePointer[Float16, MutAnyOrigin],
    w2: UnsafePointer[UInt8, ImmutAnyOrigin],
    n_rows2: Int,
    xq: UnsafePointer[Int8, ImmutAnyOrigin],
    xd: UnsafePointer[Float32, ImmutAnyOrigin],
    xsm: UnsafePointer[Float32, ImmutAnyOrigin],
    n_cols: Int,
    n_tokens: Int,
):
    """Liczy triplet kaflem 128x128 i szesnastoma warpami."""
    comptime BM = 128
    comptime BN = 128
    tile = Int(block_idx.x)
    selected = _selected_triplet_tile[BN](
        tile, y0, w0, n_rows0, y1, w1, n_rows1, y2, w2, n_rows2
    )
    gemm_i8mma_tile_impl[BM, BN, 16, 0](
        selected[0], selected[1].unsafe_mut_cast[True](),
        xq.unsafe_mut_cast[True](), xd.unsafe_mut_cast[True](),
        xsm.unsafe_mut_cast[True](), n_cols, selected[2], n_tokens,
        selected[3], Int(block_idx.y) * BM, n_tokens,
    )
