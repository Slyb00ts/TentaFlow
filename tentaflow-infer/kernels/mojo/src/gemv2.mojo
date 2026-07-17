# ===== File: gemv2.mojo — warp-per-row fused dequant GEMV (bandwidth-oriented) =====
# v2 decomposition: 8 warps per block, one output row per warp, lanes stride
# quant blocks with wide loads and independent accumulators. Compared to the
# block-per-row v1 this removes the shared-memory block reduction, keeps every
# lane busy (v1 idled half its threads on 4k-column rows), and exposes 4-8
# independent loads per lane for the memory pipeline.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.primitives import warp
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import bitcast, stack_allocation

comptime WARP = 32
comptime ROWS_PER_BLOCK = 8


def gemv_q8_0_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q8_0 GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256.

    The 34-byte block layout is only 2-byte aligned, so the 32 int8 values are
    fetched as sixteen u16 lanes and reinterpreted — same trade llama.cpp
    makes; throughput comes from utilization, not load width.
    """
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return

    blocks_per_row = n_cols // 32
    row_base = row * blocks_per_row * 34

    var acc: Float32 = 0.0
    var b = lane
    while b < blocks_per_row:
        off = row_base + b * 34
        scale = Float32((w + off).bitcast[Float16]()[0])
        v16 = (w + off + 2).bitcast[UInt16]().load[width=16]()
        q = bitcast[DType.int8, 32](v16).cast[DType.float32]()
        xv = (x + b * 32).load[width=32, alignment=64]().cast[DType.float32]()
        acc += scale * (q * xv).reduce_add()
        b += WARP

    total = warp.sum(acc)
    if lane == 0:
        y[row] = Float16(total)


def gemv_q8_0_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q8_0 logit GEMV (f32 out), warp-per-row."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return

    blocks_per_row = n_cols // 32
    row_base = row * blocks_per_row * 34

    var acc: Float32 = 0.0
    var b = lane
    while b < blocks_per_row:
        off = row_base + b * 34
        scale = Float32((w + off).bitcast[Float16]()[0])
        v16 = (w + off + 2).bitcast[UInt16]().load[width=16]()
        q = bitcast[DType.int8, 32](v16).cast[DType.float32]()
        xv = (x + b * 32).load[width=32, alignment=64]().cast[DType.float32]()
        acc += scale * (q * xv).reduce_add()
        b += WARP

    total = warp.sum(acc)
    if lane == 0:
        y[row] = total


def _e2m1x8(codes: SIMD[DType.uint8, 8]) -> SIMD[DType.float32, 8]:
    # Branch-free e2m1 magnitude (0,.5,1,1.5,2,3,4,6): 4*mag = m*2 for m<2 else
    # (2+(m&1))<<(m>>1); sign from bit 3. SIMD comparisons collapse to scalar
    # Bool in this toolchain, so masks are arithmetic.
    m = (codes & 0x07).cast[DType.int32]()
    is_small = ((m - 2) >> 31) & 1
    mag4 = is_small * (m * 2) + (1 - is_small) * ((2 + (m & 1)) << (m >> 1))
    sign = ((codes >> 3) & 1).cast[DType.float32]() * -2.0 + 1.0
    return mag4.cast[DType.float32]() * 0.25 * sign


def _f8e4m3s(b: UInt8) -> Float32:
    var sign: Float32 = 1.0
    if (b & 0x80) != 0:
        sign = -1.0
    e = Int((b >> 3) & 0x0F)
    man = Float32(Int(b & 0x07))
    if e == 0:
        return sign * man * (1.0 / 512.0)
    bits = UInt32(e - 7 + 127) << 23
    scale = UnsafePointer(to=bits).bitcast[Float32]()[0]
    return sign * (1.0 + man / 8.0) * scale


def gemv_nvfp4_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    inv_global_scale: Float32,
):
    """NVFP4 GEMV, warp-per-row; one aligned 8-byte load covers a scale group.

    e2m1 decode goes through a 16-entry shared-memory LUT: 16 entries span 16
    banks, so lanes hitting the same code broadcast and distinct codes hit
    distinct banks — conflict-free, and far cheaper than the arithmetic
    decode (708 -> 908 GB/s on RTX 4090).
    """
    tid = Int(thread_idx.x)
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    comptime e2m1_vals = SIMD[DType.float32, 16](
        0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0,
        -0.0, -0.5, -1.0, -1.5, -2.0, -3.0, -4.0, -6.0,
    )
    if tid < 16:
        lut[tid] = e2m1_vals[tid]
    barrier()

    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return

    groups = n_cols // 16
    packed_row = row * (n_cols // 2)
    scales_row = row * groups

    var acc: Float32 = 0.0
    var g = lane
    while g < groups:
        s = _f8e4m3s(scales[scales_row + g]) * inv_global_scale
        qv = (packed + packed_row + g * 8).load[width=8, alignment=8]()
        xv = (x + g * 16).load[width=16, alignment=32]().cast[DType.float32]()
        x_even, x_odd = xv.deinterleave()
        var lov = SIMD[DType.float32, 8]()
        var hiv = SIMD[DType.float32, 8]()
        comptime for j in range(8):
            lov[j] = lut[Int(qv[j] & 0x0F)]
            hiv[j] = lut[Int(qv[j] >> 4)]
        acc += s * ((lov * x_even).reduce_add() + (hiv * x_odd).reduce_add())
        g += WARP

    total = warp.sum(acc)
    if lane == 0:
        y[row] = Float16(total)


def gemv_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """f16 GEMV, warp-per-row, 8-element vector loads (requires n_cols % 256 == 0
    handled by caller padding; lanes guard the tail)."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return

    base = row * n_cols
    var acc: Float32 = 0.0
    var i = lane * 8
    stride = WARP * 8
    while i + 8 <= n_cols:
        wv = (w + base + i).load[width=8, alignment=16]().cast[DType.float32]()
        xv = (x + i).load[width=8, alignment=16]().cast[DType.float32]()
        acc += (wv * xv).reduce_add()
        i += stride

    total = warp.sum(acc)
    if lane == 0:
        y[row] = Float16(total)


def gemv_f16_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """f16 logit GEMV (f32 out), warp-per-row."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return

    base = row * n_cols
    var acc: Float32 = 0.0
    var i = lane * 8
    stride = WARP * 8
    while i + 8 <= n_cols:
        wv = (w + base + i).load[width=8, alignment=16]().cast[DType.float32]()
        xv = (x + i).load[width=8, alignment=16]().cast[DType.float32]()
        acc += (wv * xv).reduce_add()
        i += stride

    total = warp.sum(acc)
    if lane == 0:
        y[row] = total
