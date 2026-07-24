# =============================================================================
# Plik: q8_single_big_poststage.mojo
# Opis: Produkcyjny Q8 single_big z pojedynczym raw i stagingiem po MMA.
# Przyklad: gemm_q8_0_i8mma_triplet_single_big_poststage liczy trzy projekcje Q8.
# =============================================================================

from std.gpu import block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.compute.mma import ld_matrix
from std.gpu.memory import AddressSpace
from std.memory import bitcast, stack_allocation
from src.gemm import _mma_s8

comptime BM = 128
comptime BN = 128
comptime NW = 16
comptime WARP = 32
comptime BK = 32
comptime MT_PER_WARP = 2
comptime M_WARPS = BM // 32
comptime N_WARPS = NW // M_WARPS
comptime NT_PER_WARP = (BN // 8) // N_WARPS
comptime NTHREADS = NW * WARP


@always_inline
def _selected_tile(
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
    blocks0 = (n_rows0 + BN - 1) // BN
    blocks1 = (n_rows1 + BN - 1) // BN
    if tile < blocks0:
        return (y0, w0, n_rows0, tile * BN)
    if tile < blocks0 + blocks1:
        return (y1, w1, n_rows1, (tile - blocks0) * BN)
    return (y2, w2, n_rows2, (tile - blocks0 - blocks1) * BN)


def _gemm_q8_tile_poststage(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    xq_g: UnsafePointer[Int8, MutAnyOrigin],
    xd_g: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    row0: Int,
    t0: Int,
):
    tid = Int(thread_idx.x)
    xq = stack_allocation[
        2 * BM * BK, Int8, alignment=64, address_space=AddressSpace.SHARED
    ]()
    wq = stack_allocation[
        2 * BN * BK, Int8, alignment=64, address_space=AddressSpace.SHARED
    ]()
    xd = stack_allocation[2 * BM, Float32, address_space=AddressSpace.SHARED]()
    wd = stack_allocation[2 * BN, Float32, address_space=AddressSpace.SHARED]()

    var token = t0 + tid
    if token > n_tokens - 1:
        token = n_tokens - 1
    row_l = tid // 4
    part = tid % 4
    blocks_per_row = n_cols // BK
    var weight_row = row0 + row_l
    if weight_row > n_rows - 1:
        weight_row = n_rows - 1
    weight_base = weight_row * blocks_per_row * 34
    stages = n_cols // BK

    wid = tid // WARP
    lane = tid % WARP
    group = lane >> 2
    lane4 = lane & 3
    sub = lane // 8
    lane8 = lane % 8
    warp_m = (wid % M_WARPS) * 32
    warp_n = wid // M_WARPS

    var accumulators = InlineArray[
        SIMD[DType.float32, 4], MT_PER_WARP * NT_PER_WARP
    ](fill=SIMD[DType.float32, 4](0.0))
    var activation_codes = SIMD[DType.int8, 32](0)
    var activation_scale = Float32(0.0)
    var weight_codes = SIMD[DType.uint8, 8](0)
    var weight_scale = Float16(0)

    @parameter
    @always_inline
    def load_raw(stage: Int):
        if tid < BM:
            activation_codes = (
                xq_g + token * n_cols + stage * BK
            ).load[width=32, alignment=32]()
            activation_scale = xd_g[stage * n_tokens + token]
        weight_codes = (
            w + weight_base + stage * 34 + 2 + part * 8
        ).load[width=8, alignment=2]()
        if part == 0:
            weight_scale = (
                w + weight_base + stage * 34
            ).bitcast[Float16]()[0]

    @parameter
    @always_inline
    def store_raw(buffer: Int):
        if tid < BM:
            (xq + buffer * BM * BK + tid * BK).store[alignment=32](
                activation_codes
            )
            xd[buffer * BM + tid] = activation_scale
        (wq + buffer * BN * BK + row_l * BK + part * 8).store[alignment=8](
            weight_codes.cast[DType.int8]()
        )
        if part == 0:
            wd[buffer * BN + row_l] = Float32(weight_scale)

    load_raw(0)
    store_raw(0)
    barrier()

    var stage = 0
    while stage < stages:
        buffer = stage % 2
        activation_fragment = (
            xq + buffer * BM * BK + warp_m * BK
        ).bitcast[Float16]()
        var a = InlineArray[SIMD[DType.uint32, 4], MT_PER_WARP](
            fill=SIMD[DType.uint32, 4](0)
        )
        var da = InlineArray[SIMD[DType.float32, 4], MT_PER_WARP](
            fill=SIMD[DType.float32, 4](0)
        )
        comptime for m_tile in range(MT_PER_WARP):
            a_base = activation_fragment + (
                (m_tile * 16 + (sub % 2) * 8 + lane8) * 16
                + (sub // 2) * 8
            )
            a[m_tile] = bitcast[DType.uint32, 4](ld_matrix[8](a_base))
            da0 = xd[buffer * BM + warp_m + m_tile * 16 + group]
            da1 = xd[buffer * BM + warp_m + m_tile * 16 + group + 8]
            da[m_tile] = SIMD[DType.float32, 4](da0, da0, da1, da1)

        comptime for n_tile in range(NT_PER_WARP):
            row_tile = (warp_n * NT_PER_WARP + n_tile) * 8
            weight_fragment = (
                wq + buffer * BN * BK + row_tile * BK
            ).bitcast[Float16]()
            b_base = weight_fragment + lane8 * 16 + (sub % 2) * 8
            b = bitcast[DType.uint32, 2](ld_matrix[4](b_base))
            dw2 = (wd + buffer * BN + row_tile + 2 * lane4).load[width=2]()
            dw = SIMD[DType.float32, 4](dw2[0], dw2[1], dw2[0], dw2[1])
            comptime for m_tile in range(MT_PER_WARP):
                dot = _mma_s8(
                    a[m_tile][0], a[m_tile][1], a[m_tile][2], a[m_tile][3],
                    b[0], b[1], SIMD[DType.int32, 4](0),
                )
                accumulators[m_tile * NT_PER_WARP + n_tile] += (
                    da[m_tile] * dw * dot.cast[DType.float32]()
                )

        if stage + 1 < stages:
            next = (stage + 1) % 2
            load_raw(stage + 1)
            store_raw(next)
            barrier()
        stage += 1

    comptime for m_tile in range(MT_PER_WARP):
        token0 = t0 + warp_m + m_tile * 16 + group
        token1 = token0 + 8
        comptime for n_tile in range(NT_PER_WARP):
            row_tile = (warp_n * NT_PER_WARP + n_tile) * 8
            row_a = row0 + row_tile + 2 * lane4
            row_b = row_a + 1
            value = accumulators[m_tile * NT_PER_WARP + n_tile]
            if token0 < n_tokens:
                if row_a < n_rows:
                    y[token0 * n_rows + row_a] = Float16(value[0])
                if row_b < n_rows:
                    y[token0 * n_rows + row_b] = Float16(value[1])
            if token1 < n_tokens:
                if row_a < n_rows:
                    y[token1 * n_rows + row_a] = Float16(value[2])
                if row_b < n_rows:
                    y[token1 * n_rows + row_b] = Float16(value[3])


def gemm_q8_0_i8mma_triplet_single_big_poststage(
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
    _ = xsm
    tile = Int(block_idx.x)
    selected = _selected_tile(
        tile, y0, w0, n_rows0, y1, w1, n_rows1, y2, w2, n_rows2
    )
    _gemm_q8_tile_poststage(
        selected[0], selected[1].unsafe_mut_cast[True](),
        xq.unsafe_mut_cast[True](), xd.unsafe_mut_cast[True](),
        n_cols, selected[2], n_tokens, selected[3], Int(block_idx.y) * BM,
    )
