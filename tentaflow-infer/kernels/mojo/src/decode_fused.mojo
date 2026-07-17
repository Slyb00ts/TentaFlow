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
from src.gemv2 import (
    _gemv_q4_k_row_acc,
    _gemv_q6_k_row_acc,
    _q4k_fill_xsum,
    _q4k_scale_min,
    Q4K_MAX_SEGS,
    _gemv_q5_k_row_acc,
    _gemv_q3_k_row_acc,
    _gemv_q2_k_row_acc,
    _gemv_q4_0_row_acc,
    _gemv_q4_1_row_acc,
    _gemv_q5_0_row_acc,
    _gemv_q5_1_row_acc,
    _q3k_scales8,
    _q5_high_bits,
    _q2k_fill_xsum16,
    Q2K_MAX_SEGS,
    _init_lut16,
    _e8m0_half,
    _iq4xs_scale,
    _gemv_iq4_nl_row_acc,
    _gemv_mxfp4_row_acc,
    _gemv_iq4_xs_row_acc,
    IQ4NL_VALS,
    MXFP4_VALS,
    _gemv_iq2_xs_row_acc,
    _gemv_iq2_s_row_acc,
    _gemv_iq3_s_row_acc,
    _signs8,
    _gemv_iq2_xxs_row_acc,
    _gemv_iq3_xxs_row_acc,
    _gemv_iq1_s_row_acc,
    _gemv_iq1_m_row_acc,
    IQ1S_DELTA,
)

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


def _q4k_fill_xsum_shared(
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xsum: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    tid: Int,
):
    """Per-32-column x segment sums, staged from the SHARED normed x (same
    loads and reduction as _q4k_fill_xsum over global x, so the sums are
    bit-identical to what the separate-kernel chain computes)."""
    segs = n_cols // 32
    var s = tid
    while s < segs:
        xv = (xs + s * 32).load[width=32, alignment=64]().cast[DType.float32]()
        xsum[s] = xv.reduce_add()
        s += 256
    barrier()


def _dot_q4k(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xsum: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    lut: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
    # Warp-cooperative Q4_K row dot against shared x; identical accumulation
    # order to _gemv_q4_k_row_acc (gemv2.mojo), only the x address space
    # differs.
    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 144
    halves = blocks_per_row * 2

    var acc: Float32 = 0.0
    var min_acc: Float32 = 0.0
    var c = lane
    while c < halves:
        b = c // 2
        q = (c % 2) * 2
        off = row_base + b * 144
        hdr = (w + off).load[width=16, alignment=16]()
        dm = bitcast[DType.float16, 8](hdr)
        d = Float32(dm[0])
        dmin = Float32(dm[1])

        comptime for qq in range(2):
            sc1, mn1 = _q4k_scale_min(hdr, 2 * (q + qq))
            sc2, mn2 = _q4k_scale_min(hdr, 2 * (q + qq) + 1)
            qv = (w + off + 16 + (q + qq) * 32).load[width=32, alignment=16]()
            col = (c * 2 + qq) * 64
            var dot0 = SIMD[DType.float32, 8](0.0)
            var dot1 = SIMD[DType.float32, 8](0.0)
            comptime for k in range(4):
                q8 = qv.slice[8, offset = k * 8]()
                var lov = SIMD[DType.float32, 8]()
                var hiv = SIMD[DType.float32, 8]()
                comptime for j in range(8):
                    lov[j] = lut[Int(q8[j] & 0x0F)]
                    hiv[j] = lut[Int(q8[j] >> 4)]
                x0 = (xs + col + k * 8).load[width=8, alignment=16]().cast[DType.float32]()
                x1 = (xs + col + 32 + k * 8).load[width=8, alignment=16]().cast[DType.float32]()
                dot0 += lov * x0
                dot1 += hiv * x1
            acc += dot0.reduce_add() * (d * sc1) + dot1.reduce_add() * (d * sc2)
            seg = (c * 2 + qq) * 2
            min_acc += dmin * (mn1 * xsum[seg] + mn2 * xsum[seg + 1])
        c += WARP
    return warp.sum(acc - min_acc)


def _dot2_q4k(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row_g: Int,
    row_u: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xsum: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    lut: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> SIMD[DType.float32, 2]:
    # Two-row Q4_K dot with interleaved loads (the gate/up pair of a SwiGLU
    # FFN): per-row accumulation order matches _dot_q4k exactly, but the two
    # rows' header/nibble loads overlap in the memory pipeline instead of
    # running as two sequential latency-bound passes (440 -> ~630 GB/s on the
    # 28672x4096 Mistral gate|up shape at rows_per_warp 7-8, RTX 4090; a
    # 2-warp team split was measured slower — the per-iteration block
    # barriers and doubled norm recompute cost more than the extra
    # parallelism buys).
    blocks_per_row = n_cols // 256
    base_g = row_g * blocks_per_row * 144
    base_u = row_u * blocks_per_row * 144
    halves = blocks_per_row * 2

    var acc_g: Float32 = 0.0
    var min_g: Float32 = 0.0
    var acc_u: Float32 = 0.0
    var min_u: Float32 = 0.0
    var c = lane
    while c < halves:
        b = c // 2
        q = (c % 2) * 2
        off_g = base_g + b * 144
        off_u = base_u + b * 144
        hdr_g = (w + off_g).load[width=16, alignment=16]()
        hdr_u = (w + off_u).load[width=16, alignment=16]()
        dm_g = bitcast[DType.float16, 8](hdr_g)
        dm_u = bitcast[DType.float16, 8](hdr_u)
        d_g = Float32(dm_g[0])
        dmin_g = Float32(dm_g[1])
        d_u = Float32(dm_u[0])
        dmin_u = Float32(dm_u[1])

        comptime for qq in range(2):
            sc1g, mn1g = _q4k_scale_min(hdr_g, 2 * (q + qq))
            sc2g, mn2g = _q4k_scale_min(hdr_g, 2 * (q + qq) + 1)
            sc1u, mn1u = _q4k_scale_min(hdr_u, 2 * (q + qq))
            sc2u, mn2u = _q4k_scale_min(hdr_u, 2 * (q + qq) + 1)
            qv_g = (w + off_g + 16 + (q + qq) * 32).load[width=32, alignment=16]()
            qv_u = (w + off_u + 16 + (q + qq) * 32).load[width=32, alignment=16]()
            col = (c * 2 + qq) * 64
            var dot0g = SIMD[DType.float32, 8](0.0)
            var dot1g = SIMD[DType.float32, 8](0.0)
            var dot0u = SIMD[DType.float32, 8](0.0)
            var dot1u = SIMD[DType.float32, 8](0.0)
            comptime for k in range(4):
                q8g = qv_g.slice[8, offset = k * 8]()
                q8u = qv_u.slice[8, offset = k * 8]()
                var lo_g = SIMD[DType.float32, 8]()
                var hi_g = SIMD[DType.float32, 8]()
                var lo_u = SIMD[DType.float32, 8]()
                var hi_u = SIMD[DType.float32, 8]()
                comptime for j in range(8):
                    lo_g[j] = lut[Int(q8g[j] & 0x0F)]
                    hi_g[j] = lut[Int(q8g[j] >> 4)]
                    lo_u[j] = lut[Int(q8u[j] & 0x0F)]
                    hi_u[j] = lut[Int(q8u[j] >> 4)]
                x0 = (xs + col + k * 8).load[width=8, alignment=16]().cast[DType.float32]()
                x1 = (xs + col + 32 + k * 8).load[width=8, alignment=16]().cast[DType.float32]()
                dot0g += lo_g * x0
                dot1g += hi_g * x1
                dot0u += lo_u * x0
                dot1u += hi_u * x1
            acc_g += dot0g.reduce_add() * (d_g * sc1g) + dot1g.reduce_add() * (d_g * sc2g)
            acc_u += dot0u.reduce_add() * (d_u * sc1u) + dot1u.reduce_add() * (d_u * sc2u)
            seg = (c * 2 + qq) * 2
            min_g += dmin_g * (mn1g * xsum[seg] + mn2g * xsum[seg + 1])
            min_u += dmin_u * (mn1u * xsum[seg] + mn2u * xsum[seg + 1])
        c += WARP
    return SIMD[DType.float32, 2](warp.sum(acc_g - min_g), warp.sum(acc_u - min_u))


def _dot_q6k(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
    # Warp-cooperative Q6_K row dot against shared x; identical accumulation
    # order to _gemv_q6_k_row_acc (gemv2.mojo).
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
        x1 = (xs + col).load[width=32, alignment=64]().cast[DType.float32]()
        x2 = (xs + col + 32).load[width=32, alignment=64]().cast[DType.float32]()
        x3 = (xs + col + 64).load[width=32, alignment=64]().cast[DType.float32]()
        x4 = (xs + col + 96).load[width=32, alignment=64]().cast[DType.float32]()

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
    return warp.sum(acc)


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


def gemv_norm_q4_k_f16(
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
    xsum = stack_allocation[
        Q4K_MAX_SEGS, Float32, address_space = AddressSpace.SHARED
    ]()
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    tid = Int(thread_idx.x)
    if tid < 16:
        lut[tid] = Float32(tid)
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, ss_from_h16, eps)
    barrier()
    _q4k_fill_xsum_shared(xs, xsum, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < n_rows:
            total = _dot_q4k(w, row, xs, xsum, lut, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_q6_k_f16(
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
            total = _dot_q6k(w, row, xs, n_cols, lane)
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


def gemv_norm_silu_q4_k_f16(
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
    xsum = stack_allocation[
        Q4K_MAX_SEGS, Float32, address_space = AddressSpace.SHARED
    ]()
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    tid = Int(thread_idx.x)
    if tid < 16:
        lut[tid] = Float32(tid)
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, 0, eps)
    barrier()
    _q4k_fill_xsum_shared(xs, xsum, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < inter:
            gu = _dot2_q4k(w, row, inter + row, xs, xsum, lut, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gu[0]))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(gu[1])))


def gemv_norm_silu_q6_k_f16(
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
            gt = _dot_q6k(w, row, xs, n_cols, lane)
            ut = _dot_q6k(w, inter + row, xs, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))


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


def gemv_residual_q4_k_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    # x is global (the producing kernel materialized it); the xsum/lut staging
    # runs block-wide BEFORE the row guard so its barrier stays uniform.
    xsum = stack_allocation[
        Q4K_MAX_SEGS, Float32, address_space = AddressSpace.SHARED
    ]()
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    tid = Int(thread_idx.x)
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
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


def gemv_residual_q6_k_f16(
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
    total = warp.sum(_gemv_q6_k_row_acc(w, x, n_cols, row, lane))
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


# ---------------------------------------------------------------------------
# Extended-format dots (Q5_K / Q3_K / Q2_K superblocks, legacy 32-element
# Q4_0 / Q4_1 / Q5_0 / Q5_1) against the SHARED normed x. Accumulation order
# matches the corresponding _gemv_*_row_acc in gemv2.mojo exactly — only the
# x address space differs.
# ---------------------------------------------------------------------------


def _dot_q5k(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xsum: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
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
            x0 = (xs + col).load[width=32, alignment=64]().cast[DType.float32]()
            x1 = (xs + col + 32).load[width=32, alignment=64]().cast[DType.float32]()
            acc += (q0 * x0).reduce_add() * (d * sc1) + (q1 * x1).reduce_add() * (d * sc2)
            seg = (c * 2 + qq) * 2
            min_acc += dmin * (mn1 * xsum[seg] + mn2 * xsum[seg + 1])
        c += WARP
    return warp.sum(acc - min_acc)


def _dot_q3k(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
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
            xv = (xs + col + 32 * s).load[width=32, alignment=64]().cast[DType.float32]()
            p = v * xv
            blk += sc[2 * s] * p.slice[16, offset=0]().reduce_add()
            blk += sc[2 * s + 1] * p.slice[16, offset=16]().reduce_add()
        acc += d * blk
        c += WARP
    return warp.sum(acc)


def _q2k_fill_xsum16_shared(
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xsum: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    tid: Int,
):
    """Per-16-column x segment sums, staged from the SHARED normed x (same
    loads and reduction as _q2k_fill_xsum16 over global x)."""
    segs = n_cols // 16
    var s = tid
    while s < segs:
        xv = (xs + s * 16).load[width=16, alignment=32]().cast[DType.float32]()
        xsum[s] = xv.reduce_add()
        s += 256
    barrier()


def _dot_q2k(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xsum16: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
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
            xv = (xs + col + 32 * s).load[width=32, alignment=64]().cast[DType.float32]()
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
    return warp.sum(acc - min_acc)


def _dot_q4_0(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
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
        x0 = (xs + b * 32).load[width=16, alignment=32]().cast[DType.float32]()
        x1 = (xs + b * 32 + 16).load[width=16, alignment=32]().cast[DType.float32]()
        acc += d * ((lo * x0).reduce_add() + (hi * x1).reduce_add())
        b += WARP
    return warp.sum(acc)


def _dot_q4_1(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
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
        x0 = (xs + b * 32).load[width=16, alignment=32]().cast[DType.float32]()
        x1 = (xs + b * 32 + 16).load[width=16, alignment=32]().cast[DType.float32]()
        acc += d * ((lo * x0).reduce_add() + (hi * x1).reduce_add())
        acc += m * (x0.reduce_add() + x1.reduce_add())
        b += WARP
    return warp.sum(acc)


def _dot_q5_0(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
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
        x0 = (xs + b * 32).load[width=16, alignment=32]().cast[DType.float32]()
        x1 = (xs + b * 32 + 16).load[width=16, alignment=32]().cast[DType.float32]()
        acc += d * ((lo * x0).reduce_add() + (hi * x1).reduce_add())
        b += WARP
    return warp.sum(acc)


def _dot_q5_1(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
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
        x0 = (xs + b * 32).load[width=16, alignment=32]().cast[DType.float32]()
        x1 = (xs + b * 32 + 16).load[width=16, alignment=32]().cast[DType.float32]()
        acc += d * ((lo * x0).reduce_add() + (hi * x1).reduce_add())
        acc += m * (x0.reduce_add() + x1.reduce_add())
        b += WARP
    return warp.sum(acc)


# ---------------------------------------------------------------------------
# rmsnorm-recompute + GEMV, extended formats (same geometry and bit-exactness
# contract as gemv_norm_q6_k_f16; the SwiGLU variants dot gate and up rows
# separately like gemv_norm_silu_q6_k_f16).
# ---------------------------------------------------------------------------


def gemv_norm_q5_k_f16(
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
    xsum = stack_allocation[
        Q4K_MAX_SEGS, Float32, address_space = AddressSpace.SHARED
    ]()
    tid = Int(thread_idx.x)
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, ss_from_h16, eps)
    barrier()
    _q4k_fill_xsum_shared(xs, xsum, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < n_rows:
            total = _dot_q5k(w, row, xs, xsum, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_q3_k_f16(
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
            total = _dot_q3k(w, row, xs, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_q2_k_f16(
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
    xsum = stack_allocation[
        Q2K_MAX_SEGS, Float32, address_space = AddressSpace.SHARED
    ]()
    tid = Int(thread_idx.x)
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, ss_from_h16, eps)
    barrier()
    _q2k_fill_xsum16_shared(xs, xsum, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < n_rows:
            total = _dot_q2k(w, row, xs, xsum, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_q4_0_f16(
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
            total = _dot_q4_0(w, row, xs, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_q4_1_f16(
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
            total = _dot_q4_1(w, row, xs, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_q5_0_f16(
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
            total = _dot_q5_0(w, row, xs, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_q5_1_f16(
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
            total = _dot_q5_1(w, row, xs, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_silu_q5_k_f16(
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
    xsum = stack_allocation[
        Q4K_MAX_SEGS, Float32, address_space = AddressSpace.SHARED
    ]()
    tid = Int(thread_idx.x)
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, 0, eps)
    barrier()
    _q4k_fill_xsum_shared(xs, xsum, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < inter:
            gt = _dot_q5k(w, row, xs, xsum, n_cols, lane)
            ut = _dot_q5k(w, inter + row, xs, xsum, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))


def gemv_norm_silu_q3_k_f16(
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
            gt = _dot_q3k(w, row, xs, n_cols, lane)
            ut = _dot_q3k(w, inter + row, xs, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))


def gemv_norm_silu_q2_k_f16(
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
    xsum = stack_allocation[
        Q2K_MAX_SEGS, Float32, address_space = AddressSpace.SHARED
    ]()
    tid = Int(thread_idx.x)
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, 0, eps)
    barrier()
    _q2k_fill_xsum16_shared(xs, xsum, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < inter:
            gt = _dot_q2k(w, row, xs, xsum, n_cols, lane)
            ut = _dot_q2k(w, inter + row, xs, xsum, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))


def gemv_norm_silu_q4_0_f16(
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
            gt = _dot_q4_0(w, row, xs, n_cols, lane)
            ut = _dot_q4_0(w, inter + row, xs, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))


def gemv_norm_silu_q4_1_f16(
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
            gt = _dot_q4_1(w, row, xs, n_cols, lane)
            ut = _dot_q4_1(w, inter + row, xs, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))


def gemv_norm_silu_q5_0_f16(
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
            gt = _dot_q5_0(w, row, xs, n_cols, lane)
            ut = _dot_q5_0(w, inter + row, xs, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))


def gemv_norm_silu_q5_1_f16(
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
            gt = _dot_q5_1(w, row, xs, n_cols, lane)
            ut = _dot_q5_1(w, inter + row, xs, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))


def gemv_residual_q5_k_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    # x is global (the producing kernel materialized it); the xsum staging
    # runs block-wide BEFORE the row guard so its barrier stays uniform.
    xsum = stack_allocation[
        Q4K_MAX_SEGS, Float32, address_space = AddressSpace.SHARED
    ]()
    tid = Int(thread_idx.x)
    _q4k_fill_xsum(x, xsum, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q5_k_row_acc(w, x, xsum, n_cols, row, lane))
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


def gemv_residual_q3_k_f16(
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
    total = warp.sum(_gemv_q3_k_row_acc(w, x, n_cols, row, lane))
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


def gemv_residual_q2_k_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    xsum = stack_allocation[
        Q2K_MAX_SEGS, Float32, address_space = AddressSpace.SHARED
    ]()
    tid = Int(thread_idx.x)
    _q2k_fill_xsum16(x, xsum, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_q2_k_row_acc(w, x, xsum, n_cols, row, lane))
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


def gemv_residual_q4_0_f16(
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
    total = warp.sum(_gemv_q4_0_row_acc(w, x, n_cols, row, lane))
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


def gemv_residual_q4_1_f16(
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
    total = warp.sum(_gemv_q4_1_row_acc(w, x, n_cols, row, lane))
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


def gemv_residual_q5_0_f16(
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
    total = warp.sum(_gemv_q5_0_row_acc(w, x, n_cols, row, lane))
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


def gemv_residual_q5_1_f16(
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
    total = warp.sum(_gemv_q5_1_row_acc(w, x, n_cols, row, lane))
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


# ---------------------------------------------------------------------------
# Codebook 4-bit formats (IQ4_NL / IQ4_XS) and MXFP4 against the SHARED
# normed x. Accumulation order matches the _gemv_*_row_acc twins exactly.
# ---------------------------------------------------------------------------


def _dot_lut_block32_shared(
    qv: SIMD[DType.uint8, 16],
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    col: Int,
    lut: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
) -> Float32:
    var dot0 = SIMD[DType.float32, 8](0.0)
    var dot1 = SIMD[DType.float32, 8](0.0)
    comptime for k in range(2):
        q8 = qv.slice[8, offset = k * 8]()
        var lov = SIMD[DType.float32, 8]()
        var hiv = SIMD[DType.float32, 8]()
        comptime for j in range(8):
            lov[j] = lut[Int(q8[j] & 0x0F)]
            hiv[j] = lut[Int(q8[j] >> 4)]
        x0 = (xs + col + k * 8).load[width=8, alignment=16]().cast[DType.float32]()
        x1 = (xs + col + 16 + k * 8).load[width=8, alignment=16]().cast[DType.float32]()
        dot0 += lov * x0
        dot1 += hiv * x1
    return dot0.reduce_add() + dot1.reduce_add()


def _dot_iq4_nl(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    lut: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
    blocks_per_row = n_cols // 32
    row_base = row * blocks_per_row * 18
    var acc: Float32 = 0.0
    var b = lane
    while b < blocks_per_row:
        off = row_base + b * 18
        d = Float32((w + off).bitcast[Float16]()[0])
        v16 = (w + off + 2).bitcast[UInt16]().load[width=8]()
        q = bitcast[DType.uint8, 16](v16)
        acc += d * _dot_lut_block32_shared(q, xs, b * 32, lut)
        b += WARP
    return warp.sum(acc)


def _dot_mxfp4(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    lut: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
    blocks_per_row = n_cols // 32
    row_base = row * blocks_per_row * 17
    var acc: Float32 = 0.0
    var b = lane
    while b < blocks_per_row:
        off = row_base + b * 17
        d = _e8m0_half(w[off])
        q = (w + off + 1).load[width=16, alignment=1]()
        acc += d * _dot_lut_block32_shared(q, xs, b * 32, lut)
        b += WARP
    return warp.sum(acc)


def _dot_iq4_xs(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    lut: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
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
            acc += dl * _dot_lut_block32_shared(qv, xs, col + j * 32, lut)
        c += WARP
    return warp.sum(acc)


def gemv_norm_iq4_nl_f16(
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
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_lut16(lut, IQ4NL_VALS)
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, ss_from_h16, eps)
    barrier()
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < n_rows:
            total = _dot_iq4_nl(w, row, xs, lut, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_mxfp4_f16(
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
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_lut16(lut, MXFP4_VALS)
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, ss_from_h16, eps)
    barrier()
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < n_rows:
            total = _dot_mxfp4(w, row, xs, lut, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_iq4_xs_f16(
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
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_lut16(lut, IQ4NL_VALS)
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, ss_from_h16, eps)
    barrier()
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < n_rows:
            total = _dot_iq4_xs(w, row, xs, lut, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_silu_iq4_nl_f16(
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
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_lut16(lut, IQ4NL_VALS)
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, 0, eps)
    barrier()
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < inter:
            gt = _dot_iq4_nl(w, row, xs, lut, n_cols, lane)
            ut = _dot_iq4_nl(w, inter + row, xs, lut, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))


def gemv_norm_silu_mxfp4_f16(
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
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_lut16(lut, MXFP4_VALS)
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, 0, eps)
    barrier()
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < inter:
            gt = _dot_mxfp4(w, row, xs, lut, n_cols, lane)
            ut = _dot_mxfp4(w, inter + row, xs, lut, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))


def gemv_norm_silu_iq4_xs_f16(
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
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_lut16(lut, IQ4NL_VALS)
    _norm_x_to_shared(xs, h, h32, norm_w, n_cols, 0, eps)
    barrier()
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < inter:
            gt = _dot_iq4_xs(w, row, xs, lut, n_cols, lane)
            ut = _dot_iq4_xs(w, inter + row, xs, lut, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))


def gemv_residual_iq4_nl_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_lut16(lut, IQ4NL_VALS)
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq4_nl_row_acc(w, x, lut, n_cols, row, lane))
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


def gemv_residual_mxfp4_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_lut16(lut, MXFP4_VALS)
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_mxfp4_row_acc(w, x, lut, n_cols, row, lane))
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


def gemv_residual_iq4_xs_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    lut = stack_allocation[16, Float32, address_space = AddressSpace.SHARED]()
    _init_lut16(lut, IQ4NL_VALS)
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq4_xs_row_acc(w, x, lut, n_cols, row, lane))
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


# ---------------------------------------------------------------------------
# Codebook 2/3-bit formats (IQ2_XS / IQ2_S / IQ3_S) against the SHARED
# normed x; grid/ksigns tables come in as device pointers (see gemv2.mojo).
# ---------------------------------------------------------------------------


def _dot_iq2_xs(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
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
                xv = (xs + col + j * 32 + l * 8).load[width=8, alignment=16]().cast[
                    DType.float32
                ]()
                db = db0 if l < 2 else db1
                acc += db * (mag * sg * xv).reduce_add()
        c += WARP
    return warp.sum(acc)


def _dot_iq2_s(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
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
                idx = Int(qs[4 * j + l]) | ((Int(qh[j]) << (8 - 2 * l)) & 0x300)
                mag = (grid + idx * 8).load[width=8, alignment=8]().cast[
                    DType.float32
                ]()
                sg = _signs8(sgs[4 * j + l])
                xv = (xs + col + j * 32 + l * 8).load[width=8, alignment=16]().cast[
                    DType.float32
                ]()
                db = db0 if l < 2 else db1
                acc += db * (mag * sg * xv).reduce_add()
        c += WARP
    return warp.sum(acc)


def _dot_iq3_s(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
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
                xv = (xs + col + j * 32 + l * 8).load[width=8, alignment=16]().cast[
                    DType.float32
                ]()
                acc += db * (mag * sg * xv).reduce_add()
        c += WARP
    return warp.sum(acc)


def gemv_norm_iq2_xs_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
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
            total = _dot_iq2_xs(w, grid, ksigns, row, xs, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_iq2_s_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
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
            total = _dot_iq2_s(w, grid, row, xs, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_iq3_s_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
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
            total = _dot_iq3_s(w, grid, row, xs, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_silu_iq2_xs_f16(
    act: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
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
            gt = _dot_iq2_xs(w, grid, ksigns, row, xs, n_cols, lane)
            ut = _dot_iq2_xs(w, grid, ksigns, inter + row, xs, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))


def gemv_norm_silu_iq2_s_f16(
    act: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
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
            gt = _dot_iq2_s(w, grid, row, xs, n_cols, lane)
            ut = _dot_iq2_s(w, grid, inter + row, xs, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))


def gemv_norm_silu_iq3_s_f16(
    act: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
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
            gt = _dot_iq3_s(w, grid, row, xs, n_cols, lane)
            ut = _dot_iq3_s(w, grid, inter + row, xs, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))


def gemv_residual_iq2_xs_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq2_xs_row_acc(w, grid, ksigns, x, n_cols, row, lane))
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


def gemv_residual_iq2_s_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq2_s_row_acc(w, grid, x, n_cols, row, lane))
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


def gemv_residual_iq3_s_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq3_s_row_acc(w, grid, x, n_cols, row, lane))
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


# ---------------------------------------------------------------------------# IQ2_XXS / IQ3_XXS / IQ1_S / IQ1_M against the SHARED normed x; grid and# ksigns tables come in as device pointers (see gemv2.mojo).# ---------------------------------------------------------------------------

def _dot_iq2_xxs(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
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
                xv = (xs + col + j * 32 + l * 8).load[width=8, alignment=16]().cast[
                    DType.float32
                ]()
                acc += db * (mag * sg * xv).reduce_add()
        c += WARP
    return warp.sum(acc)


def _dot_iq3_xxs(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
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
                xv = (xs + col + j * 32 + l * 8).load[width=8, alignment=16]().cast[
                    DType.float32
                ]()
                acc += db * (mag * sg * xv).reduce_add()
        c += WARP
    return warp.sum(acc)


def _dot_iq1_s(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
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
                xv = (xs + col + j * 32 + l * 8).load[width=8, alignment=16]().cast[
                    DType.float32
                ]()
                acc += dl * ((mag + delta) * xv).reduce_add()
        c += WARP
    return warp.sum(acc)


def _dot_iq1_m(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
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
                xv = (xs + col + j * 32 + l * 8).load[width=8, alignment=16]().cast[
                    DType.float32
                ]()
                acc += dl * ((mag + delta) * xv).reduce_add()
        c += WARP
    return warp.sum(acc)

def gemv_norm_iq2_xxs_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
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
            total = _dot_iq2_xxs(w, grid, ksigns, row, xs, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)

def gemv_norm_silu_iq2_xxs_f16(
    act: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
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
            gt = _dot_iq2_xxs(w, grid, ksigns, row, xs, n_cols, lane)
            ut = _dot_iq2_xxs(w, grid, ksigns, inter + row, xs, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))

def gemv_residual_iq2_xxs_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq2_xxs_row_acc(w, grid, ksigns, x, n_cols, row, lane))
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v

def gemv_norm_iq3_xxs_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
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
            total = _dot_iq3_xxs(w, grid, ksigns, row, xs, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)

def gemv_norm_silu_iq3_xxs_f16(
    act: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
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
            gt = _dot_iq3_xxs(w, grid, ksigns, row, xs, n_cols, lane)
            ut = _dot_iq3_xxs(w, grid, ksigns, inter + row, xs, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))

def gemv_residual_iq3_xxs_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    ksigns: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq3_xxs_row_acc(w, grid, ksigns, x, n_cols, row, lane))
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v

def gemv_norm_iq1_s_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
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
            total = _dot_iq1_s(w, grid, row, xs, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)

def gemv_norm_silu_iq1_s_f16(
    act: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
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
            gt = _dot_iq1_s(w, grid, row, xs, n_cols, lane)
            ut = _dot_iq1_s(w, grid, inter + row, xs, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))

def gemv_residual_iq1_s_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq1_s_row_acc(w, grid, x, n_cols, row, lane))
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v

def gemv_norm_iq1_m_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
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
            total = _dot_iq1_m(w, grid, row, xs, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)

def gemv_norm_silu_iq1_m_f16(
    act: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
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
            gt = _dot_iq1_m(w, grid, row, xs, n_cols, lane)
            ut = _dot_iq1_m(w, grid, inter + row, xs, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))

def gemv_residual_iq1_m_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    grid: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    lane = Int(thread_idx.x) % WARP
    wid = Int(thread_idx.x) // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = warp.sum(_gemv_iq1_m_row_acc(w, grid, x, n_cols, row, lane))
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v

