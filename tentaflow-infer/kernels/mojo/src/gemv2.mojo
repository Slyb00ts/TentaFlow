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
from src.arch_dot import f8e4m3_to_f32, f8e4m3x2_to_f16x2

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


@always_inline
def _e2m1x8_f16(codes: SIMD[DType.uint8, 8]) -> SIMD[DType.float16, 8]:
    """Osiem kodow e2m1 wprost na wzorzec bitowy f16, bez konwersji int->float.

    e2m1 to znak, dwubitowy wykladnik z biasem 1 i jednobitowa mantysa, wiec f16
    (bias 15) powstaje przesunieciem pol: `exp16 = E + 14`, `mant16 = M << 9`.
    DWA wyjatki: zero (maskujemy calosc) oraz jedyna wartosc subnormalna 0.5
    (E=0, M=1), ktora w f16 jest normalna `0x3800`, czyli MA ZEROWA mantyse —
    dlatego bit mantysy przepuszczamy tylko dla E > 0.

    Wersja arytmetyczna (`_e2m1x8`) liczy magnitude maskami na int32 i konczy
    konwersja na float — okolo dwa razy wiecej operacji wektorowych na wartosc.
    W GEMV NVFP4 to wlasnie dekwantyzacja, a nie pamiec, byla scianka.
    """
    m = (codes & 0x07).cast[DType.uint16]()
    exponent = m >> 1
    # `(exponent + 3) >> 2` to arytmetyczny wskaznik „E > 0" (0 dla E=0, 1 dalej).
    mantissa = (m & 1) & ((exponent + 3) >> 2)
    bits = ((exponent + 14) << 10) | (mantissa << 9)
    # Porownania SIMD zapadaja sie w tym toolchainie do skalarnego Bool, wiec
    # maska „m != 0" musi byc arytmetyczna: (m + 7) >> 3 daje 0 dla zera i 1 dla
    # kazdego innego kodu, a odjecie od zera rozciaga to na pelne 0xFFFF.
    nonzero = SIMD[DType.uint16, 8](0) - ((m + 7) >> 3)
    sign = ((codes >> 3) & 1).cast[DType.uint16]() << 15
    return bitcast[DType.float16, 8]((bits & nonzero) | sign)


comptime _f8e4m3s = f8e4m3_to_f32


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


def _gemv_q6_k_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """Per-lane partial dot product for one Q6_K row (210-byte superblock:
    ql[128] low nibbles, qh[64] 2-bit highs, 16 int8 scales, d f16; value =
    d * sc * ((ql | qh<<4) - 32), dequant.rs dq_q6_k semantics).

    Work unit is a 128-element half of a 256-element superblock: lanes stride
    halves so 4096-column rows keep all 32 lanes busy. The 210-byte block is
    only 2-byte aligned, so ql/qh/scales load as u16 lanes and reinterpret
    (same trade as gemv_q8_0). Values decode arithmetically — 6-bit codes have
    no compact LUT and the I2F converts overlap the loads.
    """
    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 210
    halves = blocks_per_row * 2

    var acc: Float32 = 0.0
    var c = lane
    while c < halves:
        b = c // 2
        n = c % 2
        off = row_base + b * 210
        d = Float32((w + off + 208).bitcast[Float16]()[0])
        sc16 = (w + off + 192 + n * 8).bitcast[UInt16]().load[width=4]()
        sc = bitcast[DType.int8, 8](sc16).cast[DType.float32]()
        ql16a = (w + off + n * 64).bitcast[UInt16]().load[width=16]()
        ql16b = (w + off + n * 64 + 32).bitcast[UInt16]().load[width=16]()
        qh16 = (w + off + 128 + n * 32).bitcast[UInt16]().load[width=16]()
        ql_a = bitcast[DType.uint8, 32](ql16a)
        ql_b = bitcast[DType.uint8, 32](ql16b)
        qh = bitcast[DType.uint8, 32](qh16)

        col = c * 128
        x1 = (x + col).load[width=32, alignment=64]().cast[DType.float32]()
        x2 = (x + col + 32).load[width=32, alignment=64]().cast[DType.float32]()
        x3 = (x + col + 64).load[width=32, alignment=64]().cast[DType.float32]()
        x4 = (x + col + 96).load[width=32, alignment=64]().cast[DType.float32]()

        q1 = (((ql_a & 0x0F) | ((qh & 3) << 4)).cast[DType.int32]() - 32).cast[DType.float32]()
        q2 = (((ql_b & 0x0F) | (((qh >> 2) & 3) << 4)).cast[DType.int32]() - 32).cast[DType.float32]()
        q3 = (((ql_a >> 4) | (((qh >> 4) & 3) << 4)).cast[DType.int32]() - 32).cast[DType.float32]()
        q4 = (((ql_b >> 4) | ((qh >> 6) << 4)).cast[DType.int32]() - 32).cast[DType.float32]()

        p1 = q1 * x1
        p2 = q2 * x2
        p3 = q3 * x3
        p4 = q4 * x4
        var blk: Float32 = 0.0
        blk += sc[0] * p1.slice[16, offset=0]().reduce_add()
        blk += sc[1] * p1.slice[16, offset=16]().reduce_add()
        blk += sc[2] * p2.slice[16, offset=0]().reduce_add()
        blk += sc[3] * p2.slice[16, offset=16]().reduce_add()
        blk += sc[4] * p3.slice[16, offset=0]().reduce_add()
        blk += sc[5] * p3.slice[16, offset=16]().reduce_add()
        blk += sc[6] * p4.slice[16, offset=0]().reduce_add()
        blk += sc[7] * p4.slice[16, offset=16]().reduce_add()
        acc += d * blk
        c += WARP
    return acc


def gemv_q6_k_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q6_K GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256,
    n_cols % 256 == 0 (whole superblocks per row)."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q6_k_row_acc(w, x, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_q6_k_f16_gidx(
    y: UnsafePointer[Float16, MutAnyOrigin],
    wtab: UnsafePointer[UnsafePointer[UInt8, MutAnyOrigin], MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    ids: UnsafePointer[Int32, MutAnyOrigin],
    sel: Int,
):
    """Routed-MoE Q6_K expert GEMV: identical math to gemv_q6_k_f16_v2 but the
    expert is chosen ON DEVICE from `ids[sel]` (no host readback).

    `wtab[e]` is expert `e`'s own weight base pointer, so experts need not be
    contiguous and each may sit in VRAM or pinned host memory independently.
    Bit-identical to gemv_q6_k_f16_v2 launched against that expert's block."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    lrow = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if lrow >= n_rows:
        return
    w = wtab[Int(ids[sel])]
    total = warp.sum(_gemv_q6_k_row_acc(w, x, n_cols, lrow, lane))
    if lane == 0:
        y[lrow] = Float16(total)


def gemv_q6_k_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q6_K logit GEMV (f32 out), warp-per-row."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q6_k_row_acc(w, x, n_cols, row, lane))
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


def gemv_fp8_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[Float32, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Logity FP32 z wag E4M3 skalowanych osobno dla każdego wiersza."""
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
        raw = (w + base + i).bitcast[UInt16]().load[width=4, alignment=8]()
        var pairs = SIMD[DType.uint32, 4](0)
        comptime for j in range(4):
            pairs[j] = bitcast[DType.uint32, 1](
                f8e4m3x2_to_f16x2(UInt8(raw[j] & 0xFF), UInt8(raw[j] >> 8))
            )[0]
        wv = bitcast[DType.float16, 8](pairs).cast[DType.float32]()
        xv = (x + i).load[width=8, alignment=16]().cast[DType.float32]()
        acc += (wv * xv).reduce_add()
        i += stride

    total = warp.sum(acc) * scales[row]
    if lane == 0:
        y[row] = total


def gemv_fp8_row_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[Float32, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Ta sama matematyka co gemv_fp8_out_f32_v2, ale wynik zapisywany w f16.

    Istnieje dla wag DeepSeeka V4, gdzie projekcje FP8 karmią kolejne warstwy
    aktywacji trzymane w f16 — wariant f32 wymuszałby dodatkowe przejście po
    całym wyjściu tylko po to, żeby je zawęzić.
    """
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
        raw = (w + base + i).bitcast[UInt16]().load[width=4, alignment=8]()
        var pairs = SIMD[DType.uint32, 4](0)
        comptime for j in range(4):
            pairs[j] = bitcast[DType.uint32, 1](
                f8e4m3x2_to_f16x2(UInt8(raw[j] & 0xFF), UInt8(raw[j] >> 8))
            )[0]
        wv = bitcast[DType.float16, 8](pairs).cast[DType.float32]()
        xv = (x + i).load[width=8, alignment=16]().cast[DType.float32]()
        acc += (wv * xv).reduce_add()
        i += stride

    total = warp.sum(acc) * scales[row]
    if lane == 0:
        y[row] = Float16(total)


def _gemv_q5_k_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    xsum: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """Per-lane partial dot product for one Q5_K row (176-byte superblock:
    d/dmin f16 + 12 scale bytes — the same 16-byte header as Q4_K — then
    qh[32] high bits and qs[128] nibbles; value = d*sc*((q4|qh<<4)) - dmin*mn,
    dequant.rs dq_q5_k semantics).

    Same decomposition as _gemv_q4_k_row_acc: lanes stride 128-element halves,
    the min term folds into precomputed per-32-column x sums, and the
    176-byte block keeps every load 16-byte aligned (176 % 16 == 0). The
    5-bit values decode arithmetically (nibble | high bit << 4) — no compact
    LUT exists and the integer ops overlap the loads."""
    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 176
    halves = blocks_per_row * 2

    var acc: Float32 = 0.0
    var min_acc: Float32 = 0.0
    var c = lane
    while c < halves:
        b = c // 2
        q = (c % 2) * 2
        off = row_base + b * 176
        hdr = (w + off).load[width=16, alignment=16]()
        dm = bitcast[DType.float16, 8](hdr)
        d = Float32(dm[0])
        dmin = Float32(dm[1])
        qh = (w + off + 16).load[width=32, alignment=16]()

        comptime for qq in range(2):
            g = q + qq
            sc1, mn1 = _q4k_scale_min(hdr, 2 * g)
            sc2, mn2 = _q4k_scale_min(hdr, 2 * g + 1)
            qv = (w + off + 48 + g * 32).load[width=32, alignment=16]()
            col = (c * 2 + qq) * 64
            q0 = ((qv & 0x0F) | (((qh >> UInt8(2 * g)) & 1) << 4)).cast[DType.float32]()
            q1 = ((qv >> 4) | (((qh >> UInt8(2 * g + 1)) & 1) << 4)).cast[DType.float32]()
            x0 = (x + col).load[width=32, alignment=64]().cast[DType.float32]()
            x1 = (x + col + 32).load[width=32, alignment=64]().cast[DType.float32]()
            acc += (q0 * x0).reduce_add() * (d * sc1) + (q1 * x1).reduce_add() * (d * sc2)
            seg = (c * 2 + qq) * 2
            min_acc += dmin * (mn1 * xsum[seg] + mn2 * xsum[seg + 1])
        c += WARP
    return acc - min_acc


def gemv_q5_k_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q5_K GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256,
    n_cols % 256 == 0 (whole superblocks per row), n_cols <= 32768."""
    tid = Int(thread_idx.x)
    xsum = stack_allocation[
        Q4K_MAX_SEGS, Float32, address_space = AddressSpace.SHARED
    ]()
    _q4k_fill_xsum(x, xsum, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q5_k_row_acc(w, x, xsum, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_q5_k_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q5_K logit GEMV (f32 out), warp-per-row."""
    tid = Int(thread_idx.x)
    xsum = stack_allocation[
        Q4K_MAX_SEGS, Float32, address_space = AddressSpace.SHARED
    ]()
    _q4k_fill_xsum(x, xsum, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q5_k_row_acc(w, x, xsum, n_cols, row, lane))
    if lane == 0:
        y[row] = total


def _q3k_scales8(
    w: UnsafePointer[UInt8, MutAnyOrigin], off: Int, n: Int
) -> SIMD[DType.float32, 8]:
    """Unpack the 8 (scale - 32) values of one 128-element Q3_K half from the
    12 packed 6-bit scale bytes at `off + 96` (llama.cpp kmask1/kmask2
    shuffle, dequant.rs dq_q3_k semantics). The block is only 2-byte aligned,
    so the bytes load as u16 lanes."""
    s16a = (w + off + 96).bitcast[UInt16]().load[width=4]()
    s16b = (w + off + 104).bitcast[UInt16]().load[width=2]()
    sb = bitcast[DType.uint8, 8](s16a)
    st = bitcast[DType.uint8, 4](s16b)
    a0 = (
        UInt32(sb[0])
        | (UInt32(sb[1]) << 8)
        | (UInt32(sb[2]) << 16)
        | (UInt32(sb[3]) << 24)
    )
    a1 = (
        UInt32(sb[4])
        | (UInt32(sb[5]) << 8)
        | (UInt32(sb[6]) << 16)
        | (UInt32(sb[7]) << 24)
    )
    tmp = (
        UInt32(st[0])
        | (UInt32(st[1]) << 8)
        | (UInt32(st[2]) << 16)
        | (UInt32(st[3]) << 24)
    )
    comptime KMASK1 = UInt32(0x03030303)
    comptime KMASK2 = UInt32(0x0F0F0F0F)
    wlo = (
        (a0 & KMASK2) | ((tmp & KMASK1) << 4)
    ) if n == 0 else (((a0 >> 4) & KMASK2) | (((tmp >> 4) & KMASK1) << 4))
    whi = (
        (a1 & KMASK2) | (((tmp >> 2) & KMASK1) << 4)
    ) if n == 0 else (((a1 >> 4) & KMASK2) | (((tmp >> 6) & KMASK1) << 4))
    var sc = SIMD[DType.float32, 8]()
    comptime for j in range(4):
        sc[j] = Float32(Int((wlo >> UInt32(8 * j)) & 0xFF) - 32)
        sc[4 + j] = Float32(Int((whi >> UInt32(8 * j)) & 0xFF) - 32)
    return sc


def _gemv_q3_k_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """Per-lane partial dot product for one Q3_K row (110-byte superblock:
    hmask[32], qs[64] 2-bit codes, 12 packed 6-bit scales, d f16; value =
    d*(sc-32)*(q - (hmask bit ? 0 : 4)), dequant.rs dq_q3_k semantics).

    Lanes stride 128-element halves like Q6_K; the 110-byte block is only
    2-byte aligned so everything loads as u16 lanes."""
    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 110
    halves = blocks_per_row * 2

    var acc: Float32 = 0.0
    var c = lane
    while c < halves:
        b = c // 2
        n = c % 2
        off = row_base + b * 110
        d = Float32((w + off + 108).bitcast[Float16]()[0])
        sc = _q3k_scales8(w, off, n)
        hm16 = (w + off).bitcast[UInt16]().load[width=16]()
        qs16 = (w + off + 32 + n * 32).bitcast[UInt16]().load[width=16]()
        hm = bitcast[DType.uint8, 32](hm16)
        qs = bitcast[DType.uint8, 32](qs16)

        col = c * 128
        var blk: Float32 = 0.0
        comptime for s in range(4):
            q = ((qs >> UInt8(2 * s)) & 3).cast[DType.int32]()
            hb = ((hm >> UInt8(4 * n + s)) & 1).cast[DType.int32]()
            v = (q + 4 * hb - 4).cast[DType.float32]()
            xv = (x + col + 32 * s).load[width=32, alignment=64]().cast[DType.float32]()
            p = v * xv
            blk += sc[2 * s] * p.slice[16, offset=0]().reduce_add()
            blk += sc[2 * s + 1] * p.slice[16, offset=16]().reduce_add()
        acc += d * blk
        c += WARP
    return acc


def gemv_q3_k_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q3_K GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256,
    n_cols % 256 == 0 (whole superblocks per row)."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q3_k_row_acc(w, x, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_q3_k_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q3_K logit GEMV (f32 out), warp-per-row."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q3_k_row_acc(w, x, n_cols, row, lane))
    if lane == 0:
        y[row] = total


comptime Q2K_MAX_SEGS = 2048  # 16-col x segments -> n_cols <= 32768


def _q2k_fill_xsum16(
    x: UnsafePointer[Float16, MutAnyOrigin],
    xsum: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    tid: Int,
):
    """Stage per-16-column x segment sums to shared memory (whole block).
    Q2_K's min scales apply per 16 elements, so its min-term folding needs
    finer segments than the 32-column Q4_K/Q5_K staging."""
    segs = n_cols // 16
    var s = tid
    while s < segs:
        xv = (x + s * 16).load[width=16, alignment=32]().cast[DType.float32]()
        xsum[s] = xv.reduce_add()
        s += 256
    barrier()


def _gemv_q2_k_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    xsum16: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """Per-lane partial dot product for one Q2_K row (84-byte superblock:
    16 packed 4-bit scale/min bytes, qs[64] 2-bit codes, d f16, dmin f16;
    value = d*(sc&0xF)*q - dmin*(sc>>4), dequant.rs dq_q2_k semantics).

    Lanes stride 128-element halves; the min term folds into precomputed
    per-16-column x sums. The 84-byte block is only guaranteed 2-byte
    alignment relative to wide loads, so raw bytes load as u16 lanes."""
    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 84
    halves = blocks_per_row * 2

    var acc: Float32 = 0.0
    var min_acc: Float32 = 0.0
    var c = lane
    while c < halves:
        b = c // 2
        n = c % 2
        off = row_base + b * 84
        dm16 = (w + off + 80).bitcast[UInt16]().load[width=2]()
        dm = bitcast[DType.float16, 2](dm16)
        d = Float32(dm[0])
        dmin = Float32(dm[1])
        sc16 = (w + off).bitcast[UInt16]().load[width=8]()
        scb = bitcast[DType.uint8, 16](sc16)
        qs16 = (w + off + 16 + n * 32).bitcast[UInt16]().load[width=16]()
        qs = bitcast[DType.uint8, 32](qs16)

        col = c * 128
        comptime for s in range(4):
            q = ((qs >> UInt8(2 * s)) & 3).cast[DType.float32]()
            xv = (x + col + 32 * s).load[width=32, alignment=64]().cast[DType.float32]()
            p = q * xv
            is0 = n * 8 + 2 * s
            acc += d * Float32(scb[is0] & 0x0F) * p.slice[16, offset=0]().reduce_add()
            acc += d * Float32(scb[is0 + 1] & 0x0F) * p.slice[16, offset=16]().reduce_add()
            seg = (col + 32 * s) // 16
            min_acc += dmin * (
                Float32(scb[is0] >> 4) * xsum16[seg]
                + Float32(scb[is0 + 1] >> 4) * xsum16[seg + 1]
            )
        c += WARP
    return acc - min_acc


def gemv_q2_k_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q2_K GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256,
    n_cols % 256 == 0 (whole superblocks per row), n_cols <= 32768."""
    tid = Int(thread_idx.x)
    xsum = stack_allocation[
        Q2K_MAX_SEGS, Float32, address_space = AddressSpace.SHARED
    ]()
    _q2k_fill_xsum16(x, xsum, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q2_k_row_acc(w, x, xsum, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_q2_k_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q2_K logit GEMV (f32 out), warp-per-row."""
    tid = Int(thread_idx.x)
    xsum = stack_allocation[
        Q2K_MAX_SEGS, Float32, address_space = AddressSpace.SHARED
    ]()
    _q2k_fill_xsum16(x, xsum, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q2_k_row_acc(w, x, xsum, n_cols, row, lane))
    if lane == 0:
        y[row] = total


# ---------------------------------------------------------------------------
# Legacy 32-element block formats (Q4_0 / Q4_1 / Q5_0 / Q5_1). Same
# warp-per-row machinery as Q8_0: blocks are only 2-byte aligned, so quant
# bytes load as u16 lanes and reinterpret. Elements 0..15 are the low nibbles
# of the 16 quant bytes, 16..31 the high nibbles (dequant.rs dq_q4_0 family).
# ---------------------------------------------------------------------------

comptime _IOTA16_U32 = SIMD[DType.uint32, 16](
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15
)


def _gemv_q4_0_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """Q4_0 (18-byte block: d f16, 16 nibble bytes; value = d*(q - 8))."""
    blocks_per_row = n_cols // 32
    row_base = row * blocks_per_row * 18
    var acc: Float32 = 0.0
    var b = lane
    while b < blocks_per_row:
        off = row_base + b * 18
        d = Float32((w + off).bitcast[Float16]()[0])
        v16 = (w + off + 2).bitcast[UInt16]().load[width=8]()
        q = bitcast[DType.uint8, 16](v16)
        lo = ((q & 0x0F).cast[DType.int32]() - 8).cast[DType.float32]()
        hi = ((q >> 4).cast[DType.int32]() - 8).cast[DType.float32]()
        x0 = (x + b * 32).load[width=16, alignment=32]().cast[DType.float32]()
        x1 = (x + b * 32 + 16).load[width=16, alignment=32]().cast[DType.float32]()
        acc += d * ((lo * x0).reduce_add() + (hi * x1).reduce_add())
        b += WARP
    return acc


def gemv_q4_0_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q4_0 GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q4_0_row_acc(w, x, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_q4_0_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q4_0 logit GEMV (f32 out), warp-per-row."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q4_0_row_acc(w, x, n_cols, row, lane))
    if lane == 0:
        y[row] = total


def _gemv_q4_1_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """Q4_1 (20-byte block: d f16, m f16, 16 nibble bytes; value = q*d + m —
    the min term folds into m * sum(x) per block)."""
    blocks_per_row = n_cols // 32
    row_base = row * blocks_per_row * 20
    var acc: Float32 = 0.0
    var b = lane
    while b < blocks_per_row:
        off = row_base + b * 20
        dm16 = (w + off).bitcast[UInt16]().load[width=2]()
        dm = bitcast[DType.float16, 2](dm16)
        d = Float32(dm[0])
        m = Float32(dm[1])
        v16 = (w + off + 4).bitcast[UInt16]().load[width=8]()
        q = bitcast[DType.uint8, 16](v16)
        lo = (q & 0x0F).cast[DType.float32]()
        hi = (q >> 4).cast[DType.float32]()
        x0 = (x + b * 32).load[width=16, alignment=32]().cast[DType.float32]()
        x1 = (x + b * 32 + 16).load[width=16, alignment=32]().cast[DType.float32]()
        acc += d * ((lo * x0).reduce_add() + (hi * x1).reduce_add())
        acc += m * (x0.reduce_add() + x1.reduce_add())
        b += WARP
    return acc


def gemv_q4_1_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q4_1 GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q4_1_row_acc(w, x, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_q4_1_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q4_1 logit GEMV (f32 out), warp-per-row."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q4_1_row_acc(w, x, n_cols, row, lane))
    if lane == 0:
        y[row] = total


def _q5_high_bits(qh: UInt32) -> Tuple[SIMD[DType.int32, 16], SIMD[DType.int32, 16]]:
    """Expand the 32 qh bits into per-element +16 offsets: element e (0..15
    low nibbles, 16..31 high nibbles) uses bit e of qh — dequant.rs dq_q5_0
    xh_0/xh_1 semantics."""
    qv = SIMD[DType.uint32, 16](qh)
    lo = ((qv >> _IOTA16_U32) & 1).cast[DType.int32]() * 16
    hi = ((qv >> (_IOTA16_U32 + 16)) & 1).cast[DType.int32]() * 16
    return (lo, hi)


def _gemv_q5_0_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """Q5_0 (22-byte block: d f16, qh u32, 16 nibble bytes; value =
    d*((q | qh_bit<<4) - 16))."""
    blocks_per_row = n_cols // 32
    row_base = row * blocks_per_row * 22
    var acc: Float32 = 0.0
    var b = lane
    while b < blocks_per_row:
        off = row_base + b * 22
        d = Float32((w + off).bitcast[Float16]()[0])
        h16 = (w + off + 2).bitcast[UInt16]().load[width=2]()
        qh = UInt32(h16[0]) | (UInt32(h16[1]) << 16)
        v16 = (w + off + 6).bitcast[UInt16]().load[width=8]()
        q = bitcast[DType.uint8, 16](v16)
        hb_lo, hb_hi = _q5_high_bits(qh)
        lo = ((q & 0x0F).cast[DType.int32]() + hb_lo - 16).cast[DType.float32]()
        hi = ((q >> 4).cast[DType.int32]() + hb_hi - 16).cast[DType.float32]()
        x0 = (x + b * 32).load[width=16, alignment=32]().cast[DType.float32]()
        x1 = (x + b * 32 + 16).load[width=16, alignment=32]().cast[DType.float32]()
        acc += d * ((lo * x0).reduce_add() + (hi * x1).reduce_add())
        b += WARP
    return acc


def gemv_q5_0_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q5_0 GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q5_0_row_acc(w, x, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_q5_0_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q5_0 logit GEMV (f32 out), warp-per-row."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q5_0_row_acc(w, x, n_cols, row, lane))
    if lane == 0:
        y[row] = total


def _gemv_q5_1_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """Q5_1 (24-byte block: d f16, m f16, qh u32, 16 nibble bytes; value =
    (q | qh_bit<<4)*d + m — the min term folds into m * sum(x) per block)."""
    blocks_per_row = n_cols // 32
    row_base = row * blocks_per_row * 24
    var acc: Float32 = 0.0
    var b = lane
    while b < blocks_per_row:
        off = row_base + b * 24
        dm16 = (w + off).bitcast[UInt16]().load[width=2]()
        dm = bitcast[DType.float16, 2](dm16)
        d = Float32(dm[0])
        m = Float32(dm[1])
        h16 = (w + off + 4).bitcast[UInt16]().load[width=2]()
        qh = UInt32(h16[0]) | (UInt32(h16[1]) << 16)
        v16 = (w + off + 8).bitcast[UInt16]().load[width=8]()
        q = bitcast[DType.uint8, 16](v16)
        hb_lo, hb_hi = _q5_high_bits(qh)
        lo = ((q & 0x0F).cast[DType.int32]() + hb_lo).cast[DType.float32]()
        hi = ((q >> 4).cast[DType.int32]() + hb_hi).cast[DType.float32]()
        x0 = (x + b * 32).load[width=16, alignment=32]().cast[DType.float32]()
        x1 = (x + b * 32 + 16).load[width=16, alignment=32]().cast[DType.float32]()
        acc += d * ((lo * x0).reduce_add() + (hi * x1).reduce_add())
        acc += m * (x0.reduce_add() + x1.reduce_add())
        b += WARP
    return acc


def gemv_q5_1_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q5_1 GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q5_1_row_acc(w, x, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_q5_1_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Q5_1 logit GEMV (f32 out), warp-per-row."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q5_1_row_acc(w, x, n_cols, row, lane))
    if lane == 0:
        y[row] = total


# ---------------------------------------------------------------------------
# Non-linear 4-bit codebook formats (IQ4_NL / IQ4_XS) and MXFP4. Values
# decode through a 16-entry shared-memory LUT (same bank-broadcast trick as
# gemv_nvfp4_f16_v2); elements 0..15 of a 32-run are the low nibbles of its
# 16 quant bytes, 16..31 the high nibbles (dequant.rs dq_iq4_nl / dq_mxfp4).
# ---------------------------------------------------------------------------

comptime IQ4NL_VALS = SIMD[DType.float32, 16](
    -127.0, -104.0, -83.0, -65.0, -49.0, -35.0, -22.0, -10.0,
    1.0, 13.0, 25.0, 38.0, 53.0, 69.0, 89.0, 113.0,
)

comptime MXFP4_VALS = SIMD[DType.float32, 16](
    0.0, 1.0, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0,
    0.0, -1.0, -2.0, -3.0, -4.0, -6.0, -8.0, -12.0,
)


def _init_lut16(
    lut: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    vals: SIMD[DType.float32, 16],
):
    tid = Int(thread_idx.x)
    if tid < 16:
        lut[tid] = vals[tid]
    barrier()


def _e8m0_half(e: UInt8) -> Float32:
    # ggml ggml_e8m0_to_fp32_half: 2^(e-127) / 2 with denormal handling.
    var bits: UInt32
    if e < 2:
        bits = UInt32(0x00200000) << UInt32(e)
    else:
        bits = UInt32(UInt32(e) - 1) << 23
    return UnsafePointer(to=bits).bitcast[Float32]()[0]


def _dot_lut_block32(
    qv: SIMD[DType.uint8, 16],
    x: UnsafePointer[Float16, MutAnyOrigin],
    col: Int,
    lut: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
) -> Float32:
    # 32-element codebook dot: 8-wide LUT gathers stay in registers (32-wide
    # gathered vectors spill to local memory — same shape as gemv_nvfp4).
    var dot0 = SIMD[DType.float32, 8](0.0)
    var dot1 = SIMD[DType.float32, 8](0.0)
    comptime for k in range(2):
        q8 = qv.slice[8, offset = k * 8]()
        var lov = SIMD[DType.float32, 8]()
        var hiv = SIMD[DType.float32, 8]()
        comptime for j in range(8):
            lov[j] = lut[Int(q8[j] & 0x0F)]
            hiv[j] = lut[Int(q8[j] >> 4)]
        x0 = (x + col + k * 8).load[width=8, alignment=16]().cast[DType.float32]()
        x1 = (x + col + 16 + k * 8).load[width=8, alignment=16]().cast[DType.float32]()
        dot0 += lov * x0
        dot1 += hiv * x1
    return dot0.reduce_add() + dot1.reduce_add()


def _gemv_iq4_nl_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    lut: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """IQ4_NL (18-byte block: d f16, 16 codebook-index bytes; value =
    d * kvalues_iq4nl[q])."""
    blocks_per_row = n_cols // 32
    row_base = row * blocks_per_row * 18
    var acc: Float32 = 0.0
    var b = lane
    while b < blocks_per_row:
        off = row_base + b * 18
        d = Float32((w + off).bitcast[Float16]()[0])
        v16 = (w + off + 2).bitcast[UInt16]().load[width=8]()
        q = bitcast[DType.uint8, 16](v16)
        acc += d * _dot_lut_block32(q, x, b * 32, lut)
        b += WARP
    return acc


def gemv_iq4_nl_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ4_NL GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256."""
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_lut16(lut, IQ4NL_VALS)
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq4_nl_row_acc(w, x, lut, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_iq4_nl_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ4_NL logit GEMV (f32 out), warp-per-row."""
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_lut16(lut, IQ4NL_VALS)
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq4_nl_row_acc(w, x, lut, n_cols, row, lane))
    if lane == 0:
        y[row] = total


def _gemv_mxfp4_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    lut: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """MXFP4 (17-byte block: E8M0 scale byte + 16 e2m1 pair bytes; value =
    e8m0_half(e) * kvalues_mxfp4[q]). The odd block size leaves only byte
    alignment, so the 16 quant bytes load unaligned."""
    blocks_per_row = n_cols // 32
    row_base = row * blocks_per_row * 17
    var acc: Float32 = 0.0
    var b = lane
    while b < blocks_per_row:
        off = row_base + b * 17
        d = _e8m0_half(w[off])
        q = (w + off + 1).load[width=16, alignment=1]()
        acc += d * _dot_lut_block32(q, x, b * 32, lut)
        b += WARP
    return acc


def gemv_mxfp4_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """MXFP4 GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256."""
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_lut16(lut, MXFP4_VALS)
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_mxfp4_row_acc(w, x, lut, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_mxfp4_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """MXFP4 logit GEMV (f32 out), warp-per-row."""
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_lut16(lut, MXFP4_VALS)
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_mxfp4_row_acc(w, x, lut, n_cols, row, lane))
    if lane == 0:
        y[row] = total


def _iq4xs_scale(hdr: SIMD[DType.uint8, 8], ib: Int) -> Float32:
    # 6-bit sub-block scale: low 4 bits from scales_l nibbles (bytes 4..8),
    # high 2 bits from the scales_h u16 (bytes 2..4); dl multiplier = ls - 32.
    scales_h = Int(hdr[2]) | (Int(hdr[3]) << 8)
    ls = Int((hdr[4 + ib // 2] >> UInt8(4 * (ib % 2))) & 0x0F) | (
        ((scales_h >> (2 * ib)) & 3) << 4
    )
    return Float32(ls - 32)


def _gemv_iq4_xs_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    lut: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """IQ4_XS (136-byte superblock: d f16, scales_h u16, scales_l[4],
    qs[128]; value = d*(ls-32) * kvalues_iq4nl[q], dequant.rs dq_iq4_xs).

    Lanes stride 128-element halves (4 codebook sub-blocks each); 136 % 8 == 0
    keeps every load at least 8-byte aligned."""
    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 136
    halves = blocks_per_row * 2

    var acc: Float32 = 0.0
    var c = lane
    while c < halves:
        b = c // 2
        n = c % 2
        off = row_base + b * 136
        hdr = (w + off).load[width=8, alignment=8]()
        d = Float32(bitcast[DType.float16, 4](hdr)[0])
        col = c * 128
        comptime for j in range(4):
            ib = 4 * n + j
            qv = (w + off + 8 + ib * 16).load[width=16, alignment=8]()
            dl = d * _iq4xs_scale(hdr, ib)
            acc += dl * _dot_lut_block32(qv, x, col + j * 32, lut)
        c += WARP
    return acc


def gemv_iq4_xs_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ4_XS GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256,
    n_cols % 256 == 0 (whole superblocks per row)."""
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_lut16(lut, IQ4NL_VALS)
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq4_xs_row_acc(w, x, lut, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_iq4_xs_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ4_XS logit GEMV (f32 out), warp-per-row."""
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_lut16(lut, IQ4NL_VALS)
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq4_xs_row_acc(w, x, lut, n_cols, row, lane))
    if lane == 0:
        y[row] = total


# ---------------------------------------------------------------------------
# Codebook 2/3-bit formats (IQ2_XS / IQ2_S / IQ3_S). Grid tables are too big
# for comptime staging, so kernels take device pointers to the ggml grids
# (iq2xs_grid / iq2s_grid as u64 rows of 8 magnitudes, iq3s_grid as u32 rows
# of 4) uploaded once by the Rust launcher — the same constant-table trick
# llama.cpp's CUDA kernels use. Signs come from ksigns_iq2xs (IQ2_XS) or
# explicit per-code sign bytes (IQ2_S / IQ3_S); bit j of a sign byte flips
# element j (dequant.rs dq_iq2_xs / dq_iq2_s / dq_iq3_s semantics).
# ---------------------------------------------------------------------------

comptime _IOTA8_U8 = SIMD[DType.uint8, 8](0, 1, 2, 3, 4, 5, 6, 7)


def _signs8(b: UInt8) -> SIMD[DType.float32, 8]:
    bits = ((SIMD[DType.uint8, 8](b) >> _IOTA8_U8) & 1).cast[DType.float32]()
    return 1.0 - 2.0 * bits


def _gemv_iq2_xs_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """IQ2_XS (74-byte superblock: d f16, 32 u16 codes — 9-bit grid index +
    7-bit ksigns code — and 8 packed 4-bit scales). Lanes stride 128-element
    halves (4 codes x 4 ib32 groups each)."""
    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 74
    halves = blocks_per_row * 2

    var acc: Float32 = 0.0
    var c = lane
    while c < halves:
        b = c // 2
        n = c % 2
        off = row_base + b * 74
        d = Float32((w + off).bitcast[Float16]()[0])
        codes = (w + off + 2 + n * 32).bitcast[UInt16]().load[width=16]()
        sc16 = (w + off + 66 + n * 4).bitcast[UInt16]().load[width=2]()
        scb = bitcast[DType.uint8, 4](sc16)

        col = c * 128
        comptime for j in range(4):
            db0 = d * (0.5 + Float32(scb[j] & 0x0F)) * 0.25
            db1 = d * (0.5 + Float32(scb[j] >> 4)) * 0.25
            comptime for l in range(4):
                code = codes[4 * j + l]
                mag = (grid + Int(code & 511) * 8).load[width=8, alignment=8]().cast[
                    DType.float32
                ]()
                sg = _signs8(ksigns[Int(code >> 9)])
                xv = (x + col + j * 32 + l * 8).load[width=8, alignment=16]().cast[
                    DType.float32
                ]()
                db = db0 if l < 2 else db1
                acc += db * (mag * sg * xv).reduce_add()
        c += WARP
    return acc


def gemv_iq2_xs_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ2_XS GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256,
    n_cols % 256 == 0."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq2_xs_row_acc(w, grid, ksigns, x, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_iq2_xs_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ2_XS logit GEMV (f32 out), warp-per-row."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq2_xs_row_acc(w, grid, ksigns, x, n_cols, row, lane))
    if lane == 0:
        y[row] = total


def _gemv_iq2_s_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """IQ2_S (82-byte superblock: d f16, 32 grid-index low bytes + 32
    explicit sign bytes, qh[8] high index bits, 8 packed 4-bit scales)."""
    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 82
    halves = blocks_per_row * 2

    var acc: Float32 = 0.0
    var c = lane
    while c < halves:
        b = c // 2
        n = c % 2
        off = row_base + b * 82
        d = Float32((w + off).bitcast[Float16]()[0])
        qs16 = (w + off + 2 + n * 16).bitcast[UInt16]().load[width=8]()
        qs = bitcast[DType.uint8, 16](qs16)
        sg16 = (w + off + 34 + n * 16).bitcast[UInt16]().load[width=8]()
        sgs = bitcast[DType.uint8, 16](sg16)
        qh16 = (w + off + 66 + n * 4).bitcast[UInt16]().load[width=2]()
        qh = bitcast[DType.uint8, 4](qh16)
        sc16 = (w + off + 74 + n * 4).bitcast[UInt16]().load[width=2]()
        scb = bitcast[DType.uint8, 4](sc16)

        col = c * 128
        comptime for j in range(4):
            db0 = d * (0.5 + Float32(scb[j] & 0x0F)) * 0.25
            db1 = d * (0.5 + Float32(scb[j] >> 4)) * 0.25
            comptime for l in range(4):
                idx = Int(qs[4 * j + l]) | (
                    (Int(qh[j]) << (8 - 2 * l)) & 0x300
                )
                mag = (grid + idx * 8).load[width=8, alignment=8]().cast[
                    DType.float32
                ]()
                sg = _signs8(sgs[4 * j + l])
                xv = (x + col + j * 32 + l * 8).load[width=8, alignment=16]().cast[
                    DType.float32
                ]()
                db = db0 if l < 2 else db1
                acc += db * (mag * sg * xv).reduce_add()
        c += WARP
    return acc


def gemv_iq2_s_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ2_S GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256,
    n_cols % 256 == 0."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq2_s_row_acc(w, grid, x, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_iq2_s_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ2_S logit GEMV (f32 out), warp-per-row."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq2_s_row_acc(w, grid, x, n_cols, row, lane))
    if lane == 0:
        y[row] = total


def _gemv_iq3_s_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """IQ3_S (110-byte superblock: d f16, 64 grid-index bytes, qh[8] high
    bits, 32 explicit sign bytes, 4 packed 4-bit scales; the u32 grid packs
    4 magnitudes per entry)."""
    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 110
    halves = blocks_per_row * 2

    var acc: Float32 = 0.0
    var c = lane
    while c < halves:
        b = c // 2
        n = c % 2
        off = row_base + b * 110
        d = Float32((w + off).bitcast[Float16]()[0])
        qs16 = (w + off + 2 + n * 32).bitcast[UInt16]().load[width=16]()
        qs = bitcast[DType.uint8, 32](qs16)
        qh16 = (w + off + 66 + n * 4).bitcast[UInt16]().load[width=2]()
        qh = bitcast[DType.uint8, 4](qh16)
        sg16 = (w + off + 74 + n * 16).bitcast[UInt16]().load[width=8]()
        sgs = bitcast[DType.uint8, 16](sg16)
        sc16 = (w + off + 106 + n * 2).bitcast[UInt16]().load[width=1]()
        scb = bitcast[DType.uint8, 2](sc16)

        col = c * 128
        comptime for j in range(4):
            sraw = scb[j // 2]
            snib = (sraw & 0x0F) if j % 2 == 0 else (sraw >> 4)
            db = d * Float32(1 + 2 * Int(snib))
            h = Int(qh[j])
            comptime for l in range(4):
                i1 = Int(qs[8 * j + 2 * l]) | ((h << (8 - 2 * l)) & 256)
                i2 = Int(qs[8 * j + 2 * l + 1]) | ((h << (7 - 2 * l)) & 256)
                m1 = (grid + i1 * 4).load[width=4, alignment=4]()
                m2 = (grid + i2 * 4).load[width=4, alignment=4]()
                mag = m1.join(m2).cast[DType.float32]()
                sg = _signs8(sgs[4 * j + l])
                xv = (x + col + j * 32 + l * 8).load[width=8, alignment=16]().cast[
                    DType.float32
                ]()
                acc += db * (mag * sg * xv).reduce_add()
        c += WARP
    return acc


def gemv_iq3_s_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ3_S GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256,
    n_cols % 256 == 0."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq3_s_row_acc(w, grid, x, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_iq3_s_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ3_S logit GEMV (f32 out), warp-per-row."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq3_s_row_acc(w, grid, x, n_cols, row, lane))
    if lane == 0:
        y[row] = total


# ---------------------------------------------------------------------------
# Remaining codebook formats: IQ2_XXS / IQ3_XXS (ksigns sign codes packed in
# a per-32-element aux word) and IQ1_S / IQ1_M (signed i8 grid rows plus the
# ±0.125 IQ1S_DELTA offset). Same device-pointer grid tables as IQ2_XS.
# ---------------------------------------------------------------------------


def _gemv_iq2_xxs_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """IQ2_XXS (66-byte superblock: d f16 + 8 bytes per 32 elements — 4 grid
    index bytes and a u32 packing 4x7-bit ksigns codes + 4-bit scale)."""
    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 66
    halves = blocks_per_row * 2

    var acc: Float32 = 0.0
    var c = lane
    while c < halves:
        b = c // 2
        n = c % 2
        off = row_base + b * 66
        d = Float32((w + off).bitcast[Float16]()[0])
        data16 = (w + off + 2 + n * 32).bitcast[UInt16]().load[width=16]()
        data = bitcast[DType.uint8, 32](data16)

        col = c * 128
        comptime for j in range(4):
            aux1 = (
                UInt32(data[8 * j + 4])
                | (UInt32(data[8 * j + 5]) << 8)
                | (UInt32(data[8 * j + 6]) << 16)
                | (UInt32(data[8 * j + 7]) << 24)
            )
            db = d * (0.5 + Float32(aux1 >> 28)) * 0.25
            comptime for l in range(4):
                mag = (grid + Int(data[8 * j + l]) * 8).load[
                    width=8, alignment=8
                ]().cast[DType.float32]()
                sg = _signs8(ksigns[Int((aux1 >> UInt32(7 * l)) & 127)])
                xv = (x + col + j * 32 + l * 8).load[width=8, alignment=16]().cast[
                    DType.float32
                ]()
                acc += db * (mag * sg * xv).reduce_add()
        c += WARP
    return acc


def gemv_iq2_xxs_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ2_XXS GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block =
    256, n_cols % 256 == 0."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq2_xxs_row_acc(w, grid, ksigns, x, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_iq2_xxs_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ2_XXS logit GEMV (f32 out), warp-per-row."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq2_xxs_row_acc(w, grid, ksigns, x, n_cols, row, lane))
    if lane == 0:
        y[row] = total


def _gemv_iq3_xxs_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """IQ3_XXS (98-byte superblock: d f16, 64 grid-index bytes, then a u32
    per 32 elements packing 4x7-bit ksigns codes + 4-bit scale; the u32 grid
    packs 4 magnitudes per entry)."""
    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 98
    halves = blocks_per_row * 2

    var acc: Float32 = 0.0
    var c = lane
    while c < halves:
        b = c // 2
        n = c % 2
        off = row_base + b * 98
        d = Float32((w + off).bitcast[Float16]()[0])
        qs16 = (w + off + 2 + n * 32).bitcast[UInt16]().load[width=16]()
        qs = bitcast[DType.uint8, 32](qs16)
        sas16 = (w + off + 66 + n * 16).bitcast[UInt16]().load[width=8]()
        sas = bitcast[DType.uint8, 16](sas16)

        col = c * 128
        comptime for j in range(4):
            aux = (
                UInt32(sas[4 * j])
                | (UInt32(sas[4 * j + 1]) << 8)
                | (UInt32(sas[4 * j + 2]) << 16)
                | (UInt32(sas[4 * j + 3]) << 24)
            )
            db = d * (0.5 + Float32(aux >> 28)) * 0.5
            comptime for l in range(4):
                m1 = (grid + Int(qs[8 * j + 2 * l]) * 4).load[width=4, alignment=4]()
                m2 = (grid + Int(qs[8 * j + 2 * l + 1]) * 4).load[
                    width=4, alignment=4
                ]()
                mag = m1.join(m2).cast[DType.float32]()
                sg = _signs8(ksigns[Int((aux >> UInt32(7 * l)) & 127)])
                xv = (x + col + j * 32 + l * 8).load[width=8, alignment=16]().cast[
                    DType.float32
                ]()
                acc += db * (mag * sg * xv).reduce_add()
        c += WARP
    return acc


def gemv_iq3_xxs_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ3_XXS GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block =
    256, n_cols % 256 == 0."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq3_xxs_row_acc(w, grid, ksigns, x, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_iq3_xxs_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ3_XXS logit GEMV (f32 out), warp-per-row."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq3_xxs_row_acc(w, grid, ksigns, x, n_cols, row, lane))
    if lane == 0:
        y[row] = total


comptime IQ1S_DELTA: Float32 = 0.125


def _gemv_iq1_s_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """IQ1_S (50-byte superblock: d f16, 32 grid-index low bytes, 8 u16 qh
    words carrying 3 high index bits per code, a 3-bit scale and the delta
    sign; value = dl * (grid_i8 + ±0.125))."""
    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 50
    halves = blocks_per_row * 2

    var acc: Float32 = 0.0
    var c = lane
    while c < halves:
        b = c // 2
        n = c % 2
        off = row_base + b * 50
        d = Float32((w + off).bitcast[Float16]()[0])
        qs16 = (w + off + 2 + n * 16).bitcast[UInt16]().load[width=8]()
        qs = bitcast[DType.uint8, 16](qs16)
        qh = (w + off + 34 + n * 8).bitcast[UInt16]().load[width=4]()

        col = c * 128
        comptime for j in range(4):
            h = qh[j]
            dl = d * Float32(2 * Int((h >> 12) & 7) + 1)
            delta = -IQ1S_DELTA if (h & 0x8000) != 0 else IQ1S_DELTA
            comptime for l in range(4):
                idx = Int(qs[4 * j + l]) | (Int((h >> UInt16(3 * l)) & 7) << 8)
                mag = (
                    (grid + idx * 8)
                    .load[width=8, alignment=8]()
                    .cast[DType.int8]()
                    .cast[DType.float32]()
                )
                xv = (x + col + j * 32 + l * 8).load[width=8, alignment=16]().cast[
                    DType.float32
                ]()
                acc += dl * ((mag + delta) * xv).reduce_add()
        c += WARP
    return acc


def gemv_iq1_s_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ1_S GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256,
    n_cols % 256 == 0."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq1_s_row_acc(w, grid, x, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_iq1_s_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ1_S logit GEMV (f32 out), warp-per-row."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq1_s_row_acc(w, grid, x, n_cols, row, lane))
    if lane == 0:
        y[row] = total


def _gemv_iq1_m_row_acc(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    row: Int,
    lane: Int,
) -> Float32:
    """IQ1_M (56-byte superblock: 32 grid-index low bytes, 16 qh nibbles —
    3 high index bits + delta sign per code — and 4 u16 scale words whose
    top nibbles reassemble the superblock d; dequant.rs dq_iq1_m)."""
    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 56
    halves = blocks_per_row * 2

    var acc: Float32 = 0.0
    var c = lane
    while c < halves:
        b = c // 2
        n = c % 2
        off = row_base + b * 56
        sc = (w + off + 48).bitcast[UInt16]().load[width=4]()
        d_bits = (
            (sc[0] >> 12)
            | ((sc[1] >> 8) & 0x00F0)
            | ((sc[2] >> 4) & 0x0F00)
            | (sc[3] & 0xF000)
        )
        d = Float32(bitcast[DType.float16, 1](SIMD[DType.uint16, 1](d_bits))[0])
        qs16 = (w + off + n * 16).bitcast[UInt16]().load[width=8]()
        qs = bitcast[DType.uint8, 16](qs16)
        qh16 = (w + off + 32 + n * 8).bitcast[UInt16]().load[width=4]()
        qh = bitcast[DType.uint8, 8](qh16)

        col = c * 128
        comptime for j in range(4):
            ib = 4 * n + j
            sw = sc[ib // 2]
            dl1 = d * Float32(2 * Int((sw >> UInt16(6 * (ib % 2))) & 7) + 1)
            dl2 = d * Float32(2 * Int((sw >> UInt16(6 * (ib % 2) + 3)) & 7) + 1)
            h0 = Int(qh[2 * j])
            h1 = Int(qh[2 * j + 1])
            comptime for l in range(4):
                hb = h0 if l < 2 else h1
                shift = 8 if l % 2 == 0 else 4
                idx = Int(qs[4 * j + l]) | ((hb << shift) & 0x700)
                dbit = 0x08 if l % 2 == 0 else 0x80
                delta = -IQ1S_DELTA if (hb & dbit) != 0 else IQ1S_DELTA
                dl = dl1 if l < 2 else dl2
                mag = (
                    (grid + idx * 8)
                    .load[width=8, alignment=8]()
                    .cast[DType.int8]()
                    .cast[DType.float32]()
                )
                xv = (x + col + j * 32 + l * 8).load[width=8, alignment=16]().cast[
                    DType.float32
                ]()
                acc += dl * ((mag + delta) * xv).reduce_add()
        c += WARP
    return acc


def gemv_iq1_m_f16_v2(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ1_M GEMV, one warp per row. Grid.x = ceil(n_rows / 8), block = 256,
    n_cols % 256 == 0."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq1_m_row_acc(w, grid, x, n_cols, row, lane))
    if lane == 0:
        y[row] = Float16(total)


def gemv_iq1_m_out_f32_v2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """IQ1_M logit GEMV (f32 out), warp-per-row."""
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq1_m_row_acc(w, grid, x, n_cols, row, lane))
    if lane == 0:
        y[row] = total
