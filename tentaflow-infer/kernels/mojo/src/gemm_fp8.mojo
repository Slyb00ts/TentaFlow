# ===== File: gemm_fp8.mojo — fp8(e4m3) TENSOR-CORE prefill GEMM: Y[T,N] = X·W^T =====
# Ada (sm_89) 4th-gen fp8 tensor cores via the m16n8k32 e4m3 mma. Weights arrive
# pre-requantized to e4m3 [N,K] with ONE f32 scale per output row; activations
# are quantized per-token to e4m3 (`quantize_act_fp8`) with ONE f32 scale per
# token. Because e4m3 is floating point (4-bit exponent), a single per-row /
# per-token scale absorbs the block-to-block magnitude spread that int8 needs
# per-32-block scales for — so the hot loop accumulates the raw e4m3·e4m3 dot in
# f32 across the WHOLE K reduction and applies `s_a[t]·s_w[r]` once at the
# epilogue (no per-block rescale, unlike the i8mma path). Fragment layout mirrors
# the s8 m16n8k32 kernel (same 8-bit operand packing): A = 4×u32 via ld_matrix[8],
# B = 2×u32 via ld_matrix[4]. Grid (ceil(N/BN), ceil(T/BM)); block NW*32.
#
# The fp8 mma needs PTX ISA .version >= 8.4; Mojo's NVPTX emitter caps at 8.1 for
# sm_89, so build_kernels.mojo bumps the committed .ptx `.version` line to 8.4
# (the shim ptxas_fp8_shim.sh does the same for `mojo run` JIT). Ada fp8 tensor
# cores are hardware-valid at 8.4; this only lifts an emitter version cap.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import bitcast, stack_allocation
from std.gpu.compute.mma import ld_matrix
from std.sys import _RegisterPackType
from std.sys._assembly import inlined_assembly

comptime E4M3_MAX: Float32 = 448.0


def _mma_e4m3(
    a0: UInt32,
    a1: UInt32,
    a2: UInt32,
    a3: UInt32,
    b0: UInt32,
    b1: UInt32,
    c: SIMD[DType.float32, 4],
) -> SIMD[DType.float32, 4]:
    """One m16n8k32 e4m3·e4m3 → f32 tensor-core op: 32 fp8 MACs per lane-group,
    accumulating into the f32 fragment `c`. Requires PTX ISA >= 8.4."""
    var r = inlined_assembly[
        (
            "mma.sync.aligned.m16n8k32.row.col.f32.e4m3.e4m3.f32 {$0, $1, $2,"
            " $3}, {$4, $5, $6, $7}, {$8, $9}, {$10, $11, $12, $13};"
        ),
        _RegisterPackType[Float32, Float32, Float32, Float32],
        constraints="=f,=f,=f,=f,r,r,r,r,r,r,f,f,f,f",
        has_side_effect=False,
    ](a0, a1, a2, a3, b0, b1, c[0], c[1], c[2], c[3])
    return SIMD[DType.float32, 4](r[0], r[1], r[2], r[3])


def quantize_act_fp8(
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_tokens: Int,
):
    """Per-token activation quant: x[T,K] f16 → e4m3 codes [T,K] (`xq`, byte
    view) + one f32 scale per token (`xs`). scale = absmax(row)/448; a token's
    codes are e4m3(x/scale). One block per token; block-wide absmax reduction
    over K. n_cols % 32 == 0."""
    tok = Int(block_idx.x)
    if tok >= n_tokens:
        return
    tid = Int(thread_idx.x)
    nthreads = Int(block_dim.x)
    base = tok * n_cols

    var local: Float32 = 0.0
    var c = tid
    while c < n_cols:
        v = abs(Float32(x[base + c]))
        if v > local:
            local = v
        c += nthreads

    red = stack_allocation[
        1024, Float32, address_space = AddressSpace.SHARED
    ]()
    red[tid] = local
    barrier()
    var stride = nthreads // 2
    while stride > 0:
        if tid < stride:
            if red[tid + stride] > red[tid]:
                red[tid] = red[tid + stride]
        barrier()
        stride //= 2

    var amax = red[0]
    barrier()
    if amax == 0.0:
        if tid == 0:
            xs[tok] = 0.0
        var z = tid
        while z < n_cols:
            xq[base + z] = 0
            z += nthreads
        return

    var scale = amax / E4M3_MAX
    var inv = E4M3_MAX / amax
    if tid == 0:
        xs[tok] = scale
    var q = tid
    while q < n_cols:
        e = Scalar[DType.float8_e4m3fn](Float32(x[base + q]) * inv)
        xq[base + q] = bitcast[DType.int8, 1](e)
        q += nthreads


def gemm_fp8_impl[BM: Int, BN: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[Int8, MutAnyOrigin],
    ws_g: UnsafePointer[Float32, MutAnyOrigin],
    xq_g: UnsafePointer[Int8, MutAnyOrigin],
    xs_g: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """fp8 e4m3 tensor-core GEMM: Y[t, r] = s_a[t]·s_w[r]·Σ_k Xq[t,k]·Wq[r,k].

    `w` is e4m3 [N,K] (byte view), `ws_g` the per-row f32 scale [N]. `xq_g` is
    e4m3 [T,K] (byte view) from `quantize_act_fp8`, `xs_g` the per-token f32
    scale [T]. Grid (ceil(N/BN), ceil(T/BM)); block NW*32. n_cols % 32 == 0.
    """
    comptime MT_PER_WARP = 2
    comptime M_WARPS = BM // 32
    comptime N_WARPS = NW // M_WARPS
    comptime NT_PER_WARP = (BN // 8) // N_WARPS
    comptime NTHREADS = NW * 32
    comptime W_ROWS_PER_PASS = NTHREADS // 4
    comptime W_PASSES = BN // W_ROWS_PER_PASS

    tid = Int(thread_idx.x)
    row0 = Int(block_idx.x) * BN
    t0 = Int(block_idx.y) * BM

    xq = stack_allocation[
        2 * BM * 32, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    wq = stack_allocation[
        2 * BN * 32, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()

    # X staging: one token per thread (tid < BM); NTHREADS >= BM.
    var xtok_c = t0 + tid
    if xtok_c > n_tokens - 1:
        xtok_c = n_tokens - 1

    # W staging: 4 threads per row, 8 e4m3 bytes each; W_PASSES row-passes cover BN.
    row_l = tid // 4
    part = tid % 4
    var wrow_base = InlineArray[Int, W_PASSES](fill=0)
    comptime for p in range(W_PASSES):
        var wrow_c = row0 + p * W_ROWS_PER_PASS + row_l
        if wrow_c > n_rows - 1:
            wrow_c = n_rows - 1
        wrow_base[p] = wrow_c * n_cols

    n_stages = n_cols // 32

    # Warp / lane identity for the mma fragments.
    wid = tid // 32
    lane = tid % 32
    g = lane >> 2
    tt = lane & 3
    sub = lane // 8
    lr = lane % 8
    wid_m = wid % M_WARPS
    wid_n = wid // M_WARPS
    mt0 = wid_m * (MT_PER_WARP * 16)
    nbase = wid_n * NT_PER_WARP

    var acc = InlineArray[SIMD[DType.float32, 4], MT_PER_WARP * NT_PER_WARP](
        fill=SIMD[DType.float32, 4](0.0)
    )

    var xcodes = SIMD[DType.int8, 32](0)
    var wcodes = InlineArray[SIMD[DType.int8, 8], W_PASSES](
        fill=SIMD[DType.int8, 8](0)
    )

    if tid < BM:
        xcodes = (xq_g + xtok_c * n_cols).load[width=32, alignment=32]()
    comptime for p in range(W_PASSES):
        wcodes[p] = (w + wrow_base[p] + part * 8).load[width=8, alignment=8]()

    @parameter
    @always_inline
    def sw(buf: Int):
        if tid < BM:
            (xq + buf * BM * 32 + tid * 32).store[alignment=32](xcodes)
        comptime for p in range(W_PASSES):
            rl = p * W_ROWS_PER_PASS + row_l
            (wq + buf * BN * 32 + rl * 32 + part * 8).store[alignment=8](
                wcodes[p]
            )

    @parameter
    @always_inline
    def gl(sidx: Int):
        if tid < BM:
            xcodes = (xq_g + xtok_c * n_cols + sidx * 32).load[
                width=32, alignment=32
            ]()
        comptime for p in range(W_PASSES):
            wcodes[p] = (w + wrow_base[p] + sidx * 32 + part * 8).load[
                width=8, alignment=8
            ]()

    # Prologue: stage 0 -> buf 0, prefetch stage 1's global reads.
    sw(0)
    if n_stages > 1:
        gl(1)
    barrier()

    var s = 0
    while s < n_stages:
        buf = s % 2
        if s + 1 < n_stages:
            sw((s + 1) % 2)
        if s + 2 < n_stages:
            gl(s + 2)

        Af = (xq + buf * BM * 32 + mt0 * 32).bitcast[Float16]()
        var ai = InlineArray[SIMD[DType.uint32, 4], MT_PER_WARP](
            fill=SIMD[DType.uint32, 4](0)
        )
        comptime for mi in range(MT_PER_WARP):
            a_base = Af + (mi * 16 + (sub % 2) * 8 + lr) * 16 + (sub // 2) * 8
            ai[mi] = bitcast[DType.uint32, 4](ld_matrix[8](a_base))

        comptime for nti in range(NT_PER_WARP):
            nb = (nbase + nti) * 8
            Bf = (wq + buf * BN * 32 + nb * 32).bitcast[Float16]()
            b_base = Bf + lr * 16 + (sub % 2) * 8
            bi = bitcast[DType.uint32, 2](ld_matrix[4](b_base))
            comptime for mi in range(MT_PER_WARP):
                acc[mi * NT_PER_WARP + nti] = _mma_e4m3(
                    ai[mi][0], ai[mi][1], ai[mi][2], ai[mi][3],
                    bi[0], bi[1], acc[mi * NT_PER_WARP + nti],
                )
        barrier()
        s += 1

    comptime for mi in range(MT_PER_WARP):
        tok_a = t0 + mt0 + mi * 16 + g
        tok_b = t0 + mt0 + mi * 16 + g + 8
        var sa_a: Float32 = 0.0
        var sa_b: Float32 = 0.0
        if tok_a < n_tokens:
            sa_a = xs_g[tok_a]
        if tok_b < n_tokens:
            sa_b = xs_g[tok_b]
        comptime for nti in range(NT_PER_WARP):
            nb = (nbase + nti) * 8
            r_a = row0 + nb + 2 * tt
            r_b = row0 + nb + 2 * tt + 1
            var sw_a: Float32 = 0.0
            var sw_b: Float32 = 0.0
            if r_a < n_rows:
                sw_a = ws_g[r_a]
            if r_b < n_rows:
                sw_b = ws_g[r_b]
            d4 = acc[mi * NT_PER_WARP + nti]
            if tok_a < n_tokens:
                if r_a < n_rows:
                    y[tok_a * n_rows + r_a] = Float16(d4[0] * sa_a * sw_a)
                if r_b < n_rows:
                    y[tok_a * n_rows + r_b] = Float16(d4[1] * sa_a * sw_b)
            if tok_b < n_tokens:
                if r_a < n_rows:
                    y[tok_b * n_rows + r_a] = Float16(d4[2] * sa_b * sw_a)
                if r_b < n_rows:
                    y[tok_b * n_rows + r_b] = Float16(d4[3] * sa_b * sw_b)


comptime gemm_fp8_f16 = gemm_fp8_impl[128, 64, 8]
comptime gemm_fp8_f16_bm64 = gemm_fp8_impl[64, 64, 8]
comptime gemm_fp8_f16_big = gemm_fp8_impl[128, 128, 16]
