# =============================================================================
# Plik: decode_dp4a_amd.mojo
# Opis: Warianty dekodowania Q4_K dp4a dobrane do rozkładu pamięci RDNA.
# Przykład: gemv_q4_k_dp4a_amd_f16(y, w, x, cols, rows)
# =============================================================================

from std.gpu import block_idx, grid_dim, thread_idx
from std.gpu.primitives import warp
from std.gpu.primitives.warp import shuffle_xor
from std.gpu.memory import AddressSpace
from std.memory import bitcast, stack_allocation
from src.decode_dp4a import _dp4a, _q4k_scale_pair, _stage_quant_global

comptime WARP = 32
comptime ROWS_PER_BLOCK = 8
comptime X_MAX = 16384
comptime XDS_MAX = X_MAX // 32 * 2
# Ten wariant jest zbudowany wyłącznie dla gfx12; profil gfx1201 wybiera unroll=4.
comptime DOT_UNROLL = 4


def _dot_q4k_i8_amd(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xq: UnsafePointer[
        Int8, MutUntrackedOrigin, address_space=AddressSpace.SHARED
    ],
    xds: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space=AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 144
    i = lane % 16
    j = i // 4
    w4 = i % 4
    xqi = xq.bitcast[Int32]()

    var acc: Float32 = 0.0
    var min_acc: Float32 = 0.0
    var b = 0
    while b + (DOT_UNROLL - 1) < blocks_per_row:
        var dm = InlineArray[SIMD[DType.float16, 2], DOT_UNROLL](
            fill=SIMD[DType.float16, 2](0)
        )
        var v0 = InlineArray[Int32, DOT_UNROLL](fill=0)
        var v1 = InlineArray[Int32, DOT_UNROLL](fill=0)
        var scs = InlineArray[SIMD[DType.float32, 2], DOT_UNROLL](
            fill=SIMD[DType.float32, 2](0)
        )
        var mns = InlineArray[SIMD[DType.float32, 2], DOT_UNROLL](
            fill=SIMD[DType.float32, 2](0)
        )
        comptime for u in range(DOT_UNROLL):
            off = row_base + (b + u) * 144
            dm[u] = (w + off).bitcast[Float16]().load[width=2, alignment=4]()
            qp = (w + off + 16 + j * 32 + 4 * w4).bitcast[Int32]()
            v0[u] = qp[0]
            v1[u] = qp[4]
            scs[u], mns[u] = _q4k_scale_pair(w, off, j)
        comptime for u in range(DOT_UNROLL):
            seg = (b + u) * 8 + 2 * j
            u0a = xqi[seg * 8 + w4]
            u0b = xqi[seg * 8 + w4 + 4]
            u1a = xqi[seg * 8 + 8 + w4]
            u1b = xqi[seg * 8 + 12 + w4]
            s_lo = _dp4a(
                v1[u] & 0x0F0F0F0F, u0b, _dp4a(v0[u] & 0x0F0F0F0F, u0a, 0)
            )
            s_hi = _dp4a(
                (v1[u] >> 4) & 0x0F0F0F0F,
                u1b,
                _dp4a((v0[u] >> 4) & 0x0F0F0F0F, u1a, 0),
            )
            d = Float32(dm[u][0])
            acc += d * scs[u][0] * xds[2 * seg] * Float32(s_lo)
            acc += d * scs[u][1] * xds[2 * seg + 2] * Float32(s_hi)
            if w4 == 0:
                min_acc += Float32(dm[u][1]) * (
                    mns[u][0] * xds[2 * seg + 1] + mns[u][1] * xds[2 * seg + 3]
                )
        b += DOT_UNROLL

    while b < blocks_per_row:
        off = row_base + b * 144
        dm = (w + off).bitcast[Float16]().load[width=2, alignment=4]()
        sc, mn = _q4k_scale_pair(w, off, j)
        qp = (w + off + 16 + j * 32 + 4 * w4).bitcast[Int32]()
        lo0 = qp[0] & 0x0F0F0F0F
        lo1 = qp[4] & 0x0F0F0F0F
        hi0 = (qp[0] >> 4) & 0x0F0F0F0F
        hi1 = (qp[4] >> 4) & 0x0F0F0F0F
        seg = b * 8 + 2 * j
        u0a = xqi[seg * 8 + w4]
        u0b = xqi[seg * 8 + w4 + 4]
        u1a = xqi[seg * 8 + 8 + w4]
        u1b = xqi[seg * 8 + 12 + w4]
        s_lo = _dp4a(lo1, u0b, _dp4a(lo0, u0a, 0))
        s_hi = _dp4a(hi1, u1b, _dp4a(hi0, u1a, 0))
        acc += Float32(dm[0]) * sc[0] * xds[2 * seg] * Float32(s_lo)
        acc += Float32(dm[0]) * sc[1] * xds[2 * seg + 2] * Float32(s_hi)
        if w4 == 0:
            min_acc += Float32(dm[1]) * (
                mn[0] * xds[2 * seg + 1] + mn[1] * xds[2 * seg + 3]
            )
        b += 1
    var total = acc - min_acc
    comptime for span in [1, 2, 4, 8]:
        total += shuffle_xor(total, UInt32(span))
    return total


def gemv_q4_k_dp4a_amd_u4_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    tid = Int(thread_idx.x)
    xq = stack_allocation[
        X_MAX, Int8, alignment=64, address_space=AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XDS_MAX, Float32, address_space=AddressSpace.SHARED
    ]()
    _stage_quant_global(x, xq, xds, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = _dot_q4k_i8_amd(w, row, xq, xds, n_cols, lane)
    if lane == 0:
        y[row] = Float16(total)


@always_inline
def gemv_q4_k_dp4a_amd_u4_persist_impl[
    XCAP: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    tid = Int(thread_idx.x)
    xq = stack_allocation[
        XCAP, Int8, alignment=64, address_space=AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XCAP // 32 * 2, Float32, address_space=AddressSpace.SHARED
    ]()
    _stage_quant_global(x, xq, xds, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    var row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    stride = Int(grid_dim.x) * ROWS_PER_BLOCK
    while row < n_rows:
        total = _dot_q4k_i8_amd(w, row, xq, xds, n_cols, lane)
        if lane == 0:
            y[row] = Float16(total)
        row += stride


comptime gemv_q4_k_dp4a_amd_u4_persist_f16 = gemv_q4_k_dp4a_amd_u4_persist_impl[
    X_MAX
]
comptime gemv_q4_k_dp4a_amd_u4_persist_x4k_f16 = gemv_q4_k_dp4a_amd_u4_persist_impl[
    4096
]


def gemv_q4_k_dp4a_amd_u4_group4_f16(
    y0: UnsafePointer[Float16, MutAnyOrigin],
    w0: UnsafePointer[UInt8, MutAnyOrigin],
    rows0: Int,
    y1: UnsafePointer[Float16, MutAnyOrigin],
    w1: UnsafePointer[UInt8, MutAnyOrigin],
    rows1: Int,
    y2: UnsafePointer[Float16, MutAnyOrigin],
    w2: UnsafePointer[UInt8, MutAnyOrigin],
    rows2: Int,
    y3: UnsafePointer[Float16, MutAnyOrigin],
    w3: UnsafePointer[UInt8, MutAnyOrigin],
    rows3: Int,
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
):
    tid = Int(thread_idx.x)
    xq = stack_allocation[
        X_MAX, Int8, alignment=64, address_space=AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XDS_MAX, Float32, address_space=AddressSpace.SHARED
    ]()
    _stage_quant_global(x, xq, xds, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP

    var tile = Int(block_idx.x)
    blocks0 = (rows0 + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK
    blocks1 = (rows1 + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK
    blocks2 = (rows2 + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK

    if tile < blocks0:
        row0 = tile * ROWS_PER_BLOCK + wid
        if row0 < rows0:
            total0 = _dot_q4k_i8_amd(w0, row0, xq, xds, n_cols, lane)
            if lane == 0:
                y0[row0] = Float16(total0)
        return
    tile -= blocks0
    if tile < blocks1:
        row1 = tile * ROWS_PER_BLOCK + wid
        if row1 < rows1:
            total1 = _dot_q4k_i8_amd(w1, row1, xq, xds, n_cols, lane)
            if lane == 0:
                y1[row1] = Float16(total1)
        return
    tile -= blocks1
    if tile < blocks2:
        row2 = tile * ROWS_PER_BLOCK + wid
        if row2 < rows2:
            total2 = _dot_q4k_i8_amd(w2, row2, xq, xds, n_cols, lane)
            if lane == 0:
                y2[row2] = Float16(total2)
        return
    tile -= blocks2
    row3 = tile * ROWS_PER_BLOCK + wid
    if row3 < rows3:
        total3 = _dot_q4k_i8_amd(w3, row3, xq, xds, n_cols, lane)
        if lane == 0:
            y3[row3] = Float16(total3)
