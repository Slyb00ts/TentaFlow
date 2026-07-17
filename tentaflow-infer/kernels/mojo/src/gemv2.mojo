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


def _q4k_scale_min(scales: SIMD[DType.uint8, 16], j: Int) -> Tuple[Float32, Float32]:
    # llama.cpp get_scale_min_k4 over the 12 packed bytes (scales[0] sits at
    # SIMD index 4: the vector is the raw 16-byte block header d|dmin|scales).
    # Branch-free select: lanes hit both j ranges in the same warp iteration,
    # so an if would serialize both paths anyway.
    lt = Int(j < 4)
    sc_lo = Int(scales[4 + j] & 63)
    mn_lo = Int(scales[8 + j] & 63)
    sc_hi = Int((scales[8 + j] & 0x0F) | ((scales[j] >> 6) << 4))
    mn_hi = Int((scales[8 + j] >> 4) | ((scales[4 + j] >> 6) << 4))
    return (
        Float32(lt * sc_lo + (1 - lt) * sc_hi),
        Float32(lt * mn_lo + (1 - lt) * mn_hi),
    )


comptime Q4K_MAX_SEGS = 1024  # 32-col x segments -> n_cols <= 32768


def _gemv_q4_k_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    xsum: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    lut: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """Per-lane partial dot product for one Q4_K row.

    Work unit is a 64-element quarter of a 256-element superblock: lanes
    stride quarters, so 4096-column rows keep all 32 lanes busy (64 quarters)
    instead of idling half the warp on whole superblocks. The 144-byte block
    is 16-byte aligned (144 % 16 == 0 and rows start at block boundaries), so
    the header is one 16-byte load and each quarter's nibbles one 32-byte load;
    the 4-lane header re-read coalesces to a single transaction.

    value = d*sc*q - dmin*mn splits into a q-dependent dot (vector-accumulated,
    one reduction per row) and a min term that only needs per-segment x sums —
    those come precomputed in shared memory (`xsum`), removing half the
    per-quarter reduction work.
    """
    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 144
    halves = blocks_per_row * 2

    var acc: Float32 = 0.0
    var min_acc: Float32 = 0.0
    var c = lane
    while c < halves:
        b = c // 2
        q = (c % 2) * 2  # first quarter of this half-superblock
        off = row_base + b * 144
        hdr = (w + off).load[width=16, alignment=16]()
        dm = bitcast[DType.float16, 8](hdr)
        d = Float32(dm[0])
        dmin = Float32(dm[1])

        comptime for qq in range(2):
            sc1, mn1 = _q4k_scale_min(hdr, 2 * (q + qq))
            sc2, mn2 = _q4k_scale_min(hdr, 2 * (q + qq) + 1)
            qv = (w + off + 16 + (q + qq) * 32).load[width=32, alignment=16]()
            # Nibble -> f32 through the 16-entry shared LUT: shared loads
            # issue on the LSU pipe and overlap the FP math, unlike I2F
            # conversions (same trade as gemv_nvfp4).
            col = (c * 2 + qq) * 64
            # 8-wide LUT gathers with 8-wide dot accumulators: 32-wide
            # gathered vectors get materialized in local memory (stack frame),
            # 8-wide ones stay in registers (same shape as gemv_nvfp4).
            var dot0 = SIMD[DType.float32, 8](0.0)
            var dot1 = SIMD[DType.float32, 8](0.0)
            comptime for k in range(4):
                q8 = qv.slice[8, offset = k * 8]()
                var lov = SIMD[DType.float32, 8]()
                var hiv = SIMD[DType.float32, 8]()
                comptime for j in range(8):
                    lov[j] = lut[Int(q8[j] & 0x0F)]
                    hiv[j] = lut[Int(q8[j] >> 4)]
                x0 = (x + col + k * 8).load[width=8, alignment=16]().cast[DType.float32]()
                x1 = (x + col + 32 + k * 8).load[width=8, alignment=16]().cast[DType.float32]()
                dot0 += lov * x0
                dot1 += hiv * x1
            acc += dot0.reduce_add() * (d * sc1) + dot1.reduce_add() * (d * sc2)
            seg = (c * 2 + qq) * 2
            min_acc += dmin * (mn1 * xsum[seg] + mn2 * xsum[seg + 1])
        c += WARP
    return acc - min_acc


def _q4k_fill_xsum(
    x: UnsafePointer[Float16, MutAnyOrigin],
    xsum: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    tid: Int,
):
    """Stage per-32-column x segment sums to shared memory (whole block)."""
    segs = n_cols // 32
    var s = tid
    while s < segs:
        xv = (x + s * 32).load[width=32, alignment=64]().cast[DType.float32]()
        xsum[s] = xv.reduce_add()
        s += 256
    barrier()


def gemv_q4_k_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q4_K GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256,
    n_cols % 256 == 0 (whole superblocks per row), n_cols <= 32768."""
    tid = Int(thread_idx.x)
    xsum = stack_allocation[
        Q4K_MAX_SEGS, Float32, address_space = AddressSpace.SHARED
    ]()
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    if tid < 16:
        lut[tid] = Float32(tid)
    _q4k_fill_xsum(x, xsum, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q4_k_row_acc(w, x, xsum, lut, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_q4_k_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q4_K logit GEMV (f32 out), warp-per-row."""
    tid = Int(thread_idx.x)
    xsum = stack_allocation[
        Q4K_MAX_SEGS, Float32, address_space = AddressSpace.SHARED
    ]()
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    if tid < 16:
        lut[tid] = Float32(tid)
    _q4k_fill_xsum(x, xsum, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q4_k_row_acc(w, x, xsum, lut, n_cols, row, lane))
    if lane == 0:
        y[row] = total


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
