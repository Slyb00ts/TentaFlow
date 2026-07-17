# ===== File: gemm.mojo — batched prefill GEMM: Y[T,rows] = X[T,cols] · W^T =====
# Tensor-core decomposition: one block computes a BM(=128 tokens) x BN(=64
# rows) output tile with m16n8k16 f16 mma. X streams k-major through cp.async
# into shared memory (A fragments via transposed ldmatrix); W rows are staged
# n-major (a W row IS the mma B fragment, non-transposed ldmatrix). Both smem
# tiles are double-buffered so the next stage's copies overlap the current
# stage's mma work. Quantized variants dequantize W to f16 in registers on the
# way into smem. Out-of-range tokens/rows are clamped instead of guarded:
# their products land in output positions that are never stored; the k tail
# zero-fills W chunks so clamped X garbage multiplies against zeros.

from std.gpu import block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.memory import (
    AddressSpace,
    async_copy,
    async_copy_commit_group,
    async_copy_wait_group,
)
from std.memory import stack_allocation
from std.gpu.compute.mma import mma, ld_matrix
from src.gemv2 import _e2m1x8, _f8e4m3s

comptime WARP = 32
comptime BM = 128
comptime BN = 64
comptime BK = 32
comptime XPAD = 8
comptime LDX = BM + XPAD  # xs row stride (k-major rows of tokens)
comptime LDW = BK + XPAD  # ws row stride (n-major rows of k)
comptime XTILE = BK * LDX
comptime WTILE = BN * LDW


def transpose_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_tokens: Int,
    n_cols: Int,
):
    """xT[c, t] = x[t, c]. Grid: (ceil(cols/32), ceil(tokens/8)); block 256 =
    32×8 tile; naive but tiny next to the GEMMs it feeds."""
    c = Int(block_idx.x) * 32 + Int(thread_idx.x) % 32
    t = Int(block_idx.y) * 8 + Int(thread_idx.x) // 32
    if c < n_cols and t < n_tokens:
        out_ptr[c * n_tokens + t] = x[t * n_cols + c]


def _issue_x(
    s: Int,
    n_cols: Int,
    n_tokens: Int,
    xk: Int,
    xsrc: UnsafePointer[Float16, MutAnyOrigin, address_space = AddressSpace.GLOBAL],
    xdst0: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xdst1: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
):
    """cp.async stage s's X tile into buffer (s % 2); the k tail clamps rows
    (their garbage meets zeroed W columns, so products vanish)."""
    k0 = s * BK
    buf = s % 2
    if k0 + BK <= n_cols:
        async_copy[16](xsrc + k0 * n_tokens, xdst0 + buf * XTILE)
        async_copy[16](xsrc + (k0 + 16) * n_tokens, xdst1 + buf * XTILE)
    else:
        var kr0 = k0 + xk
        if kr0 > n_cols - 1:
            kr0 = n_cols - 1
        var kr1 = k0 + xk + 16
        if kr1 > n_cols - 1:
            kr1 = n_cols - 1
        async_copy[16](xsrc + (kr0 - xk) * n_tokens, xdst0 + buf * XTILE)
        async_copy[16](xsrc + (kr1 - xk - 16) * n_tokens, xdst1 + buf * XTILE)


def gemm_f16_xt_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[Float16, MutAnyOrigin],
    xt: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """f16 tensor-core GEMM: Y[t, r] = dot(w[r], x[t]).

    Grid: (ceil(rows/64), ceil(T/128)), block 256, n_tokens % 8 == 0,
    n_cols % 8 == 0. W also streams through cp.async (no conversion needed).
    """
    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    # staging indices: 2 X chunks + 1 W chunk of 16 B per thread per stage
    var xc_m = (tid % 16) * 8
    if xc_m > n_tokens - 8 - t0:
        xc_m = n_tokens - 8 - t0
    xk = tid // 16
    var wn = row0 + tid // 4
    if wn > n_rows - 1:
        wn = n_rows - 1
    wk = (tid % 4) * 8

    xsrc = (xt + xk * n_tokens + t0 + xc_m).address_space_cast[AddressSpace.GLOBAL]()
    wsrc = (w + wn * n_cols + wk).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xk * LDX + (tid % 16) * 8
    xdst1 = xdst0 + 16 * LDX
    wdst = ws + (tid // 4) * LDW + wk

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % 4) * 32
    warp_n = (wid // 4) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + ((sub // 2) * 8 + lr) * LDX + warp_m + (sub % 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = (n_cols + BK - 1) // BK

    def issue_w(
        s: Int,
        n_cols: Int,
        wk: Int,
        wsrc: UnsafePointer[
            Float16, MutAnyOrigin, address_space = AddressSpace.GLOBAL
        ],
        wdst: UnsafePointer[
            Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
        ],
    ):
        k0 = s * BK
        if k0 + wk + 8 <= n_cols:
            async_copy[16](wsrc + k0, wdst + (s % 2) * WTILE)
        else:
            (wdst + (s % 2) * WTILE).store[width=8, alignment=16](
                SIMD[DType.float16, 8](0.0)
            )

    _issue_x(0, n_cols, n_tokens, xk, xsrc, xdst0, xdst1)
    issue_w(0, n_cols, wk, wsrc, wdst)
    async_copy_commit_group()

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x(s + 1, n_cols, n_tokens, xk, xsrc, xdst0, xdst1)
            issue_w(s + 1, n_cols, wk, wsrc, wdst)
            async_copy_commit_group()
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8, transpose=True](a_base + buf * XTILE + kb * LDX)
            a1 = ld_matrix[8, transpose=True](a_base + buf * XTILE + kb * LDX + 16)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


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


def gemm_q8_0_xt_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    xt: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Q8_0 tensor-core GEMM. Grid (ceil(rows/64), ceil(T/128)), block 256,
    n_tokens % 8 == 0, n_cols % 32 == 0 (one q8 block per row per stage).
    W is dequantized to f16 in registers (prefetched a stage ahead) and
    staged to smem; the f16-domain multiply keeps the hot loop lean."""
    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    var xc_m = (tid % 16) * 8
    if xc_m > n_tokens - 8 - t0:
        xc_m = n_tokens - 8 - t0
    xk = tid // 16
    xsrc = (xt + xk * n_tokens + t0 + xc_m).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xk * LDX + (tid % 16) * 8
    xdst1 = xdst0 + 16 * LDX

    # W staging: 4 threads per row, 8 quants each; one q8 block per stage.
    var wrow = row0 + tid // 4
    if wrow > n_rows - 1:
        wrow = n_rows - 1
    part = tid % 4
    blocks_per_row = n_cols // 32
    wbase = w + wrow * blocks_per_row * 34
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % 4) * 32
    warp_n = (wid // 4) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + ((sub // 2) * 8 + lr) * LDX + warp_m + (sub % 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = n_cols // BK

    _issue_x(0, n_cols, n_tokens, xk, xsrc, xdst0, xdst1)
    async_copy_commit_group()
    var scale = Float16((wbase).bitcast[Float16]()[0])
    var qv = (wbase + 2 + part * 8).load[width=8, alignment=2]()

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x(s + 1, n_cols, n_tokens, xk, xsrc, xdst0, xdst1)
            async_copy_commit_group()
        wv = qv.cast[DType.int8]().cast[DType.float16]() * scale
        (wdst + (s % 2) * WTILE).store[width=8, alignment=16](wv)
        if s + 1 < n_stages:
            off = (s + 1) * 34
            scale = Float16((wbase + off).bitcast[Float16]()[0])
            qv = (wbase + off + 2 + part * 8).load[width=8, alignment=2]()
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8, transpose=True](a_base + buf * XTILE + kb * LDX)
            a1 = ld_matrix[8, transpose=True](a_base + buf * XTILE + kb * LDX + 16)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)


def gemm_nvfp4_xt_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[UInt8, MutAnyOrigin],
    xt: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    inv_global_scale: Float32,
):
    """NVFP4 tensor-core GEMM. Grid (ceil(rows/64), ceil(T/128)), block 256,
    n_tokens % 8 == 0, n_cols % 16 == 0 (two scale groups per stage; a 16-col
    tail zero-fills the second group)."""
    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space = AddressSpace.SHARED]()

    var xc_m = (tid % 16) * 8
    if xc_m > n_tokens - 8 - t0:
        xc_m = n_tokens - 8 - t0
    xk = tid // 16
    xsrc = (xt + xk * n_tokens + t0 + xc_m).address_space_cast[AddressSpace.GLOBAL]()
    xdst0 = xs + xk * LDX + (tid % 16) * 8
    xdst1 = xdst0 + 16 * LDX

    # W staging: 4 threads per row, 8 values (half a scale group) each.
    var wrow = row0 + tid // 4
    if wrow > n_rows - 1:
        wrow = n_rows - 1
    part = tid % 4
    groups = n_cols // 16
    pbase = packed + wrow * (n_cols // 2) + part * 4
    sbase = scales + wrow * groups + part // 2
    wdst = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    tid4 = lane & 3
    warp_m = (wid % 4) * 32
    warp_n = (wid // 4) * 32
    sub = lane // 8
    lr = lane % 8
    a_base = xs + ((sub // 2) * 8 + lr) * LDX + warp_m + (sub % 2) * 8
    b_base = ws + (warp_n + lr) * LDW + (sub % 2) * 8

    var acc = InlineArray[SIMD[DType.float32, 4], 8](fill=SIMD[DType.float32, 4](0.0))
    n_stages = (n_cols + BK - 1) // BK
    g_mine = part // 2  # this thread's group within the stage (0 or 1)

    def fetch_w(
        s: Int,
        groups: Int,
        g_mine: Int,
        inv_global_scale: Float32,
        pbase: UnsafePointer[UInt8, MutAnyOrigin],
        sbase: UnsafePointer[UInt8, MutAnyOrigin],
        mut qv: SIMD[DType.uint8, 4],
        mut sc: Float32,
    ):
        g_abs = s * 2 + g_mine
        if g_abs < groups:
            qv = (pbase + s * 16).load[width=4, alignment=4]()
            sc = _f8e4m3s(sbase[s * 2]) * inv_global_scale
        else:
            qv = SIMD[DType.uint8, 4](0)
            sc = 0.0

    _issue_x(0, n_cols, n_tokens, xk, xsrc, xdst0, xdst1)
    async_copy_commit_group()
    var qv = SIMD[DType.uint8, 4](0)
    var sc: Float32 = 0.0
    fetch_w(0, groups, g_mine, inv_global_scale, pbase, sbase, qv, sc)

    var s = 0
    while s < n_stages:
        if s + 1 < n_stages:
            _issue_x(s + 1, n_cols, n_tokens, xk, xsrc, xdst0, xdst1)
            async_copy_commit_group()
        lo = qv & 0x0F
        hi = qv >> 4
        codes = lo.interleave(hi)
        wv = (_e2m1x8(codes) * sc).cast[DType.float16]()
        (wdst + (s % 2) * WTILE).store[width=8, alignment=16](wv)
        if s + 1 < n_stages:
            fetch_w(s + 1, groups, g_mine, inv_global_scale, pbase, sbase, qv, sc)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buf = s % 2
        comptime for k16 in range(BK // 16):
            comptime kb = k16 * 16
            a0 = ld_matrix[8, transpose=True](a_base + buf * XTILE + kb * LDX)
            a1 = ld_matrix[8, transpose=True](a_base + buf * XTILE + kb * LDX + 16)
            comptime for ni in range(4):
                b = ld_matrix[4](b_base + buf * WTILE + ni * 8 * LDW + kb)
                mma(acc[ni], a0, b, acc[ni])
                mma(acc[4 + ni], a1, b, acc[4 + ni])
        barrier()
        s += 1

    _store_tile(y, acc, t0, row0, warp_m, warp_n, group, tid4, n_rows, n_tokens)
