# ===== File: decode_dp4a.mojo — int8-activation (q8_1) dp4a GEMV variants =====
# Activations are quantized per 32-column segment to int8 (d = amax/127,
# q = round(x/d)) and dotted against the quantized weight codes with dp4a
# (llvm.nvvm.idp4a.s.s) — llama.cpp's mul_mat_vec_q / vec_dot_*_q8_1 scheme.
# The Q4_K min term uses the TRUE per-segment f32 x sums (the same sums the
# f16 kernels staged as xsum), not the quantized sums, so only the q-dependent
# dot changes precision.
#
# Quantization happens per block into shared memory: every warp-per-row GEMV
# block already re-reads the full x vector (L2-resident), so a separate global
# quantize pass would only trade f16 L2 re-reads for int8 ones while forcing
# new activation buffers through the engine. Staging in shared keeps the
# kernel signatures identical to the f16-x variants; the per-segment math is
# deterministic, so every block (and the fused vs plain paths) quantizes x
# identically.
#
# Numerical contract: int8 activation quantization + integer accumulation
# rounds differently than the f16-x kernels — outputs are NOT bit-exact vs
# the *_f16_v2 / decode_fused chain. test_dp4a.mojo bounds the relative error
# against an f64 CPU reference and checks fused-vs-plain consistency WITHIN
# the dp4a path.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.primitives import warp
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import bitcast, stack_allocation
from std.math import exp, rsqrt
from std.sys.intrinsics import llvm_intrinsic
from src.reduce import block_reduce_sum

comptime WARP = 32
comptime ROWS_PER_BLOCK = 8
comptime MAX_HIDDEN = 8192
# Column bound for the kernels that stage x from GLOBAL memory (plain +
# residual variants): 16 KiB int8 + 4 KiB scale/sum pairs of shared memory.
comptime X_MAX = 16384
comptime XDS_MAX = X_MAX // 32 * 2
comptime XDS_HID = MAX_HIDDEN // 32 * 2
comptime XQS_MAX = X_MAX // 16
comptime XQS_HID = MAX_HIDDEN // 16


def _dp4a(a: Int32, b: Int32, c: Int32) -> Int32:
    """c + dot(4 signed int8 lanes of a, 4 signed int8 lanes of b) — one
    dp4a.s32.s32 instruction on sm_61+."""
    return llvm_intrinsic["llvm.nvvm.idp4a.s.s", Int32](a, b, c)


def _quantize_seg(
    xv: SIMD[DType.float32, 32],
) -> Tuple[SIMD[DType.int8, 32], Float32, Float32]:
    """One 32-element q8_1 segment: (int8 codes, dequant scale d, f32 sum).
    d = amax/127, q = round(x/d) — quantize_q8_1 (llama.cpp quantize.cu)."""
    s = xv.reduce_add()
    amax = abs(xv).reduce_max()
    if amax == 0.0:
        return (SIMD[DType.int8, 32](0), Float32(0.0), s)
    q = round(xv * (127.0 / amax)).cast[DType.int8]()
    return (q, amax * (1.0 / 127.0), s)


def _stage_quant_global(
    x: UnsafePointer[Float16, MutAnyOrigin],
    xq: UnsafePointer[
        Int8, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xds: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    tid: Int,
):
    """Quantize global f16 x into shared (xq int8, xds (d, sum) pairs),
    whole block; one 32-column segment per thread iteration."""
    segs = n_cols // 32
    var s_i = tid
    while s_i < segs:
        xv = (x + s_i * 32).load[width=32, alignment=64]().cast[DType.float32]()
        q, d, s = _quantize_seg(xv)
        (xq + s_i * 32).store[alignment=32](q)
        xds[2 * s_i] = d
        xds[2 * s_i + 1] = s
        s_i += 256
    barrier()


def _norm_quant_to_shared(
    xq: UnsafePointer[
        Int8, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xds: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    h: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    norm_w: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    ss_from_h16: Int,
    eps: Float32,
):
    """Recompute rmsnorm(residual) and quantize it straight into shared int8
    (fused chain). The sum-of-squares phase mirrors _norm_x_to_shared
    (decode_fused.mojo) exactly; each normed value is rounded through f16
    before quantization, so the int8 codes are bit-identical to quantizing
    the separate chain's staged f16 x — and no f16 copy is materialized
    (16 KiB of shared saved -> higher occupancy than the f16-x kernels)."""
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
    segs = n_cols // 32
    var s_i = tid
    while s_i < segs:
        hv = (h + s_i * 32).load[width=32, alignment=64]().cast[DType.float32]()
        wv = (norm_w + s_i * 32).load[width=32, alignment=64]().cast[DType.float32]()
        xv = (hv * inv * wv).cast[DType.float16]().cast[DType.float32]()
        q, d, s = _quantize_seg(xv)
        (xq + s_i * 32).store[alignment=32](q)
        xds[2 * s_i] = d
        xds[2 * s_i + 1] = s
        s_i += bdim
    barrier()


def _stage_quant_global_q6(
    x: UnsafePointer[Float16, MutAnyOrigin],
    xq: UnsafePointer[
        Int8, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xds: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xqs: UnsafePointer[
        Int32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    tid: Int,
):
    """_stage_quant_global plus per-16-column int sums of the codes (xqs):
    the Q6_K dot folds its -32 bias as dot(q-32, u) = dot(q, u) - 32*sum(u),
    so the sums are computed once per block instead of per row."""
    segs = n_cols // 32
    var s_i = tid
    while s_i < segs:
        xv = (x + s_i * 32).load[width=32, alignment=64]().cast[DType.float32]()
        q, d, s = _quantize_seg(xv)
        (xq + s_i * 32).store[alignment=32](q)
        xds[2 * s_i] = d
        xds[2 * s_i + 1] = s
        xqs[2 * s_i] = q.slice[16, offset=0]().cast[DType.int32]().reduce_add()
        xqs[2 * s_i + 1] = q.slice[16, offset=16]().cast[DType.int32]().reduce_add()
        s_i += 256
    barrier()


def _norm_quant_to_shared_q6(
    xq: UnsafePointer[
        Int8, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xds: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xqs: UnsafePointer[
        Int32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    h: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    norm_w: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    ss_from_h16: Int,
    eps: Float32,
):
    """_norm_quant_to_shared plus the Q6_K per-16-column code sums (xqs)."""
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
    segs = n_cols // 32
    var s_i = tid
    while s_i < segs:
        hv = (h + s_i * 32).load[width=32, alignment=64]().cast[DType.float32]()
        wv = (norm_w + s_i * 32).load[width=32, alignment=64]().cast[DType.float32]()
        xv = (hv * inv * wv).cast[DType.float16]().cast[DType.float32]()
        q, d, s = _quantize_seg(xv)
        (xq + s_i * 32).store[alignment=32](q)
        xds[2 * s_i] = d
        xds[2 * s_i + 1] = s
        xqs[2 * s_i] = q.slice[16, offset=0]().cast[DType.int32]().reduce_add()
        xqs[2 * s_i + 1] = q.slice[16, offset=16]().cast[DType.int32]().reduce_add()
        s_i += bdim
    barrier()


def _q4k_scales4(
    hdr: SIMD[DType.uint8, 16], hi_half: Int
) -> Tuple[SIMD[DType.float32, 4], SIMD[DType.float32, 4]]:
    """All four (scale, min) pairs of one Q4_K half-superblock at once.
    hi_half selects j 0..3 (0) or j 4..7 (1) of get_scale_min_k4; the
    packed-int32 formulation replaces 4x the byte-wise extraction (same
    bit math as _q4k_scale_min in gemv2.mojo, vectorized)."""
    w = bitcast[DType.uint32, 4](hdr)
    if hi_half == 0:
        return (
            bitcast[DType.uint8, 4](w[1] & 0x3F3F3F3F).cast[DType.float32](),
            bitcast[DType.uint8, 4](w[2] & 0x3F3F3F3F).cast[DType.float32](),
        )
    sc32 = (w[3] & 0x0F0F0F0F) | ((w[1] >> 2) & 0x30303030)
    mn32 = ((w[3] >> 4) & 0x0F0F0F0F) | ((w[2] >> 2) & 0x30303030)
    return (
        bitcast[DType.uint8, 4](sc32).cast[DType.float32](),
        bitcast[DType.uint8, 4](mn32).cast[DType.float32](),
    )


def _dot_q8_0_i8(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xq: UnsafePointer[
        Int8, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xds: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
    # Warp-cooperative Q8_0 row dot against quantized shared x:
    # vec_dot_q8_0_q8_1 (d_w * d_x * dp4a-sum per 32-block).
    blocks_per_row = n_cols // 32
    row_base = row * blocks_per_row * 34
    var acc: Float32 = 0.0
    var b = lane
    while b < blocks_per_row:
        off = row_base + b * 34
        scale = Float32((w + off).bitcast[Float16]()[0])
        v16 = (w + off + 2).bitcast[UInt16]().load[width=16]()
        v = bitcast[DType.int32, 8](v16)
        u = (xq + b * 32).bitcast[Int32]().load[width=8, alignment=32]()
        var sumi: Int32 = 0
        comptime for i in range(8):
            sumi = _dp4a(v[i], u[i], sumi)
        acc += scale * xds[2 * b] * Float32(sumi)
        b += WARP
    return warp.sum(acc)


def _dot2_q8_0_i8(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row_g: Int,
    row_u: Int,
    xq: UnsafePointer[
        Int8, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xds: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> SIMD[DType.float32, 2]:
    # Two-row Q8_0 dp4a dot (gate/up pair): loads of both rows overlap in the
    # memory pipeline, mirroring _dot2_q8_0 (decode_fused.mojo).
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
        vg16 = (w + off_g + 2).bitcast[UInt16]().load[width=16]()
        vu16 = (w + off_u + 2).bitcast[UInt16]().load[width=16]()
        vg = bitcast[DType.int32, 8](vg16)
        vu = bitcast[DType.int32, 8](vu16)
        u = (xq + b * 32).bitcast[Int32]().load[width=8, alignment=32]()
        var sum_g: Int32 = 0
        var sum_u: Int32 = 0
        comptime for i in range(8):
            sum_g = _dp4a(vg[i], u[i], sum_g)
            sum_u = _dp4a(vu[i], u[i], sum_u)
        d8 = xds[2 * b]
        acc_g += scale_g * d8 * Float32(sum_g)
        acc_u += scale_u * d8 * Float32(sum_u)
        b += WARP
    return SIMD[DType.float32, 2](warp.sum(acc_g), warp.sum(acc_u))


def _dot_q4k_i8(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xq: UnsafePointer[
        Int8, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xds: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
    # Warp-cooperative Q4_K row dot: nibble unpack via int32 mask/shift +
    # dp4a per 32-segment (vec_dot_q4_K_q8_1_impl_vmmq); the dmin term uses
    # the true f32 segment sums staged in xds (more precise than llama.cpp's
    # quantized dot2 sum). Loop geometry matches _dot_q4k (decode_fused).
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
        scv, mnv = _q4k_scales4(hdr, c % 2)

        comptime for qq in range(2):
            sc1 = scv[2 * qq]
            sc2 = scv[2 * qq + 1]
            mn1 = mnv[2 * qq]
            mn2 = mnv[2 * qq + 1]
            qv = (w + off + 16 + (q + qq) * 32).load[width=32, alignment=16]()
            v = bitcast[DType.int32, 8](qv)
            # Low nibbles = first 32 columns, high nibbles = next 32; the
            # arithmetic >> sign-fill lands only in bits the mask clears.
            lo = v & 0x0F0F0F0F
            hi = (v >> 4) & 0x0F0F0F0F
            col = (c * 2 + qq) * 64
            u0 = (xq + col).bitcast[Int32]().load[width=8, alignment=32]()
            u1 = (xq + col + 32).bitcast[Int32]().load[width=8, alignment=32]()
            var s0: Int32 = 0
            var s1: Int32 = 0
            comptime for i in range(8):
                s0 = _dp4a(lo[i], u0[i], s0)
                s1 = _dp4a(hi[i], u1[i], s1)
            seg = (c * 2 + qq) * 2
            acc += d * sc1 * xds[2 * seg] * Float32(s0)
            acc += d * sc2 * xds[2 * seg + 2] * Float32(s1)
            min_acc += dmin * (mn1 * xds[2 * seg + 1] + mn2 * xds[2 * seg + 3])
        c += WARP
    return warp.sum(acc - min_acc)


def _dot2_q4k_i8(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row_g: Int,
    row_u: Int,
    xq: UnsafePointer[
        Int8, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xds: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> SIMD[DType.float32, 2]:
    # Two-row Q4_K dp4a dot (gate/up pair), interleaved loads like _dot2_q4k.
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
        scv_g, mnv_g = _q4k_scales4(hdr_g, c % 2)
        scv_u, mnv_u = _q4k_scales4(hdr_u, c % 2)

        comptime for qq in range(2):
            sc1g = scv_g[2 * qq]
            sc2g = scv_g[2 * qq + 1]
            mn1g = mnv_g[2 * qq]
            mn2g = mnv_g[2 * qq + 1]
            sc1u = scv_u[2 * qq]
            sc2u = scv_u[2 * qq + 1]
            mn1u = mnv_u[2 * qq]
            mn2u = mnv_u[2 * qq + 1]
            qv_g = (w + off_g + 16 + (q + qq) * 32).load[width=32, alignment=16]()
            qv_u = (w + off_u + 16 + (q + qq) * 32).load[width=32, alignment=16]()
            v_g = bitcast[DType.int32, 8](qv_g)
            v_u = bitcast[DType.int32, 8](qv_u)
            lo_g = v_g & 0x0F0F0F0F
            hi_g = (v_g >> 4) & 0x0F0F0F0F
            lo_u = v_u & 0x0F0F0F0F
            hi_u = (v_u >> 4) & 0x0F0F0F0F
            col = (c * 2 + qq) * 64
            u0 = (xq + col).bitcast[Int32]().load[width=8, alignment=32]()
            u1 = (xq + col + 32).bitcast[Int32]().load[width=8, alignment=32]()
            var s0g: Int32 = 0
            var s1g: Int32 = 0
            var s0u: Int32 = 0
            var s1u: Int32 = 0
            comptime for i in range(8):
                s0g = _dp4a(lo_g[i], u0[i], s0g)
                s1g = _dp4a(hi_g[i], u1[i], s1g)
                s0u = _dp4a(lo_u[i], u0[i], s0u)
                s1u = _dp4a(hi_u[i], u1[i], s1u)
            seg = (c * 2 + qq) * 2
            d0 = xds[2 * seg]
            d1 = xds[2 * seg + 2]
            acc_g += d_g * sc1g * d0 * Float32(s0g) + d_g * sc2g * d1 * Float32(s1g)
            acc_u += d_u * sc1u * d0 * Float32(s0u) + d_u * sc2u * d1 * Float32(s1u)
            min_g += dmin_g * (mn1g * xds[2 * seg + 1] + mn2g * xds[2 * seg + 3])
            min_u += dmin_u * (mn1u * xds[2 * seg + 1] + mn2u * xds[2 * seg + 3])
        c += WARP
    return SIMD[DType.float32, 2](warp.sum(acc_g - min_g), warp.sum(acc_u - min_u))


def _dot_q6k_i8(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    xq: UnsafePointer[
        Int8, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xds: UnsafePointer[
        Float32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    xqs: UnsafePointer[
        Int32, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    lane: Int,
) -> Float32:
    # Warp-cooperative Q6_K row dot (vec_dot_q6_K_q8_1_impl_mmvq): codes are
    # rebuilt as UNSIGNED 6-bit values in the int32 domain (no per-byte
    # subtraction) and the -32 bias is folded through the staged per-16-column
    # code sums: dot(q-32, u) = dot(q, u) - 32*sum16(u) — exact in integers,
    # so results are identical to biasing the codes.
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
        la = bitcast[DType.int32, 8](ql16a)
        lb = bitcast[DType.int32, 8](ql16b)
        hh = bitcast[DType.int32, 8](qh16)

        v1 = (la & 0x0F0F0F0F) | ((hh & 0x03030303) << 4)
        v2 = (lb & 0x0F0F0F0F) | (((hh >> 2) & 0x03030303) << 4)
        v3 = ((la >> 4) & 0x0F0F0F0F) | (((hh >> 4) & 0x03030303) << 4)
        v4 = ((lb >> 4) & 0x0F0F0F0F) | (((hh >> 6) & 0x03030303) << 4)

        col = c * 128
        u1 = (xq + col).bitcast[Int32]().load[width=8, alignment=32]()
        u2 = (xq + col + 32).bitcast[Int32]().load[width=8, alignment=32]()
        u3 = (xq + col + 64).bitcast[Int32]().load[width=8, alignment=32]()
        u4 = (xq + col + 96).bitcast[Int32]().load[width=8, alignment=32]()

        var s1a: Int32 = 0
        var s1b: Int32 = 0
        var s2a: Int32 = 0
        var s2b: Int32 = 0
        var s3a: Int32 = 0
        var s3b: Int32 = 0
        var s4a: Int32 = 0
        var s4b: Int32 = 0
        comptime for i in range(4):
            s1a = _dp4a(v1[i], u1[i], s1a)
            s2a = _dp4a(v2[i], u2[i], s2a)
            s3a = _dp4a(v3[i], u3[i], s3a)
            s4a = _dp4a(v4[i], u4[i], s4a)
        comptime for i in range(4, 8):
            s1b = _dp4a(v1[i], u1[i], s1b)
            s2b = _dp4a(v2[i], u2[i], s2b)
            s3b = _dp4a(v3[i], u3[i], s3b)
            s4b = _dp4a(v4[i], u4[i], s4b)

        seg = col // 32
        us = col // 16
        var blk: Float32 = 0.0
        blk += xds[2 * seg] * (
            sc[0] * Float32(s1a - 32 * xqs[us])
            + sc[1] * Float32(s1b - 32 * xqs[us + 1])
        )
        blk += xds[2 * (seg + 1)] * (
            sc[2] * Float32(s2a - 32 * xqs[us + 2])
            + sc[3] * Float32(s2b - 32 * xqs[us + 3])
        )
        blk += xds[2 * (seg + 2)] * (
            sc[4] * Float32(s3a - 32 * xqs[us + 4])
            + sc[5] * Float32(s3b - 32 * xqs[us + 5])
        )
        blk += xds[2 * (seg + 3)] * (
            sc[6] * Float32(s4a - 32 * xqs[us + 6])
            + sc[7] * Float32(s4b - 32 * xqs[us + 7])
        )
        acc += d * blk
        c += WARP
    return warp.sum(acc)


# ---------------------------------------------------------------------------
# Plain warp-per-row GEMVs (drop-in signatures of the *_v2 kernels; x is
# quantized block-locally). Grid.x = ceil(n_rows / 8), block = 256,
# n_cols <= X_MAX.
# ---------------------------------------------------------------------------


def gemv_q8_0_dp4a_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    tid = Int(thread_idx.x)
    xq = stack_allocation[
        X_MAX, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XDS_MAX, Float32, address_space = AddressSpace.SHARED
    ]()
    _stage_quant_global(x, xq, xds, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = _dot_q8_0_i8(w, row, xq, xds, n_cols, lane)
    if lane == 0:
        y[row] = Float16(total)


def gemv_q8_0_dp4a_out_f32(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    tid = Int(thread_idx.x)
    xq = stack_allocation[
        X_MAX, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XDS_MAX, Float32, address_space = AddressSpace.SHARED
    ]()
    _stage_quant_global(x, xq, xds, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = _dot_q8_0_i8(w, row, xq, xds, n_cols, lane)
    if lane == 0:
        y[row] = total


def gemv_q4_k_dp4a_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    tid = Int(thread_idx.x)
    xq = stack_allocation[
        X_MAX, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XDS_MAX, Float32, address_space = AddressSpace.SHARED
    ]()
    _stage_quant_global(x, xq, xds, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = _dot_q4k_i8(w, row, xq, xds, n_cols, lane)
    if lane == 0:
        y[row] = Float16(total)


def gemv_q4_k_dp4a_out_f32(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    tid = Int(thread_idx.x)
    xq = stack_allocation[
        X_MAX, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XDS_MAX, Float32, address_space = AddressSpace.SHARED
    ]()
    _stage_quant_global(x, xq, xds, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = _dot_q4k_i8(w, row, xq, xds, n_cols, lane)
    if lane == 0:
        y[row] = total


def gemv_q6_k_dp4a_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    tid = Int(thread_idx.x)
    xq = stack_allocation[
        X_MAX, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XDS_MAX, Float32, address_space = AddressSpace.SHARED
    ]()
    xqs = stack_allocation[
        XQS_MAX, Int32, address_space = AddressSpace.SHARED
    ]()
    _stage_quant_global_q6(x, xq, xds, xqs, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = _dot_q6k_i8(w, row, xq, xds, xqs, n_cols, lane)
    if lane == 0:
        y[row] = Float16(total)


def gemv_q6_k_dp4a_out_f32(
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    tid = Int(thread_idx.x)
    xq = stack_allocation[
        X_MAX, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XDS_MAX, Float32, address_space = AddressSpace.SHARED
    ]()
    xqs = stack_allocation[
        XQS_MAX, Int32, address_space = AddressSpace.SHARED
    ]()
    _stage_quant_global_q6(x, xq, xds, xqs, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = _dot_q6k_i8(w, row, xq, xds, xqs, n_cols, lane)
    if lane == 0:
        y[row] = total


# ---------------------------------------------------------------------------
# rmsnorm-recompute + dp4a GEMV (decode qkv projection). Same launch geometry
# and epilogue as the gemv_norm_* kernels (decode_fused.mojo); the staged
# normed x is additionally quantized to shared int8 before the dots.
# ---------------------------------------------------------------------------


def gemv_norm_q8_0_dp4a_f16(
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
    xq = stack_allocation[
        MAX_HIDDEN, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XDS_HID, Float32, address_space = AddressSpace.SHARED
    ]()
    tid = Int(thread_idx.x)
    _norm_quant_to_shared(xq, xds, h, h32, norm_w, n_cols, ss_from_h16, eps)
    lane = tid % WARP
    wid = tid // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < n_rows:
            total = _dot_q8_0_i8(w, row, xq, xds, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_q4_k_dp4a_f16(
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
    xq = stack_allocation[
        MAX_HIDDEN, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XDS_HID, Float32, address_space = AddressSpace.SHARED
    ]()
    tid = Int(thread_idx.x)
    _norm_quant_to_shared(xq, xds, h, h32, norm_w, n_cols, ss_from_h16, eps)
    lane = tid % WARP
    wid = tid // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < n_rows:
            total = _dot_q4k_i8(w, row, xq, xds, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


def gemv_norm_q6_k_dp4a_f16(
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
    xq = stack_allocation[
        MAX_HIDDEN, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XDS_HID, Float32, address_space = AddressSpace.SHARED
    ]()
    xqs = stack_allocation[
        XQS_HID, Int32, address_space = AddressSpace.SHARED
    ]()
    tid = Int(thread_idx.x)
    _norm_quant_to_shared_q6(xq, xds, xqs, h, h32, norm_w, n_cols, ss_from_h16, eps)
    lane = tid % WARP
    wid = tid // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < n_rows:
            total = _dot_q6k_i8(w, row, xq, xds, xqs, n_cols, lane)
            if lane == 0:
                y[row] = Float16(total)


# ---------------------------------------------------------------------------
# rmsnorm-recompute + fused gate|up dp4a GEMV + SiLU (decode FFN front half).
# Same epilogue rounding as gemv_norm_silu_* (decode_fused.mojo).
# ---------------------------------------------------------------------------


def gemv_norm_silu_q8_0_dp4a_f16(
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
    xq = stack_allocation[
        MAX_HIDDEN, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XDS_HID, Float32, address_space = AddressSpace.SHARED
    ]()
    tid = Int(thread_idx.x)
    _norm_quant_to_shared(xq, xds, h, h32, norm_w, n_cols, 0, eps)
    lane = tid % WARP
    wid = tid // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < inter:
            gu = _dot2_q8_0_i8(w, row, inter + row, xq, xds, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gu[0]))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(gu[1])))


def gemv_norm_silu_q4_k_dp4a_f16(
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
    xq = stack_allocation[
        MAX_HIDDEN, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XDS_HID, Float32, address_space = AddressSpace.SHARED
    ]()
    tid = Int(thread_idx.x)
    _norm_quant_to_shared(xq, xds, h, h32, norm_w, n_cols, 0, eps)
    lane = tid % WARP
    wid = tid // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < inter:
            gu = _dot2_q4k_i8(w, row, inter + row, xq, xds, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gu[0]))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(gu[1])))


def gemv_norm_silu_q6_k_dp4a_f16(
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
    xq = stack_allocation[
        MAX_HIDDEN, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XDS_HID, Float32, address_space = AddressSpace.SHARED
    ]()
    xqs = stack_allocation[
        XQS_HID, Int32, address_space = AddressSpace.SHARED
    ]()
    tid = Int(thread_idx.x)
    _norm_quant_to_shared_q6(xq, xds, xqs, h, h32, norm_w, n_cols, 0, eps)
    lane = tid % WARP
    wid = tid // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < inter:
            gt = _dot_q6k_i8(w, row, xq, xds, xqs, n_cols, lane)
            ut = _dot_q6k_i8(w, inter + row, xq, xds, xqs, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gt))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(ut)))


# ---------------------------------------------------------------------------
# dp4a GEMV + residual add (decode o-projection and down-projection). Same
# (h, h32) epilogue as gemv_residual_* (decode_fused.mojo); x comes from
# global memory and is quantized block-locally. n_cols <= X_MAX.
# ---------------------------------------------------------------------------


def gemv_residual_q8_0_dp4a_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    tid = Int(thread_idx.x)
    xq = stack_allocation[
        X_MAX, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XDS_MAX, Float32, address_space = AddressSpace.SHARED
    ]()
    _stage_quant_global(x, xq, xds, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = _dot_q8_0_i8(w, row, xq, xds, n_cols, lane)
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


def gemv_residual_q4_k_dp4a_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    tid = Int(thread_idx.x)
    xq = stack_allocation[
        X_MAX, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XDS_MAX, Float32, address_space = AddressSpace.SHARED
    ]()
    _stage_quant_global(x, xq, xds, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = _dot_q4k_i8(w, row, xq, xds, n_cols, lane)
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v


def gemv_residual_q6_k_dp4a_f16(
    h_io: UnsafePointer[Float16, MutAnyOrigin],
    h32: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    tid = Int(thread_idx.x)
    xq = stack_allocation[
        X_MAX, Int8, alignment=64, address_space = AddressSpace.SHARED
    ]()
    xds = stack_allocation[
        XDS_MAX, Float32, address_space = AddressSpace.SHARED
    ]()
    xqs = stack_allocation[
        XQS_MAX, Int32, address_space = AddressSpace.SHARED
    ]()
    _stage_quant_global_q6(x, xq, xds, xqs, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = _dot_q6k_i8(w, row, xq, xds, xqs, n_cols, lane)
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v
