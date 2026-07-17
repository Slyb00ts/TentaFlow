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
from src.gemv2 import _e2m1x8, _f8e4m3s, _q4k_scale_min

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
