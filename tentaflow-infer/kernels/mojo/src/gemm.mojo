# ===== File: gemm.mojo — batched prefill GEMM: Y[T,rows] = X[T,cols] · W^T =====
# Tensor-core decomposition: one block computes a BM x 64 output tile
# (BM tokens x 64 weight rows) with m16n8k16 f16 mma. Two instantiations per
# weight format: BM=128 (8 warps) for tall grids and BM=64 (4 warps) whose
# doubled token-block count keeps small shapes (few weight rows and/or short
# chunks) from starving the SMs — the launcher picks per shape; both produce
# BIT-IDENTICAL outputs (same per-element mma chain). X streams row-major
# ([token][col], the natural activation layout — no transpose pass) through
# cp.async into shared memory; A fragments come from non-transposed ldmatrix
# over the [m][k] tile. W rows are staged n-major (a W row IS the mma B
# fragment, non-transposed ldmatrix). Both smem tiles are double-buffered so
# the next stage's copies overlap the current stage's mma work. Quantized
# variants dequantize W to f16 in registers on the way into smem.
# Out-of-range tokens/columns are clamped instead of guarded: clamped token
# rows re-read valid rows and their products land in output positions that
# are never stored; the k tail zero-fills W chunks so clamped X data
# multiplies against zeros.

from std.gpu import block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.memory import (
    AddressSpace,
    async_copy,
    async_copy_commit_group,
    async_copy_wait_group,
)
from std.memory import bitcast, stack_allocation
from std.gpu.compute.mma import mma, ld_matrix
from std.sys import _RegisterPackType
from std.sys._assembly import inlined_assembly
from src.gemv2 import _e2m1x8, _f8e4m3s, _q4k_scale_min, _q3k_scales8
from src.gemv2 import _iq4xs_scale, _e8m0_half, IQ4NL_VALS, MXFP4_VALS
from src.gemv2 import _signs8
from src.gemv2 import IQ1S_DELTA

comptime WARP = 32
comptime BN = 64
comptime BK = 32
comptime XPAD = 8
comptime LDK = BK + XPAD  # xs row stride (row-major token rows of k)
comptime LDW = BK + XPAD  # ws row stride (n-major rows of k)
comptime WTILE = BN * LDW


def _issue_x[BM: Int, NW: Int](
    s: Int,
    n_cols: Int,
    kc8: Int,
    xsrc0: UnsafePointer[Float16, MutAnyOrigin, address_space = AddressSpace.GLOBAL],
    xsrc1: UnsafePointer[Float16, MutAnyOrigin, address_space = AddressSpace.GLOBAL],
    xdst0: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xdst1: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
):
    """cp.async stage s's X tile into buffer (s % 2); the k tail clamps the
    column chunk (its data meets zeroed W columns, so products vanish)."""
    comptime XTILE = BM * LDK
    var kk = s * BK + kc8
    if kk > n_cols - 8:
        kk = n_cols - 8
    buf = s % 2
    async_copy[16](xsrc0 + kk - kc8, xdst0 + buf * XTILE)
    async_copy[16](xsrc1 + kk - kc8, xdst1 + buf * XTILE)


def _store_tile(
    y: UnsafePointer[Float16, MutAnyOrigin],
    acc: InlineArray[SIMD[DType.float32, 4], 8],
    t0: Int,
    row0: Int,
    warp_m: Int,
    warp_n: Int,
    group: Int,
    tid4: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Scatter the warp's 8 mma accumulators: d[0],d[1] -> (t, r), (t, r+1);
    d[2],d[3] land at t+8."""
    comptime for mi in range(2):
        comptime for ni in range(4):
            d = acc[mi * 4 + ni]
            t = t0 + warp_m + mi * 16 + group
            r = row0 + warp_n + ni * 8 + tid4 * 2
            if t < n_tokens and r < n_rows:
                y[t * n_rows + r] = Float16(d[0])
                if r + 1 < n_rows:
                    y[t * n_rows + r + 1] = Float16(d[1])
            if t + 8 < n_tokens and r < n_rows:
                y[(t + 8) * n_rows + r] = Float16(d[2])
                if r + 1 < n_rows:
                    y[(t + 8) * n_rows + r + 1] = Float16(d[3])


def gemm_f16_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """f16 tensor-core GEMM: Y[t, r] = dot(w[r], x[t]).

    Grid: (ceil(rows/64), ceil(T/BM)), block NW*32, n_cols % 8 == 0.
    W also streams through cp.async (no conversion needed).
    """
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4  # X token rows staged per pass (4 threads/row)
    comptime w_passes = (BN * 4) // NT  # W row-chunk passes (4 threads/row)
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    wk = (tid % 4) * 8
    var wsrc = InlineArray[
        UnsafePointer[Float16, MutAnyOrigin, address_space = AddressSpace.GLOBAL],
        w_passes,
    ](uninitialized=True)

    comptime for wp in range(w_passes):
        var wn = row0 + tid // 4 + wp * (NT // 4)
        if wn > n_rows - 1:
            wn = n_rows - 1
        wsrc[wp] = (w + wn * n_cols + wk).address_space_cast[AddressSpace.GLOBAL]()
    wdst = ws + (tid // 4) * LDW + wk

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = (n_cols + BK - 1) // BK

    def issue_w(
        s: Int,
        n_cols: Int,
        wk: Int,
        wsrc: InlineArray[
            UnsafePointer[
                Float16, MutAnyOrigin, address_space = AddressSpace.GLOBAL
            ],
            w_passes,
        ],
        wdst: UnsafePointer[
            Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
        ],
    ):
        k0 = s * BK

        comptime for wp in range(w_passes):
            if k0 + wk + 8 <= n_cols:
                async_copy[16](
                    wsrc[wp] + k0,
                    wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW,
                )
            else:
                (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                    width=8, alignment=16
                ](SIMD[DType.float16, 8](0.0))

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    issue_w(0, n_cols, wk, wsrc, wdst)
    async_copy_commit_group()

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            issue_w(s + 1, n_cols, wk, wsrc, wdst)
            async_copy_commit_group()
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


def gemm_q8_0_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Q8_0 tensor-core GEMM. Grid (ceil(rows/64), ceil(T/BM)), block NW*32,
    n_cols % 32 == 0 (one q8 block per row per stage). W is dequantized to
    f16 in registers (prefetched a stage ahead) and staged to smem; the
    f16-domain multiply keeps the hot loop lean."""
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    # W staging: 4 threads per row, 8 quants each; one q8 block per stage.
    part = tid % 4
    blocks_per_row = n_cols // 32
    var wbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        wbase[wp] = w + wrow * blocks_per_row * 34
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var scale = InlineArray[Float16, w_passes](uninitialized=True)
    var qv = InlineArray[SIMD[DType.uint8, 8], w_passes](uninitialized=True)

    comptime for wp in range(w_passes):
        scale[wp] = Float16((wbase[wp]).bitcast[Float16]()[0])
        qv[wp] = (wbase[wp] + 2 + part * 8).load[width=8, alignment=2]()

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        comptime for wp in range(w_passes):
            wv = qv[wp].cast[DType.int8]().cast[DType.float16]() * scale[wp]
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](wv)
        if s + 1 < n_stages:
            off = (s + 1) * 34

            comptime for wp in range(w_passes):
                scale[wp] = Float16((wbase[wp] + off).bitcast[Float16]()[0])
                qv[wp] = (wbase[wp] + off + 2 + part * 8).load[
                    width=8, alignment=2
                ]()
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


def gemm_q4_k_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Q4_K tensor-core GEMM. Grid (ceil(rows/64), ceil(T/BM)), block NW*32,
    n_cols % 256 == 0 (whole 144-byte superblocks per row). Each 32-col stage
    covers one 6-bit-scaled sub-block: nibbles are dequantized to f16 in
    registers (prefetched a stage ahead) and staged to smem like Q8_0."""
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    # W staging: 4 threads per row, 8 nibbles each; one sub-block per stage.
    part = tid % 4
    blocks_per_row = n_cols // 256
    var wbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        wbase[wp] = w + wrow * blocks_per_row * 144
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    def fetch_w(
        s: Int,
        part: Int,
        wbase: InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes],
        mut dsc: InlineArray[Float32, w_passes],
        mut dmn: InlineArray[Float32, w_passes],
        mut qv: InlineArray[SIMD[DType.uint8, 8], w_passes],
        mut high: InlineArray[Bool, w_passes],
    ):
        k0 = s * BK
        boff = (k0 // 256) * 144
        j = (k0 % 256) // 32
        chunk = (k0 % 256) // 64
        is_high = (k0 % 64) == 32

        comptime for wp in range(w_passes):
            hdr = (wbase[wp] + boff).load[width=16, alignment=16]()
            dm = bitcast[DType.float16, 8](hdr)
            sc, mn = _q4k_scale_min(hdr, j)
            dsc[wp] = Float32(dm[0]) * sc
            dmn[wp] = Float32(dm[1]) * mn
            qv[wp] = (wbase[wp] + boff + 16 + chunk * 32 + part * 8).load[
                width=8, alignment=8
            ]()
            high[wp] = is_high

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var dsc = InlineArray[Float32, w_passes](fill=0.0)
    var dmn = InlineArray[Float32, w_passes](fill=0.0)
    var qv = InlineArray[SIMD[DType.uint8, 8], w_passes](
        fill=SIMD[DType.uint8, 8](0)
    )
    var high = InlineArray[Bool, w_passes](fill=False)
    fetch_w(0, part, wbase, dsc, dmn, qv, high)

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        comptime for wp in range(w_passes):
            var nib: SIMD[DType.uint8, 8]
            if high[wp]:
                nib = qv[wp] >> 4
            else:
                nib = qv[wp] & 0x0F
            wv = nib.cast[DType.float16]() * Float16(dsc[wp]) - Float16(dmn[wp])
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](wv)
        if s + 1 < n_stages:
            fetch_w(s + 1, part, wbase, dsc, dmn, qv, high)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


def gemm_q6_k_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Q6_K tensor-core GEMM. Grid (ceil(rows/64), ceil(T/BM)), block NW*32,
    n_cols % 256 == 0 (whole 210-byte superblocks per row). Each 32-col stage
    covers one int8-scaled quadrant slice: ql nibbles + qh 2-bit highs are
    combined to (q - 32) f16 in registers (prefetched a stage ahead) and
    staged to smem like Q4_K. The 210-byte block is only 2-byte aligned, so
    raw bytes load as u16 lanes."""
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    # W staging: 4 threads per row, 8 quants each; one quadrant per stage.
    part = tid % 4
    blocks_per_row = n_cols // 256
    var wbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        wbase[wp] = w + wrow * blocks_per_row * 210
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    def fetch_w(
        s: Int,
        part: Int,
        wbase: InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes],
        mut dsc: InlineArray[Float32, w_passes],
        mut qlv: InlineArray[SIMD[DType.uint8, 8], w_passes],
        mut qhv: InlineArray[SIMD[DType.uint8, 8], w_passes],
    ):
        k0 = s * BK
        boff = (k0 // 256) * 210
        p = k0 % 256
        n = p // 128
        qd = (p % 128) // 32
        l0 = part * 8

        comptime for wp in range(w_passes):
            base = wbase[wp] + boff
            d = Float32((base + 208).bitcast[Float16]()[0])
            var sb = Int((base + 192 + n * 8 + 2 * qd + part // 2)[0])
            if sb > 127:
                sb -= 256
            dsc[wp] = d * Float32(sb)
            ql16 = (base + n * 64 + (qd % 2) * 32 + l0).bitcast[UInt16]().load[width=4]()
            qh16 = (base + 128 + n * 32 + l0).bitcast[UInt16]().load[width=4]()
            qlv[wp] = bitcast[DType.uint8, 8](ql16)
            qhv[wp] = bitcast[DType.uint8, 8](qh16)

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var dsc = InlineArray[Float32, w_passes](fill=0.0)
    var qlv = InlineArray[SIMD[DType.uint8, 8], w_passes](
        fill=SIMD[DType.uint8, 8](0)
    )
    var qhv = InlineArray[SIMD[DType.uint8, 8], w_passes](
        fill=SIMD[DType.uint8, 8](0)
    )
    fetch_w(0, part, wbase, dsc, qlv, qhv)

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        qd = ((s * BK) % 128) // 32
        comptime for wp in range(w_passes):
            var nib: SIMD[DType.uint8, 8]
            if qd >= 2:
                nib = qlv[wp] >> 4
            else:
                nib = qlv[wp] & 0x0F
            hi = (qhv[wp] >> UInt8(2 * qd)) & 3
            q = ((nib | (hi << 4)).cast[DType.int32]() - 32).cast[DType.float16]()
            wv = q * Float16(dsc[wp])
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](wv)
        if s + 1 < n_stages:
            fetch_w(s + 1, part, wbase, dsc, qlv, qhv)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


def gemm_nvfp4_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    inv_global_scale: Float32,
):
    """NVFP4 tensor-core GEMM. Grid (ceil(rows/64), ceil(T/BM)), block NW*32,
    n_cols % 16 == 0 (two scale groups per stage; a 16-col tail zero-fills
    the second group)."""
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    # W staging: 4 threads per row, 8 values (half a scale group) each.
    part = tid % 4
    groups = n_cols // 16
    var pbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )
    var sbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        pbase[wp] = packed + wrow * (n_cols // 2) + part * 4
        sbase[wp] = scales + wrow * groups + part // 2
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = (n_cols + BK - 1) // BK
    g_mine = part // 2  # this thread's group within the stage (0 or 1)

    def fetch_w(
        s: Int,
        groups: Int,
        g_mine: Int,
        inv_global_scale: Float32,
        pbase: InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes],
        sbase: InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes],
        mut qv: InlineArray[SIMD[DType.uint8, 4], w_passes],
        mut sc: InlineArray[Float32, w_passes],
    ):
        g_abs = s * 2 + g_mine

        comptime for wp in range(w_passes):
            if g_abs < groups:
                qv[wp] = (pbase[wp] + s * 16).load[width=4, alignment=4]()
                sc[wp] = _f8e4m3s(sbase[wp][s * 2]) * inv_global_scale
            else:
                qv[wp] = SIMD[DType.uint8, 4](0)
                sc[wp] = 0.0

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var qv = InlineArray[SIMD[DType.uint8, 4], w_passes](
        fill=SIMD[DType.uint8, 4](0)
    )
    var sc = InlineArray[Float32, w_passes](fill=0.0)
    fetch_w(0, groups, g_mine, inv_global_scale, pbase, sbase, qv, sc)

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        comptime for wp in range(w_passes):
            lo = qv[wp] & 0x0F
            hi = qv[wp] >> 4
            codes = lo.interleave(hi)
            wv = (_e2m1x8(codes) * sc[wp]).cast[DType.float16]()
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](wv)
        if s + 1 < n_stages:
            fetch_w(s + 1, groups, g_mine, inv_global_scale, pbase, sbase, qv, sc)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)



def gemm_q5_k_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Q5_K tensor-core GEMM. Grid (ceil(rows/64), ceil(T/BM)), block NW*32,
    n_cols % 256 == 0 (whole 176-byte superblocks per row). Same staging as
    Q4_K plus the qh high bit folded into each nibble in registers."""
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    part = tid % 4
    blocks_per_row = n_cols // 256
    var wbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        wbase[wp] = w + wrow * blocks_per_row * 176
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    def fetch_w(
        s: Int,
        part: Int,
        wbase: InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes],
        mut dsc: InlineArray[Float32, w_passes],
        mut dmn: InlineArray[Float32, w_passes],
        mut qv: InlineArray[SIMD[DType.uint8, 8], w_passes],
        mut qhv: InlineArray[SIMD[DType.uint8, 8], w_passes],
    ):
        k0 = s * BK
        boff = (k0 // 256) * 176
        j = (k0 % 256) // 32
        chunk = (k0 % 256) // 64

        comptime for wp in range(w_passes):
            hdr = (wbase[wp] + boff).load[width=16, alignment=16]()
            dm = bitcast[DType.float16, 8](hdr)
            sc, mn = _q4k_scale_min(hdr, j)
            dsc[wp] = Float32(dm[0]) * sc
            dmn[wp] = Float32(dm[1]) * mn
            qv[wp] = (wbase[wp] + boff + 48 + chunk * 32 + part * 8).load[
                width=8, alignment=8
            ]()
            qhv[wp] = (wbase[wp] + boff + 16 + part * 8).load[width=8, alignment=8]()

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var dsc = InlineArray[Float32, w_passes](fill=0.0)
    var dmn = InlineArray[Float32, w_passes](fill=0.0)
    var qv = InlineArray[SIMD[DType.uint8, 8], w_passes](
        fill=SIMD[DType.uint8, 8](0)
    )
    var qhv = InlineArray[SIMD[DType.uint8, 8], w_passes](
        fill=SIMD[DType.uint8, 8](0)
    )
    fetch_w(0, part, wbase, dsc, dmn, qv, qhv)

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        p = (s * BK) % 256
        chunk = p // 64
        is_high = (p % 64) == 32
        hbit = 2 * chunk + (1 if is_high else 0)
        comptime for wp in range(w_passes):
            var nib: SIMD[DType.uint8, 8]
            if is_high:
                nib = qv[wp] >> 4
            else:
                nib = qv[wp] & 0x0F
            q5 = nib | (((qhv[wp] >> UInt8(hbit)) & 1) << 4)
            wv = q5.cast[DType.float16]() * Float16(dsc[wp]) - Float16(dmn[wp])
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](wv)
        if s + 1 < n_stages:
            fetch_w(s + 1, part, wbase, dsc, dmn, qv, qhv)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


def gemm_q3_k_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Q3_K tensor-core GEMM. Grid (ceil(rows/64), ceil(T/BM)), block NW*32,
    n_cols % 256 == 0 (whole 110-byte superblocks per row). Each 32-col stage
    covers one 2-bit shift group; the packed 6-bit scale for the thread's
    16-column half unpacks in fetch_w. Only 2-byte aligned, so raw bytes load
    as u16 lanes."""
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    part = tid % 4
    blocks_per_row = n_cols // 256
    var wbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        wbase[wp] = w + wrow * blocks_per_row * 110
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    def fetch_w(
        s: Int,
        part: Int,
        wbase: InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes],
        mut dsc: InlineArray[Float32, w_passes],
        mut qsv: InlineArray[SIMD[DType.uint8, 8], w_passes],
        mut hmv: InlineArray[SIMD[DType.uint8, 8], w_passes],
    ):
        k0 = s * BK
        boff = (k0 // 256) * 110
        p = k0 % 256
        n = p // 128
        sh = (p % 128) // 32

        comptime for wp in range(w_passes):
            base = wbase[wp] + boff
            d = Float32((base + 108).bitcast[Float16]()[0])
            sc8 = _q3k_scales8(wbase[wp], boff, n)
            dsc[wp] = d * sc8[2 * sh + part // 2]
            qs16 = (base + 32 + n * 32 + part * 8).bitcast[UInt16]().load[width=4]()
            hm16 = (base + part * 8).bitcast[UInt16]().load[width=4]()
            qsv[wp] = bitcast[DType.uint8, 8](qs16)
            hmv[wp] = bitcast[DType.uint8, 8](hm16)

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var dsc = InlineArray[Float32, w_passes](fill=0.0)
    var qsv = InlineArray[SIMD[DType.uint8, 8], w_passes](
        fill=SIMD[DType.uint8, 8](0)
    )
    var hmv = InlineArray[SIMD[DType.uint8, 8], w_passes](
        fill=SIMD[DType.uint8, 8](0)
    )
    fetch_w(0, part, wbase, dsc, qsv, hmv)

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        p = (s * BK) % 256
        n = p // 128
        sh = (p % 128) // 32
        comptime for wp in range(w_passes):
            q = ((qsv[wp] >> UInt8(2 * sh)) & 3).cast[DType.int32]()
            hb = ((hmv[wp] >> UInt8(4 * n + sh)) & 1).cast[DType.int32]()
            v = (q + 4 * hb - 4).cast[DType.float16]()
            wv = v * Float16(dsc[wp])
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](wv)
        if s + 1 < n_stages:
            fetch_w(s + 1, part, wbase, dsc, qsv, hmv)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


def gemm_q2_k_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Q2_K tensor-core GEMM. Grid (ceil(rows/64), ceil(T/BM)), block NW*32,
    n_cols % 256 == 0 (whole 84-byte superblocks per row). Each thread's
    16-column half has one packed scale/min byte."""
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    part = tid % 4
    blocks_per_row = n_cols // 256
    var wbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        wbase[wp] = w + wrow * blocks_per_row * 84
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    def fetch_w(
        s: Int,
        part: Int,
        wbase: InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes],
        mut dsc: InlineArray[Float32, w_passes],
        mut dmn: InlineArray[Float32, w_passes],
        mut qsv: InlineArray[SIMD[DType.uint8, 8], w_passes],
    ):
        k0 = s * BK
        boff = (k0 // 256) * 84
        p = k0 % 256
        n = p // 128
        sh = (p % 128) // 32
        is0 = n * 8 + 2 * sh + part // 2

        comptime for wp in range(w_passes):
            base = wbase[wp] + boff
            dm16 = (base + 80).bitcast[UInt16]().load[width=2]()
            dm = bitcast[DType.float16, 2](dm16)
            scb = base[is0]
            dsc[wp] = Float32(dm[0]) * Float32(scb & 0x0F)
            dmn[wp] = Float32(dm[1]) * Float32(scb >> 4)
            qs16 = (base + 16 + n * 32 + part * 8).bitcast[UInt16]().load[width=4]()
            qsv[wp] = bitcast[DType.uint8, 8](qs16)

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var dsc = InlineArray[Float32, w_passes](fill=0.0)
    var dmn = InlineArray[Float32, w_passes](fill=0.0)
    var qsv = InlineArray[SIMD[DType.uint8, 8], w_passes](
        fill=SIMD[DType.uint8, 8](0)
    )
    fetch_w(0, part, wbase, dsc, dmn, qsv)

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        sh = ((s * BK) % 128) // 32
        comptime for wp in range(w_passes):
            q = ((qsv[wp] >> UInt8(2 * sh)) & 3).cast[DType.float16]()
            wv = q * Float16(dsc[wp]) - Float16(dmn[wp])
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](wv)
        if s + 1 < n_stages:
            fetch_w(s + 1, part, wbase, dsc, dmn, qsv)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


comptime _GEMM_IOTA8_U32 = SIMD[DType.uint32, 8](0, 1, 2, 3, 4, 5, 6, 7)


def gemm_legacy32_impl[BM: Int, NW: Int, FMT: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Legacy 32-element-block tensor-core GEMM (FMT 0 = Q4_0, 1 = Q4_1,
    2 = Q5_0, 3 = Q5_1). Grid (ceil(rows/64), ceil(T/BM)), block NW*32,
    n_cols % 32 == 0 (one block per stage, like Q8_0). Elements 0..15 are the
    low nibbles of the 16 quant bytes, 16..31 the high nibbles; Q5 formats
    add bit e of the block's qh word to element e (dequant.rs dq_q4_0
    family). Blocks are only 2-byte aligned, so bytes load as u16 lanes."""
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32
    comptime BB = 18 + 2 * FMT  # block bytes: 18 / 20 / 22 / 24
    comptime HAS_MIN = FMT == 1 or FMT == 3
    comptime HAS_QH = FMT >= 2
    comptime QS_OFF = 2 + (2 if FMT == 1 else 0) + (4 if FMT == 2 else 0) + (
        6 if FMT == 3 else 0
    )

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    part = tid % 4
    blocks_per_row = n_cols // 32
    var wbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        wbase[wp] = w + wrow * blocks_per_row * BB
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    def fetch_w(
        s: Int,
        part: Int,
        wbase: InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes],
        mut d: InlineArray[Float16, w_passes],
        mut m: InlineArray[Float16, w_passes],
        mut qh: InlineArray[UInt32, w_passes],
        mut qv: InlineArray[SIMD[DType.uint8, 8], w_passes],
    ):
        off = s * BB

        comptime for wp in range(w_passes):
            base = wbase[wp] + off
            d[wp] = (base).bitcast[Float16]()[0]
            comptime if HAS_MIN:
                m[wp] = (base + 2).bitcast[Float16]()[0]
            comptime if HAS_QH:
                h16 = (base + QS_OFF - 4).bitcast[UInt16]().load[width=2]()
                qh[wp] = UInt32(h16[0]) | (UInt32(h16[1]) << 16)
            q16 = (base + QS_OFF + (part % 2) * 8).bitcast[UInt16]().load[width=4]()
            qv[wp] = bitcast[DType.uint8, 8](q16)

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var d = InlineArray[Float16, w_passes](fill=Float16(0.0))
    var m = InlineArray[Float16, w_passes](fill=Float16(0.0))
    var qh = InlineArray[UInt32, w_passes](fill=UInt32(0))
    var qv = InlineArray[SIMD[DType.uint8, 8], w_passes](
        fill=SIMD[DType.uint8, 8](0)
    )
    fetch_w(0, part, wbase, d, m, qh, qv)

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        comptime for wp in range(w_passes):
            var nib: SIMD[DType.uint8, 8]
            if part >= 2:
                nib = qv[wp] >> 4
            else:
                nib = qv[wp] & 0x0F
            var q = nib.cast[DType.int32]()
            comptime if HAS_QH:
                hb = (
                    (SIMD[DType.uint32, 8](qh[wp]) >> (_GEMM_IOTA8_U32 + UInt32(part * 8)))
                    & 1
                ).cast[DType.int32]()
                q += hb * 16
            comptime if FMT == 0:
                q -= 8
            comptime if FMT == 2:
                q -= 16
            var wv = q.cast[DType.float16]() * d[wp]
            comptime if HAS_MIN:
                wv += m[wp]
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](wv)
        if s + 1 < n_stages:
            fetch_w(s + 1, part, wbase, d, m, qh, qv)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)



def gemm_iq4_lut_impl[BM: Int, NW: Int, XS: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Codebook 4-bit tensor-core GEMM (XS 0 = IQ4_NL 18-byte blocks, 1 =
    IQ4_XS 136-byte superblocks). Grid (ceil(rows/64), ceil(T/BM)), block
    NW*32; n_cols % 32 == 0 (NL) / % 256 == 0 (XS). Codebook values decode
    in registers from the comptime kvalues table while staging to smem."""
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    part = tid % 4
    comptime row_block_elems = 256 if XS == 1 else 32
    comptime row_block_bytes = 136 if XS == 1 else 18
    blocks_per_row = n_cols // row_block_elems
    var wbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        wbase[wp] = w + wrow * blocks_per_row * row_block_bytes
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    def fetch_w(
        s: Int,
        part: Int,
        wbase: InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes],
        mut dl: InlineArray[Float32, w_passes],
        mut qv: InlineArray[SIMD[DType.uint8, 8], w_passes],
    ):
        comptime if XS == 1:
            k0 = s * BK
            boff = (k0 // 256) * 136
            ib = (k0 % 256) // 32

            comptime for wp in range(w_passes):
                base = wbase[wp] + boff
                hdr = base.load[width=8, alignment=8]()
                d = Float32(bitcast[DType.float16, 4](hdr)[0])
                dl[wp] = d * _iq4xs_scale(hdr, ib)
                qv[wp] = (base + 8 + ib * 16 + (part % 2) * 8).load[
                    width=8, alignment=8
                ]()
        else:
            off = s * 18

            comptime for wp in range(w_passes):
                base = wbase[wp] + off
                dl[wp] = Float32(base.bitcast[Float16]()[0])
                q16 = (base + 2 + (part % 2) * 8).bitcast[UInt16]().load[width=4]()
                qv[wp] = bitcast[DType.uint8, 8](q16)

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var dl = InlineArray[Float32, w_passes](fill=0.0)
    var qv = InlineArray[SIMD[DType.uint8, 8], w_passes](
        fill=SIMD[DType.uint8, 8](0)
    )
    fetch_w(0, part, wbase, dl, qv)

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        comptime for wp in range(w_passes):
            var nib: SIMD[DType.uint8, 8]
            if part >= 2:
                nib = qv[wp] >> 4
            else:
                nib = qv[wp] & 0x0F
            var vals = SIMD[DType.float32, 8]()
            comptime for j in range(8):
                vals[j] = IQ4NL_VALS[Int(nib[j])]
            wv = (vals * dl[wp]).cast[DType.float16]()
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](wv)
        if s + 1 < n_stages:
            fetch_w(s + 1, part, wbase, dl, qv)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


def gemm_mxfp4_gguf_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """GGML MXFP4 tensor-core GEMM (17-byte blocks: E8M0 scale + 16 e2m1
    pair bytes). Grid (ceil(rows/64), ceil(T/BM)), block NW*32, n_cols % 32
    == 0. The odd block size leaves only byte alignment for the quant
    bytes."""
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    part = tid % 4
    blocks_per_row = n_cols // 32
    var wbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        wbase[wp] = w + wrow * blocks_per_row * 17
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    def fetch_w(
        s: Int,
        part: Int,
        wbase: InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes],
        mut dl: InlineArray[Float32, w_passes],
        mut qv: InlineArray[SIMD[DType.uint8, 8], w_passes],
    ):
        off = s * 17

        comptime for wp in range(w_passes):
            base = wbase[wp] + off
            dl[wp] = _e8m0_half(base[0])
            qv[wp] = (base + 1 + (part % 2) * 8).load[width=8, alignment=1]()

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var dl = InlineArray[Float32, w_passes](fill=0.0)
    var qv = InlineArray[SIMD[DType.uint8, 8], w_passes](
        fill=SIMD[DType.uint8, 8](0)
    )
    fetch_w(0, part, wbase, dl, qv)

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        comptime for wp in range(w_passes):
            var nib: SIMD[DType.uint8, 8]
            if part >= 2:
                nib = qv[wp] >> 4
            else:
                nib = qv[wp] & 0x0F
            var vals = SIMD[DType.float32, 8]()
            comptime for j in range(8):
                vals[j] = MXFP4_VALS[Int(nib[j])]
            wv = (vals * dl[wp]).cast[DType.float16]()
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](wv)
        if s + 1 < n_stages:
            fetch_w(s + 1, part, wbase, dl, qv)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)



def gemm_iq2_xs_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """IQ2_XS tensor-core GEMM (74-byte superblocks; grid/ksigns tables come
    in as device pointers). Grid (ceil(rows/64), ceil(T/BM)), block NW*32,
    n_cols % 256 == 0. Each thread stages one 8-element grid code."""
    comptime BBYTES = 74
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    part = tid % 4
    blocks_per_row = n_cols // 256
    var wbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        wbase[wp] = w + wrow * blocks_per_row * BBYTES
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    def fetch_w(
        s: Int,
        part: Int,
        grid: UnsafePointer[UInt8, MutAnyOrigin],
        ksigns: UnsafePointer[UInt8, MutAnyOrigin],
        wbase: InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes],
        mut vals: InlineArray[SIMD[DType.float32, 8], w_passes],
    ):
        k0 = s * BK
        boff = (k0 // 256) * 74
        ib32 = (k0 % 256) // 32

        comptime for wp in range(w_passes):
            base = wbase[wp] + boff
            d = Float32(base.bitcast[Float16]()[0])
            code = (base + 2 + ib32 * 8 + part * 2).bitcast[UInt16]()[0]
            scb = base[66 + ib32]
            snib = (scb & 0x0F) if part < 2 else (scb >> 4)
            db = d * (0.5 + Float32(snib)) * 0.25
            mag = (grid + Int(code & 511) * 8).load[width=8, alignment=8]().cast[
                DType.float32
            ]()
            vals[wp] = mag * _signs8(ksigns[Int(code >> 9)]) * db

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var vals = InlineArray[SIMD[DType.float32, 8], w_passes](
        fill=SIMD[DType.float32, 8](0.0)
    )
    fetch_w(0, part, grid, ksigns, wbase, vals)

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        comptime for wp in range(w_passes):
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](vals[wp].cast[DType.float16]())
        if s + 1 < n_stages:
            fetch_w(s + 1, part, grid, ksigns, wbase, vals)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


def gemm_iq2_s_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """IQ2_S tensor-core GEMM (82-byte superblocks; grid table as a device
    pointer, explicit per-code sign bytes). Grid (ceil(rows/64),
    ceil(T/BM)), block NW*32, n_cols % 256 == 0."""
    comptime BBYTES = 82
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    part = tid % 4
    blocks_per_row = n_cols // 256
    var wbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        wbase[wp] = w + wrow * blocks_per_row * BBYTES
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    def fetch_w(
        s: Int,
        part: Int,
        grid: UnsafePointer[UInt8, MutAnyOrigin],
        wbase: InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes],
        mut vals: InlineArray[SIMD[DType.float32, 8], w_passes],
    ):
        k0 = s * BK
        boff = (k0 // 256) * 82
        ib32 = (k0 % 256) // 32

        comptime for wp in range(w_passes):
            base = wbase[wp] + boff
            d = Float32(base.bitcast[Float16]()[0])
            idx = Int(base[2 + 4 * ib32 + part]) | (
                (Int(base[66 + ib32]) << (8 - 2 * part)) & 0x300
            )
            scb = base[74 + ib32]
            snib = (scb & 0x0F) if part < 2 else (scb >> 4)
            db = d * (0.5 + Float32(snib)) * 0.25
            mag = (grid + idx * 8).load[width=8, alignment=8]().cast[DType.float32]()
            vals[wp] = mag * _signs8(base[34 + 4 * ib32 + part]) * db

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var vals = InlineArray[SIMD[DType.float32, 8], w_passes](
        fill=SIMD[DType.float32, 8](0.0)
    )
    fetch_w(0, part, grid, wbase, vals)

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        comptime for wp in range(w_passes):
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](vals[wp].cast[DType.float16]())
        if s + 1 < n_stages:
            fetch_w(s + 1, part, grid, wbase, vals)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


def gemm_iq3_s_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """IQ3_S tensor-core GEMM (110-byte superblocks; u32 grid packs 4
    magnitudes). Grid (ceil(rows/64), ceil(T/BM)), block NW*32, n_cols % 256
    == 0. Each thread stages one grid1|grid2 code pair."""
    comptime BBYTES = 110
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    part = tid % 4
    blocks_per_row = n_cols // 256
    var wbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        wbase[wp] = w + wrow * blocks_per_row * BBYTES
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    def fetch_w(
        s: Int,
        part: Int,
        grid: UnsafePointer[UInt8, MutAnyOrigin],
        wbase: InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes],
        mut vals: InlineArray[SIMD[DType.float32, 8], w_passes],
    ):
        k0 = s * BK
        boff = (k0 // 256) * 110
        ib32 = (k0 % 256) // 32

        comptime for wp in range(w_passes):
            base = wbase[wp] + boff
            d = Float32(base.bitcast[Float16]()[0])
            scb = base[106 + ib32 // 2]
            snib = (scb & 0x0F) if ib32 % 2 == 0 else (scb >> 4)
            db = d * Float32(1 + 2 * Int(snib))
            h = Int(base[66 + ib32])
            i1 = Int(base[2 + 8 * ib32 + 2 * part]) | ((h << (8 - 2 * part)) & 256)
            i2 = Int(base[2 + 8 * ib32 + 2 * part + 1]) | ((h << (7 - 2 * part)) & 256)
            m1 = (grid + i1 * 4).load[width=4, alignment=4]()
            m2 = (grid + i2 * 4).load[width=4, alignment=4]()
            vals[wp] = m1.join(m2).cast[DType.float32]() * _signs8(
                base[74 + 4 * ib32 + part]
            ) * db

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var vals = InlineArray[SIMD[DType.float32, 8], w_passes](
        fill=SIMD[DType.float32, 8](0.0)
    )
    fetch_w(0, part, grid, wbase, vals)

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        comptime for wp in range(w_passes):
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](vals[wp].cast[DType.float16]())
        if s + 1 < n_stages:
            fetch_w(s + 1, part, grid, wbase, vals)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)



def gemm_iq2_xxs_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """IQ2_XXS tensor-core GEMM (66-byte superblocks; grid/ksigns as device
    pointers). Grid (ceil(rows/64), ceil(T/BM)), block NW*32, n_cols % 256
    == 0. Each thread stages one 8-element grid code."""
    comptime BBYTES = 66
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    part = tid % 4
    blocks_per_row = n_cols // 256
    var wbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        wbase[wp] = w + wrow * blocks_per_row * BBYTES
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    def fetch_w(
        s: Int,
        part: Int,
        grid: UnsafePointer[UInt8, MutAnyOrigin],
        ksigns: UnsafePointer[UInt8, MutAnyOrigin],
        wbase: InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes],
        mut vals: InlineArray[SIMD[DType.float32, 8], w_passes],
    ):
        k0 = s * BK
        boff = (k0 // 256) * 66
        ib32 = (k0 % 256) // 32

        comptime for wp in range(w_passes):
            base = wbase[wp] + boff
            d = Float32(base.bitcast[Float16]()[0])
            aux1 = (
                UInt32(base[2 + 8 * ib32 + 4])
                | (UInt32(base[2 + 8 * ib32 + 5]) << 8)
                | (UInt32(base[2 + 8 * ib32 + 6]) << 16)
                | (UInt32(base[2 + 8 * ib32 + 7]) << 24)
            )
            db = d * (0.5 + Float32(aux1 >> 28)) * 0.25
            mag = (grid + Int(base[2 + 8 * ib32 + part]) * 8).load[
                width=8, alignment=8
            ]().cast[DType.float32]()
            vals[wp] = mag * _signs8(
                ksigns[Int((aux1 >> UInt32(7 * part)) & 127)]
            ) * db

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var vals = InlineArray[SIMD[DType.float32, 8], w_passes](
        fill=SIMD[DType.float32, 8](0.0)
    )
    fetch_w(0, part, grid, ksigns, wbase, vals)

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        comptime for wp in range(w_passes):
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](vals[wp].cast[DType.float16]())
        if s + 1 < n_stages:
            fetch_w(s + 1, part, grid, ksigns, wbase, vals)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


def gemm_iq3_xxs_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """IQ3_XXS tensor-core GEMM (98-byte superblocks; u32 grid packs 4
    magnitudes). Grid (ceil(rows/64), ceil(T/BM)), block NW*32, n_cols %
    256 == 0."""
    comptime BBYTES = 98
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    part = tid % 4
    blocks_per_row = n_cols // 256
    var wbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        wbase[wp] = w + wrow * blocks_per_row * BBYTES
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    def fetch_w(
        s: Int,
        part: Int,
        grid: UnsafePointer[UInt8, MutAnyOrigin],
        ksigns: UnsafePointer[UInt8, MutAnyOrigin],
        wbase: InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes],
        mut vals: InlineArray[SIMD[DType.float32, 8], w_passes],
    ):
        k0 = s * BK
        boff = (k0 // 256) * 98
        ib32 = (k0 % 256) // 32

        comptime for wp in range(w_passes):
            base = wbase[wp] + boff
            d = Float32(base.bitcast[Float16]()[0])
            aux = (
                UInt32(base[66 + 4 * ib32])
                | (UInt32(base[66 + 4 * ib32 + 1]) << 8)
                | (UInt32(base[66 + 4 * ib32 + 2]) << 16)
                | (UInt32(base[66 + 4 * ib32 + 3]) << 24)
            )
            db = d * (0.5 + Float32(aux >> 28)) * 0.5
            m1 = (grid + Int(base[2 + 8 * ib32 + 2 * part]) * 4).load[
                width=4, alignment=4
            ]()
            m2 = (grid + Int(base[2 + 8 * ib32 + 2 * part + 1]) * 4).load[
                width=4, alignment=4
            ]()
            vals[wp] = m1.join(m2).cast[DType.float32]() * _signs8(
                ksigns[Int((aux >> UInt32(7 * part)) & 127)]
            ) * db

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var vals = InlineArray[SIMD[DType.float32, 8], w_passes](
        fill=SIMD[DType.float32, 8](0.0)
    )
    fetch_w(0, part, grid, ksigns, wbase, vals)

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        comptime for wp in range(w_passes):
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](vals[wp].cast[DType.float16]())
        if s + 1 < n_stages:
            fetch_w(s + 1, part, grid, ksigns, wbase, vals)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


def gemm_iq1_s_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """IQ1_S tensor-core GEMM (50-byte superblocks; signed i8 grid rows +
    ±0.125 delta). Grid (ceil(rows/64), ceil(T/BM)), block NW*32, n_cols %
    256 == 0."""
    comptime BBYTES = 50
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    part = tid % 4
    blocks_per_row = n_cols // 256
    var wbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        wbase[wp] = w + wrow * blocks_per_row * BBYTES
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    def fetch_w(
        s: Int,
        part: Int,
        grid: UnsafePointer[UInt8, MutAnyOrigin],
        wbase: InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes],
        mut vals: InlineArray[SIMD[DType.float32, 8], w_passes],
    ):
        k0 = s * BK
        boff = (k0 // 256) * 50
        ib32 = (k0 % 256) // 32

        comptime for wp in range(w_passes):
            base = wbase[wp] + boff
            d = Float32(base.bitcast[Float16]()[0])
            h = (base + 34 + 2 * ib32).bitcast[UInt16]()[0]
            dl = d * Float32(2 * Int((h >> 12) & 7) + 1)
            delta = -IQ1S_DELTA if (h & 0x8000) != 0 else IQ1S_DELTA
            idx = Int(base[2 + 4 * ib32 + part]) | (
                Int((h >> UInt16(3 * part)) & 7) << 8
            )
            mag = (
                (grid + idx * 8)
                .load[width=8, alignment=8]()
                .cast[DType.int8]()
                .cast[DType.float32]()
            )
            vals[wp] = (mag + delta) * dl

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var vals = InlineArray[SIMD[DType.float32, 8], w_passes](
        fill=SIMD[DType.float32, 8](0.0)
    )
    fetch_w(0, part, grid, wbase, vals)

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        comptime for wp in range(w_passes):
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](vals[wp].cast[DType.float16]())
        if s + 1 < n_stages:
            fetch_w(s + 1, part, grid, wbase, vals)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


def gemm_iq1_m_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """IQ1_M tensor-core GEMM (56-byte superblocks; packed d nibbles in the
    scale words). Grid (ceil(rows/64), ceil(T/BM)), block NW*32, n_cols %
    256 == 0."""
    comptime BBYTES = 56
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    part = tid % 4
    blocks_per_row = n_cols // 256
    var wbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        wbase[wp] = w + wrow * blocks_per_row * BBYTES
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    def fetch_w(
        s: Int,
        part: Int,
        grid: UnsafePointer[UInt8, MutAnyOrigin],
        wbase: InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes],
        mut vals: InlineArray[SIMD[DType.float32, 8], w_passes],
    ):
        k0 = s * BK
        boff = (k0 // 256) * 56
        ib32 = (k0 % 256) // 32

        comptime for wp in range(w_passes):
            base = wbase[wp] + boff
            sc = (base + 48).bitcast[UInt16]().load[width=4]()
            d_bits = (
                (sc[0] >> 12)
                | ((sc[1] >> 8) & 0x00F0)
                | ((sc[2] >> 4) & 0x0F00)
                | (sc[3] & 0xF000)
            )
            d = Float32(
                bitcast[DType.float16, 1](SIMD[DType.uint16, 1](d_bits))[0]
            )
            sw = sc[ib32 // 2]
            var dl: Float32
            if part < 2:
                dl = d * Float32(2 * Int((sw >> UInt16(6 * (ib32 % 2))) & 7) + 1)
            else:
                dl = d * Float32(
                    2 * Int((sw >> UInt16(6 * (ib32 % 2) + 3)) & 7) + 1
                )
            hb = Int(base[32 + 2 * ib32 + part // 2])
            shift = 8 if part % 2 == 0 else 4
            idx = Int(base[4 * ib32 + part]) | ((hb << shift) & 0x700)
            dbit = 0x08 if part % 2 == 0 else 0x80
            delta = -IQ1S_DELTA if (hb & dbit) != 0 else IQ1S_DELTA
            mag = (
                (grid + idx * 8)
                .load[width=8, alignment=8]()
                .cast[DType.int8]()
                .cast[DType.float32]()
            )
            vals[wp] = (mag + delta) * dl

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var vals = InlineArray[SIMD[DType.float32, 8], w_passes](
        fill=SIMD[DType.float32, 8](0.0)
    )
    fetch_w(0, part, grid, wbase, vals)

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        comptime for wp in range(w_passes):
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](vals[wp].cast[DType.float16]())
        if s + 1 < n_stages:
            fetch_w(s + 1, part, grid, wbase, vals)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


def _store_tile_f32(
    y: UnsafePointer[Float32, MutAnyOrigin],
    acc: InlineArray[SIMD[DType.float32, 4], 8],
    t0: Int,
    row0: Int,
    warp_m: Int,
    warp_n: Int,
    group: Int,
    tid4: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """`_store_tile` writing f32 logits verbatim from the mma accumulator (no
    f16 rounding), so batched logits keep the same precision as the single-row
    gemv_*_out_f32 path."""
    comptime for mi in range(2):
        comptime for ni in range(4):
            d = acc[mi * 4 + ni]
            t = t0 + warp_m + mi * 16 + group
            r = row0 + warp_n + ni * 8 + tid4 * 2
            if t < n_tokens and r < n_rows:
                y[t * n_rows + r] = d[0]
                if r + 1 < n_rows:
                    y[t * n_rows + r + 1] = d[1]
            if t + 8 < n_tokens and r < n_rows:
                y[(t + 8) * n_rows + r] = d[2]
                if r + 1 < n_rows:
                    y[(t + 8) * n_rows + r + 1] = d[3]


def gemm_f16_out_f32_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """f16 tensor-core GEMM emitting f32 outputs (batched logit head). Same
    tiling/dataflow as gemm_f16_impl; only the store keeps f32."""
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    wk = (tid % 4) * 8
    var wsrc = InlineArray[
        UnsafePointer[Float16, MutAnyOrigin, address_space = AddressSpace.GLOBAL],
        w_passes,
    ](uninitialized=True)

    comptime for wp in range(w_passes):
        var wn = row0 + tid // 4 + wp * (NT // 4)
        if wn > n_rows - 1:
            wn = n_rows - 1
        wsrc[wp] = (w + wn * n_cols + wk).address_space_cast[AddressSpace.GLOBAL]()
    wdst = ws + (tid // 4) * LDW + wk

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = (n_cols + BK - 1) // BK

    def issue_w(
        s: Int,
        n_cols: Int,
        wk: Int,
        wsrc: InlineArray[
            UnsafePointer[
                Float16, MutAnyOrigin, address_space = AddressSpace.GLOBAL
            ],
            w_passes,
        ],
        wdst: UnsafePointer[
            Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
        ],
    ):
        k0 = s * BK

        comptime for wp in range(w_passes):
            if k0 + wk + 8 <= n_cols:
                async_copy[16](
                    wsrc[wp] + k0,
                    wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW,
                )
            else:
                (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                    width=8, alignment=16
                ](SIMD[DType.float16, 8](0.0))

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    issue_w(0, n_cols, wk, wsrc, wdst)
    async_copy_commit_group()

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            issue_w(s + 1, n_cols, wk, wsrc, wdst)
            async_copy_commit_group()
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile_f32(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


def gemm_q8_0_out_f32_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Q8_0 tensor-core GEMM emitting f32 outputs (batched logit head). Same
    dataflow as gemm_q8_0_impl; only the store keeps f32."""
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP
    comptime x_rpp = NT // 4
    comptime w_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    xrow = tid // 4
    kc8 = (tid % 4) * 8
    var xr0 = t0 + xrow
    if xr0 > n_tokens - 1:
        xr0 = n_tokens - 1
    var xr1 = t0 + xrow + x_rpp
    if xr1 > n_tokens - 1:
        xr1 = n_tokens - 1
    xsrc0 = (x + xr0 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xsrc1 = (x + xr1 * n_cols + kc8).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xrow * LDK + kc8
    xdst1 = xdst0 + x_rpp * LDK

    part = tid % 4
    blocks_per_row = n_cols // 32
    var wbase = InlineArray[UnsafePointer[UInt8, MutAnyOrigin], w_passes](
        uninitialized=True
    )

    comptime for wp in range(w_passes):
        var wrow = row0 + tid // 4 + wp * (NT // 4)
        if wrow > n_rows - 1:
            wrow = n_rows - 1
        wbase[wp] = w + wrow * blocks_per_row * 34
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % m_warps) * 32
    warp_n = (wid // m_warps) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lr) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    _issue_x[BM, NW](0, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
    async_copy_commit_group()
    var scale = InlineArray[Float16, w_passes](uninitialized=True)
    var qv = InlineArray[SIMD[DType.uint8, 8], w_passes](uninitialized=True)

    comptime for wp in range(w_passes):
        scale[wp] = Float16((wbase[wp]).bitcast[Float16]()[0])
        qv[wp] = (wbase[wp] + 2 + part * 8).load[width=8, alignment=2]()

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x[BM, NW](s + 1, n_cols, kc8, xsrc0, xsrc1, xdst0, xdst1)
            async_copy_commit_group()

        comptime for wp in range(w_passes):
            wv = qv[wp].cast[DType.int8]().cast[DType.float16]() * scale[wp]
            (wdst + (s % 2) * WTILE + wp * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](wv)
        if s + 1 < n_stages:
            off = (s + 1) * 34

            comptime for wp in range(w_passes):
                scale[wp] = Float16((wbase[wp] + off).bitcast[Float16]()[0])
                qv[wp] = (wbase[wp] + off + 2 + part * 8).load[
                    width=8, alignment=2
                ]()
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8](a_base + buf * XTILE + kb)
            a1 = ld_matrix[8](a_base + buf * XTILE + kb + 16 * LDK)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile_f32(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


# ===== int8 TENSOR-CORE MMQ prefill GEMM =====
# MMQ contract (q8_1-quantized activation tile, native weight codes, per-32-block
# scale/min), with the inner K=32 MAC on the int8 tensor cores via
# mma.sync.aligned.m16n8k32.row.col.s32.s8.s8.s32 (2x the f16-TC MAC throughput,
# zero dequant bandwidth — the only path that beats the f16 GEMM on Ada; dp4a on
# the CUDA cores was measured ~1.8x slower). The s8x s8 -> s32 tile covers one full
# Q-block per instruction; each block's s32 accumulators are scaled to f32 and
# summed (block scales differ, so accumulation is f32 outside the tensor op).
# 256-thread block = 8 warps; one block owns a BM-token x 64-row output tile.
# Warps split as M_WARPS x N_WARPS; each warp owns MT_PER_WARP token m-tiles and
# NT_PER_WARP 8-row n-tiles. Staging = activation q8_1 quant + weight-code
# unpack + per-block scales into shared, software-pipelined a stage ahead.
def _mma_s8(
    a0: UInt32,
    a1: UInt32,
    a2: UInt32,
    a3: UInt32,
    b0: UInt32,
    b1: UInt32,
    c: SIMD[DType.int32, 4],
) -> SIMD[DType.int32, 4]:
    var r = inlined_assembly[
        (
            "mma.sync.aligned.m16n8k32.row.col.s32.s8.s8.s32 {$0, $1, $2,"
            " $3}, {$4, $5, $6, $7}, {$8, $9}, {$10, $11, $12, $13};"
        ),
        _RegisterPackType[Int32, Int32, Int32, Int32],
        constraints="=r,=r,=r,=r,r,r,r,r,r,r,r,r,r,r",
        has_side_effect=False,
    ](a0, a1, a2, a3, b0, b1, c[0], c[1], c[2], c[3])
    return SIMD[DType.int32, 4](r[0], r[1], r[2], r[3])


def gemm_i8mma_impl[BM: Int, FMT: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    # 256-thread block = 8 warps. Each warp owns MT_PER_WARP=2 token m-tiles
    # (32 tokens) and NT_PER_WARP row n-tiles, so A fragments are reused across
    # n-tiles and B fragments across m-tiles — this halves redundant weight
    # smem traffic vs a 1-m-tile layout (mirrors the f16 kernel's warp tiling).
    comptime MT_PER_WARP = 2
    comptime M_WARPS = BM // 32
    comptime N_WARPS = 8 // M_WARPS
    comptime NT_PER_WARP = 8 // N_WARPS
    comptime BLK_BYTES = 34 if FMT == 0 else 144
    comptime BPR_DIV = 32 if FMT == 0 else 256

    tid = Int(thread_idx.x)
    row0 = Int(block_idx.x) * 64
    t0 = Int(block_idx.y) * BM

    xq = stack_allocation[
        2 * BM * 32, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    wq = stack_allocation[
        2 * 64 * 32, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    xd = stack_allocation[2 * BM, Float32, address_space = AddressSpace.SHARED]()
    xsm = stack_allocation[
        2 * BM, Float32, address_space = AddressSpace.SHARED
    ]()
    wdsc = stack_allocation[
        2 * 64, Float32, address_space = AddressSpace.SHARED
    ]()
    wdmn = stack_allocation[
        2 * 64, Float32, address_space = AddressSpace.SHARED
    ]()

    # X staging: one token per thread (tid < BM).
    var xtok_c = t0 + tid
    if xtok_c > n_tokens - 1:
        xtok_c = n_tokens - 1

    # W staging: 4 threads per row, 8 codes each.
    row_l = tid // 4
    part = tid % 4
    var wrow_c = row0 + row_l
    if wrow_c > n_rows - 1:
        wrow_c = n_rows - 1
    blocks_per_row = n_cols // BPR_DIV
    wrow_base = wrow_c * blocks_per_row * BLK_BYTES

    n_stages = n_cols // 32

    # Warp / lane identity for the mma fragments.
    wid = tid // 32
    lane = tid % 32
    g = lane >> 2
    tt = lane & 3
    sub = lane // 8  # ldmatrix.x4 tile selector
    lr = lane % 8  # ldmatrix row within tile
    wid_m = wid % M_WARPS
    wid_n = wid // M_WARPS
    mt0 = wid_m * (MT_PER_WARP * 16)  # token base for this warp
    nbase = wid_n * NT_PER_WARP

    var acc = InlineArray[SIMD[DType.float32, 4], MT_PER_WARP * NT_PER_WARP](
        fill=SIMD[DType.float32, 4](0.0)
    )

    # Stage-ahead prefetch registers (raw global reads).
    var xf = SIMD[DType.float16, 32](0)
    var wcodes = SIMD[DType.uint8, 8](0)
    var wsc = Float16(0)
    var whdr = SIMD[DType.uint8, 16](0)

    comptime W_QOFF = 2 if FMT == 0 else 16

    if tid < BM:
        xf = (x + xtok_c * n_cols).load[width=32, alignment=64]()
    comptime if FMT == 0:
        wcodes = (w + wrow_base + W_QOFF + part * 8).load[width=8, alignment=2]()
        if part == 0:
            wsc = (w + wrow_base).bitcast[Float16]()[0]
    else:
        wcodes = (w + wrow_base + W_QOFF + part * 8).load[width=8, alignment=8]()
        if part == 0:
            whdr = (w + wrow_base).load[width=16, alignment=16]()

    # Software pipeline (mirrors the f16 kernel): each iteration computes stage
    # s from buffer s%2 while stage s+1 is quantized/unpacked into the OTHER
    # buffer (via `sw`) and stage s+2's raw global reads prefetch into registers
    # (via `gl`), so the activation-quant + weight-unpack work hides in the
    # tensor pipe's shadow. One barrier per stage.
    @parameter
    @always_inline
    def sw(sidx: Int, buf: Int):
        if tid < BM:
            xv = xf.cast[DType.float32]()
            amax = abs(xv).reduce_max()
            if amax == 0.0:
                (xq + buf * BM * 32 + tid * 32).store[alignment=32](
                    SIMD[DType.int8, 32](0)
                )
                xd[buf * BM + tid] = 0.0
                xsm[buf * BM + tid] = 0.0
            else:
                d = amax * (1.0 / 127.0)
                q = round(xv * (127.0 / amax)).cast[DType.int8]()
                (xq + buf * BM * 32 + tid * 32).store[alignment=32](q)
                xd[buf * BM + tid] = d
                var sumq: Int32 = 0
                comptime for e in range(32):
                    sumq += Int32(q[e])
                xsm[buf * BM + tid] = d * Float32(sumq)

        var codes8: SIMD[DType.int8, 8]
        comptime if FMT == 0:
            codes8 = wcodes.cast[DType.int8]()
            if part == 0:
                wdsc[buf * 64 + row_l] = Float32(wsc)
        else:
            half = (sidx % 8) % 2
            codes8 = ((wcodes >> UInt8(4 * half)) & 0x0F).cast[DType.int8]()
            if part == 0:
                dm = bitcast[DType.float16, 8](whdr)
                sc, mn = _q4k_scale_min(whdr, sidx % 8)
                wdsc[buf * 64 + row_l] = Float32(dm[0]) * sc
                wdmn[buf * 64 + row_l] = Float32(dm[1]) * mn
        (wq + buf * 64 * 32 + row_l * 32 + part * 8).store[alignment=8](codes8)

    @parameter
    @always_inline
    def gl(sidx: Int):
        if tid < BM:
            xf = (x + xtok_c * n_cols + sidx * 32).load[
                width=32, alignment=64
            ]()
        comptime if FMT == 0:
            wcodes = (w + wrow_base + sidx * 34 + 2 + part * 8).load[
                width=8, alignment=2
            ]()
            if part == 0:
                wsc = (w + wrow_base + sidx * 34).bitcast[Float16]()[0]
        else:
            nsb = sidx // 8
            nsub = sidx % 8
            nchunk = nsub // 2
            wcodes = (
                w + wrow_base + nsb * 144 + 16 + nchunk * 32 + part * 8
            ).load[width=8, alignment=8]()
            if part == 0:
                whdr = (w + wrow_base + nsb * 144).load[
                    width=16, alignment=16
                ]()

    # Prologue: stage 0 -> buf 0, prefetch stage 1's global reads.
    sw(0, 0)
    if n_stages > 1:
        gl(1)
    barrier()

    var s = 0
    while s < n_stages:
        buf = s % 2

        # Stage s+1 into the other buffer (overlaps this stage's compute), then
        # prefetch stage s+2's global reads into registers.
        if s + 1 < n_stages:
            sw(s + 1, (s + 1) % 2)
        if s + 2 < n_stages:
            gl(s + 2)

        # int8 tensor-core MAC over this 32-column block. Fragments load via
        # ld_matrix (b16 view: 32 int8/row = 16 b16), one warp-wide instruction
        # each instead of per-thread scalar loads — the f16 kernel's lever.
        # MT_PER_WARP A fragments are reused across all NT_PER_WARP n-tiles.
        Af = (xq + buf * BM * 32 + mt0 * 32).bitcast[Float16]()
        var ai = InlineArray[SIMD[DType.uint32, 4], MT_PER_WARP](
            fill=SIMD[DType.uint32, 4](0)
        )
        # Per-block activation scales per m-tile (broadcast a,a,b,b over the
        # fragment's 4 outputs). The scale epilogue is vectorized to SIMD[f32,4].
        var dxv = InlineArray[SIMD[DType.float32, 4], MT_PER_WARP](
            fill=SIMD[DType.float32, 4](0)
        )
        var xsv = InlineArray[SIMD[DType.float32, 4], MT_PER_WARP](
            fill=SIMD[DType.float32, 4](0)
        )
        comptime for mi in range(MT_PER_WARP):
            a_base = Af + (mi * 16 + (sub % 2) * 8 + lr) * 16 + (sub // 2) * 8
            ai[mi] = bitcast[DType.uint32, 4](ld_matrix[8](a_base))
            dx_a = xd[buf * BM + mt0 + mi * 16 + g]
            dx_b = xd[buf * BM + mt0 + mi * 16 + g + 8]
            dxv[mi] = SIMD[DType.float32, 4](dx_a, dx_a, dx_b, dx_b)
            comptime if FMT == 1:
                xs_a = xsm[buf * BM + mt0 + mi * 16 + g]
                xs_b = xsm[buf * BM + mt0 + mi * 16 + g + 8]
                xsv[mi] = SIMD[DType.float32, 4](xs_a, xs_a, xs_b, xs_b)

        comptime for nti in range(NT_PER_WARP):
            nb = (nbase + nti) * 8
            Bf = (wq + buf * 64 * 32 + nb * 32).bitcast[Float16]()
            b_base = Bf + lr * 16 + (sub % 2) * 8
            bi = bitcast[DType.uint32, 2](ld_matrix[4](b_base))
            dw2 = (wdsc + buf * 64 + nb + 2 * tt).load[width=2]()
            dwv = SIMD[DType.float32, 4](dw2[0], dw2[1], dw2[0], dw2[1])
            var mnv = SIMD[DType.float32, 4](0)
            comptime if FMT == 1:
                mn2 = (wdmn + buf * 64 + nb + 2 * tt).load[width=2]()
                mnv = SIMD[DType.float32, 4](mn2[0], mn2[1], mn2[0], mn2[1])
            comptime for mi in range(MT_PER_WARP):
                mres = _mma_s8(
                    ai[mi][0], ai[mi][1], ai[mi][2], ai[mi][3],
                    bi[0], bi[1], SIMD[DType.int32, 4](0),
                )
                acc[mi * NT_PER_WARP + nti] += (
                    dxv[mi] * dwv * mres.cast[DType.float32]()
                )
                comptime if FMT == 1:
                    acc[mi * NT_PER_WARP + nti] -= mnv * xsv[mi]
        barrier()
        s += 1

    comptime for mi in range(MT_PER_WARP):
        tok_a = t0 + mt0 + mi * 16 + g
        tok_b = t0 + mt0 + mi * 16 + g + 8
        comptime for nti in range(NT_PER_WARP):
            nb = (nbase + nti) * 8
            r_a = row0 + nb + 2 * tt
            r_b = row0 + nb + 2 * tt + 1
            d4 = acc[mi * NT_PER_WARP + nti]
            if tok_a < n_tokens:
                if r_a < n_rows:
                    y[tok_a * n_rows + r_a] = Float16(d4[0])
                if r_b < n_rows:
                    y[tok_a * n_rows + r_b] = Float16(d4[1])
            if tok_b < n_tokens:
                if r_a < n_rows:
                    y[tok_b * n_rows + r_a] = Float16(d4[2])
                if r_b < n_rows:
                    y[tok_b * n_rows + r_b] = Float16(d4[3])


comptime gemm_q8_0_i8mma = gemm_i8mma_impl[128, 0]
comptime gemm_q8_0_i8mma_bm64 = gemm_i8mma_impl[64, 0]
comptime gemm_q4_k_i8mma = gemm_i8mma_impl[128, 1]
comptime gemm_q4_k_i8mma_bm64 = gemm_i8mma_impl[64, 1]


comptime gemm_f16_out_f32 = gemm_f16_out_f32_impl[128, 8]
comptime gemm_f16_out_f32_bm64 = gemm_f16_out_f32_impl[64, 4]
comptime gemm_q8_0_out_f32 = gemm_q8_0_out_f32_impl[128, 8]
comptime gemm_q8_0_out_f32_bm64 = gemm_q8_0_out_f32_impl[64, 4]
comptime gemm_f16 = gemm_f16_impl[128, 8]
comptime gemm_f16_bm64 = gemm_f16_impl[64, 4]
comptime gemm_q8_0_f16 = gemm_q8_0_impl[128, 8]
comptime gemm_q8_0_f16_bm64 = gemm_q8_0_impl[64, 4]
comptime gemm_q4_k_f16 = gemm_q4_k_impl[128, 8]
comptime gemm_q4_k_f16_bm64 = gemm_q4_k_impl[64, 4]
comptime gemm_q6_k_f16 = gemm_q6_k_impl[128, 8]
comptime gemm_q6_k_f16_bm64 = gemm_q6_k_impl[64, 4]
comptime gemm_nvfp4_f16 = gemm_nvfp4_impl[128, 8]
comptime gemm_nvfp4_f16_bm64 = gemm_nvfp4_impl[64, 4]
comptime gemm_q5_k_f16 = gemm_q5_k_impl[128, 8]
comptime gemm_q5_k_f16_bm64 = gemm_q5_k_impl[64, 4]
comptime gemm_q3_k_f16 = gemm_q3_k_impl[128, 8]
comptime gemm_q3_k_f16_bm64 = gemm_q3_k_impl[64, 4]
comptime gemm_q2_k_f16 = gemm_q2_k_impl[128, 8]
comptime gemm_q2_k_f16_bm64 = gemm_q2_k_impl[64, 4]
comptime gemm_q4_0_f16 = gemm_legacy32_impl[128, 8, 0]
comptime gemm_q4_0_f16_bm64 = gemm_legacy32_impl[64, 4, 0]
comptime gemm_q4_1_f16 = gemm_legacy32_impl[128, 8, 1]
comptime gemm_q4_1_f16_bm64 = gemm_legacy32_impl[64, 4, 1]
comptime gemm_q5_0_f16 = gemm_legacy32_impl[128, 8, 2]
comptime gemm_q5_0_f16_bm64 = gemm_legacy32_impl[64, 4, 2]
comptime gemm_q5_1_f16 = gemm_legacy32_impl[128, 8, 3]
comptime gemm_q5_1_f16_bm64 = gemm_legacy32_impl[64, 4, 3]
comptime gemm_iq4_nl_f16 = gemm_iq4_lut_impl[128, 8, 0]
comptime gemm_iq4_nl_f16_bm64 = gemm_iq4_lut_impl[64, 4, 0]
comptime gemm_iq4_xs_f16 = gemm_iq4_lut_impl[128, 8, 1]
comptime gemm_iq4_xs_f16_bm64 = gemm_iq4_lut_impl[64, 4, 1]
comptime gemm_mxfp4_gguf_f16 = gemm_mxfp4_gguf_impl[128, 8]
comptime gemm_mxfp4_gguf_f16_bm64 = gemm_mxfp4_gguf_impl[64, 4]
comptime gemm_iq2_xs_f16 = gemm_iq2_xs_impl[128, 8]
comptime gemm_iq2_xs_f16_bm64 = gemm_iq2_xs_impl[64, 4]
comptime gemm_iq2_s_f16 = gemm_iq2_s_impl[128, 8]
comptime gemm_iq2_s_f16_bm64 = gemm_iq2_s_impl[64, 4]
comptime gemm_iq3_s_f16 = gemm_iq3_s_impl[128, 8]
comptime gemm_iq3_s_f16_bm64 = gemm_iq3_s_impl[64, 4]
comptime gemm_iq2_xxs_f16 = gemm_iq2_xxs_impl[128, 8]
comptime gemm_iq2_xxs_f16_bm64 = gemm_iq2_xxs_impl[64, 4]
comptime gemm_iq3_xxs_f16 = gemm_iq3_xxs_impl[128, 8]
comptime gemm_iq3_xxs_f16_bm64 = gemm_iq3_xxs_impl[64, 4]
comptime gemm_iq1_s_f16 = gemm_iq1_s_impl[128, 8]
comptime gemm_iq1_s_f16_bm64 = gemm_iq1_s_impl[64, 4]
comptime gemm_iq1_m_f16 = gemm_iq1_m_impl[128, 8]
comptime gemm_iq1_m_f16_bm64 = gemm_iq1_m_impl[64, 4]
