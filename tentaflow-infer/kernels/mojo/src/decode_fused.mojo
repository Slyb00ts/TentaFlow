# ===== File: decode_fused.mojo — fused decode-layer kernels (norm+GEMV, GEMV+residual, GEMV+SwiGLU) =====
# Collapses the decode step's per-layer launch chain. Instead of materializing
# the normed hidden state once per sublayer (rmsnorm_residual -> gemv), every
# GEMV block recomputes the RMSNorm of the residual stream into shared memory
# (hidden <= MAX_HIDDEN, ~a few microseconds of L2-resident reads) and the
# residual add is folded into the consuming GEMV's epilogue. Per layer this
# turns rmsnorm + gemv + silu + gemv + rmsnorm_residual chains into three
# launches: gemv_norm (qkv), gemv_residual (o/down), gemv_norm_silu (gate|up).
#
# Bit-exactness contract (vs the separate-kernel chain, verified in
# test_kernels2.mojo): the separate chain's rmsnorm_residual computes its
# sum-of-squares over the UNROUNDED residual values (f32 h+x) but normalizes
# the ROUNDED f16 residual it stored. To reproduce that dataflow across the
# kernel split, gemv_residual writes both the f16 residual (h) and the
# unrounded f32 copy (h32); the norm-recomputing kernels take the
# sum-of-squares from h32 and the normalized value from h. ss_from_h16=1
# (layer 0, h fresh from the embedding gather where rmsnorm_f16 was the
# reference) sums the f16 values instead. All rounding points (f16 staging of
# the normed x, f16 rounding of GEMV outputs before residual add / SiLU)
# mirror the separate kernels exactly.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.primitives import warp
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import bitcast, stack_allocation
from std.math import rsqrt, exp
from src.reduce import block_reduce_sum

comptime WARP = 32
comptime ROWS_PER_BLOCK = 8
comptime MAX_HIDDEN = 8192


def _norm_x_to_shared(
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    h: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    norm_w: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    ss_from_h16: Int,
    eps: Float32,
):
    # Recompute rmsnorm(residual) into shared memory with the exact loop
    # geometry of rmsnorm_f16 / rmsnorm_residual_f16 (256-thread stride, f32
    # accumulation, block_reduce_sum), so the staged x is bit-identical to
    # what the separate norm kernel would have written to global memory.
    tid = Int(thread_idx.x)
    bdim = Int(block_dim.x)
    var ss: Float32 = 0.0
    var i = tid
    while i < n_cols:
        var v: Float32 = 0.0
        if ss_from_h16 == 1:
            v = Float32(h[i])
        else:
            v = h32[i]
        ss += v * v
        i += bdim
    total = block_reduce_sum(ss)
    inv = rsqrt(total / Float32(n_cols) + eps)
    i = tid
    while i < n_cols:
        xs[i] = Float16(Float32(h[i]) * inv * Float32(norm_w[i]))
        i += bdim


def _dot_q8_0(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
    # Warp-cooperative Q8_0 row dot against shared x; identical accumulation
    # order to gemv_q8_0_f16_v2 (lane strides quant blocks, scalar f32 acc).
    blocks_per_row = n_cols // 32
    row_base = row * blocks_per_row * 34
    var acc: Float32 = 0.0
    var b = lane
    while b < blocks_per_row:
        off = row_base + b * 34
        scale = Float32((w + off).bitcast[Float16]()[0])
        v16 = (w + off + 2).bitcast[UInt16]().load[width=16]()
        q = bitcast[DType.int8, 32](v16).cast[DType.float32]()
        xv = (xs + b * 32).load[width=32, alignment=64]().cast[DType.float32]()
        acc += scale * (q * xv).reduce_add()
        b += WARP
    return warp.sum(acc)


def _dot_f16(
    w: UnsafePointer[Float16, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
    base = row * n_cols
    var acc: Float32 = 0.0
    var i = lane * 8
    stride = WARP * 8
    while i + 8 <= n_cols:
        wv = (w + base + i).load[width=8, alignment=16]().cast[DType.float32]()
        xv = (xs + i).load[width=8, alignment=16]().cast[DType.float32]()
        acc += (wv * xv).reduce_add()
        i += stride
    return warp.sum(acc)


def _dot2_q8_0(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row_g: Int,
    row_u: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> SIMD[DType.float32, 2]:
    # Two-row Q8_0 dot with interleaved loads (the gate/up pair of a SwiGLU
    # FFN): per-row accumulation order matches _dot_q8_0 exactly, but the two
    # rows' loads overlap in the memory pipeline instead of running as two
    # sequential latency-bound passes.
    blocks_per_row = n_cols // 32
    base_g = row_g * blocks_per_row * 34
    base_u = row_u * blocks_per_row * 34
    var acc_g: Float32 = 0.0
    var acc_u: Float32 = 0.0
    var b = lane
    while b < blocks_per_row:
        off_g = base_g + b * 34
        off_u = base_u + b * 34
        scale_g = Float32((w + off_g).bitcast[Float16]()[0])
        scale_u = Float32((w + off_u).bitcast[Float16]()[0])
        vg = (w + off_g + 2).bitcast[UInt16]().load[width=16]()
        vu = (w + off_u + 2).bitcast[UInt16]().load[width=16]()
        xv = (xs + b * 32).load[width=32, alignment=64]().cast[DType.float32]()
        qg = bitcast[DType.int8, 32](vg).cast[DType.float32]()
        qu = bitcast[DType.int8, 32](vu).cast[DType.float32]()
        acc_g += scale_g * (qg * xv).reduce_add()
        acc_u += scale_u * (qu * xv).reduce_add()
        b += WARP
    return SIMD[DType.float32, 2](warp.sum(acc_g), warp.sum(acc_u))


def _dot2_f16(
    w: UnsafePointer[Float16, MutAnyOrigin],
    row_g: Int,
    row_u: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> SIMD[DType.float32, 2]:
    base_g = row_g * n_cols
    base_u = row_u * n_cols
    var acc_g: Float32 = 0.0
    var acc_u: Float32 = 0.0
    var i = lane * 8
    stride = WARP * 8
    while i + 8 <= n_cols:
        wg = (w + base_g + i).load[width=8, alignment=16]().cast[DType.float32]()
        wu = (w + base_u + i).load[width=8, alignment=16]().cast[DType.float32]()
        xv = (xs + i).load[width=8, alignment=16]().cast[DType.float32]()
        acc_g += (wg * xv).reduce_add()
        acc_u += (wu * xv).reduce_add()
        i += stride
    return SIMD[DType.float32, 2](warp.sum(acc_g), warp.sum(acc_u))


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


def _dot_nvfp4(
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    lut: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
    inv_global_scale: Float32,
) -> Float32:
    groups = n_cols // 16
    packed_row = row * (n_cols // 2)
    scales_row = row * groups
    var acc: Float32 = 0.0
    var g = lane
    while g < groups:
        s = _f8e4m3s(scales[scales_row + g]) * inv_global_scale
        qv = (packed + packed_row + g * 8).load[width=8, alignment=8]()
        xv = (xs + g * 16).load[width=16, alignment=32]().cast[DType.float32]()
        x_even, x_odd = xv.deinterleave()
        var lov = SIMD[DType.float32, 8]()
        var hiv = SIMD[DType.float32, 8]()
        comptime for j in range(8):
            lov[j] = lut[Int(qv[j] & 0x0F)]
            hiv[j] = lut[Int(qv[j] >> 4)]
        acc += s * ((lov * x_even).reduce_add() + (hiv * x_odd).reduce_add())
        g += WARP
    return warp.sum(acc)


def _dot2_nvfp4(
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[UInt8, MutAnyOrigin],
    row_g: Int,
    row_u: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    lut: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
    inv_global_scale: Float32,
) -> SIMD[DType.float32, 2]:
    groups = n_cols // 16
    packed_g = row_g * (n_cols // 2)
    packed_u = row_u * (n_cols // 2)
    scales_g = row_g * groups
    scales_u = row_u * groups
    var acc_g: Float32 = 0.0
    var acc_u: Float32 = 0.0
    var g = lane
    while g < groups:
        sg = _f8e4m3s(scales[scales_g + g]) * inv_global_scale
        su = _f8e4m3s(scales[scales_u + g]) * inv_global_scale
        qg = (packed + packed_g + g * 8).load[width=8, alignment=8]()
        qu = (packed + packed_u + g * 8).load[width=8, alignment=8]()
        xv = (xs + g * 16).load[width=16, alignment=32]().cast[DType.float32]()
        x_even, x_odd = xv.deinterleave()
        var lo_g = SIMD[DType.float32, 8]()
        var hi_g = SIMD[DType.float32, 8]()
        var lo_u = SIMD[DType.float32, 8]()
        var hi_u = SIMD[DType.float32, 8]()
        comptime for j in range(8):
            lo_g[j] = lut[Int(qg[j] & 0x0F)]
            hi_g[j] = lut[Int(qg[j] >> 4)]
            lo_u[j] = lut[Int(qu[j] & 0x0F)]
            hi_u[j] = lut[Int(qu[j] >> 4)]
        acc_g += sg * ((lo_g * x_even).reduce_add() + (hi_g * x_odd).reduce_add())
        acc_u += su * ((lo_u * x_even).reduce_add() + (hi_u * x_odd).reduce_add())
        g += WARP
    return SIMD[DType.float32, 2](warp.sum(acc_g), warp.sum(acc_u))


def _init_nvfp4_lut(
    lut: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
):
    comptime e2m1_vals = SIMD[DType.float32, 16](
        0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0,
        -0.0, -0.5, -1.0, -1.5, -2.0, -3.0, -4.0, -6.0,
    )
    tid = Int(thread_idx.x)
    if tid < 16:
        lut[tid] = e2m1_vals[tid]


# ---------------------------------------------------------------------------
# rmsnorm-recompute + GEMV (decode qkv projection)
# Grid.x = ceil(n_rows / (8 * rows_per_warp)), block = 256. All threads stage
# x; row guards apply only to the GEMV epilogue, so the block-wide barriers
# stay uniform. rows_per_warp > 1 amortizes the per-block norm recompute over
# more output rows (fewer blocks -> less redundant h32/h/norm_w traffic) for
# tall projections; per-row math is unchanged, so it stays bit-exact.
# ---------------------------------------------------------------------------


def gemv_norm_q8_0_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    h: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    norm_w: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    ss_from_h16: Int,
    eps: Float32,
    rows_per_warp: Int,
):
    xs = stack_allocation[
        MAX_HIDDEN, Float16, alignment=64, address_space = AddressSpace.SHARED
    ]()
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, ss_from_h16, eps)
    barrier()
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < n_rows:
            total = _dot_q8_0(w, row, xs, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_nvfp4_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[UInt8, MutAnyOrigin],
    h: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    norm_w: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    inv_global_scale: Float32,
    ss_from_h16: Int,
    eps: Float32,
    rows_per_warp: Int,
):
    xs = stack_allocation[
        MAX_HIDDEN, Float16, alignment=64, address_space = AddressSpace.SHARED
    ]()
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_nvfp4_lut(lut)
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, ss_from_h16, eps)
    barrier()
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < n_rows:
            total = _dot_nvfp4(packed, scales, row, xs, lut, n_cols, lane, inv_global_scale)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[Float16, MutAnyOrigin],
    h: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    norm_w: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    ss_from_h16: Int,
    eps: Float32,
    rows_per_warp: Int,
):
    xs = stack_allocation[
        MAX_HIDDEN, Float16, alignment=64, address_space = AddressSpace.SHARED
    ]()
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, ss_from_h16, eps)
    barrier()
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < n_rows:
            total = _dot_f16(w, row, xs, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


# ---------------------------------------------------------------------------
# rmsnorm-recompute + fused gate|up GEMV + SiLU (decode FFN front half)
# One warp owns act row r: it dots weight rows r (gate) and inter + r (up)
# against the staged x, then applies silu(gate) * up with the same f16
# rounding points as the separate gemv + silu_mul_f16 chain.
# Grid.x = ceil(inter / (8 * rows_per_warp)), block = 256.
# ---------------------------------------------------------------------------


def gemv_norm_silu_q8_0_f16(
    act: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    h: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    norm_w: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    inter: Int,
    eps: Float32,
    rows_per_warp: Int,
):
    xs = stack_allocation[
        MAX_HIDDEN, Float16, alignment=64, address_space = AddressSpace.SHARED
    ]()
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, 0, eps)
    barrier()
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < inter:
            gu = _dot2_q8_0(w, row, inter + row, xs, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gu[0]))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(gu[1])))


def gemv_norm_silu_nvfp4_f16(
    act: UnsafePointer[Float16, MutAnyOrigin],
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[UInt8, MutAnyOrigin],
    h: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    norm_w: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    inter: Int,
    inv_global_scale: Float32,
    eps: Float32,
    rows_per_warp: Int,
):
    xs = stack_allocation[
        MAX_HIDDEN, Float16, alignment=64, address_space = AddressSpace.SHARED
    ]()
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_nvfp4_lut(lut)
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, 0, eps)
    barrier()
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < inter:
            gu = _dot2_nvfp4(
                packed, scales, row, inter + row, xs, lut, n_cols, lane, inv_global_scale
            )
            if lane == 0:
                g = Float32(Float16(gu[0]))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(gu[1])))


def gemv_norm_silu_f16(
    act: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[Float16, MutAnyOrigin],
    h: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    norm_w: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    inter: Int,
    eps: Float32,
    rows_per_warp: Int,
):
    xs = stack_allocation[
        MAX_HIDDEN, Float16, alignment=64, address_space = AddressSpace.SHARED
    ]()
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, 0, eps)
    barrier()
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < inter:
            gu = _dot2_f16(w, row, inter + row, xs, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gu[0]))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(gu[1])))


# ---------------------------------------------------------------------------
# GEMV + residual add (decode o-projection and down-projection)
# h_io[row] += f16(dot) with rmsnorm_residual_f16's rounding: the f16
# residual stream continues in h_io while the unrounded f32 sum lands in h32
# for the next norm recompute. x is read from global memory (the producing
# kernel already materialized it). Grid.x = ceil(n_rows / 8), block = 256.
# ---------------------------------------------------------------------------


def gemv_residual_q8_0_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
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
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


def gemv_residual_nvfp4_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    inv_global_scale: Float32,
):
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_nvfp4_lut(lut)
    barrier()
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
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
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


def gemv_residual_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
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
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


# ---------------------------------------------------------------------------
# Final output norm from the (h f16, h32 f32) residual pair — the decode
# graph's last norm before the logit GEMV. Same split dataflow as above:
# sum-of-squares over h32, normalized value from h. One block per row.
# ---------------------------------------------------------------------------


def rmsnorm_h32_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    h: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    weight: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    eps: Float32,
):
    row = Int(block_idx.x)
    base = row * n_cols
    var ss: Float32 = 0.0
    var i = Int(thread_idx.x)
    while i < n_cols:
        v = h32[base + i]
        ss += v * v
        i += Int(block_dim.x)
    total = block_reduce_sum(ss)
    inv = rsqrt(total / Float32(n_cols) + eps)
    i = Int(thread_idx.x)
    while i < n_cols:
        out_ptr[base + i] = Float16(Float32(h[base + i]) * inv * Float32(weight[i]))
        i += Int(block_dim.x)
