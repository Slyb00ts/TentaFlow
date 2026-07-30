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
from src.arch_dot import dot4_i8
from src.reduce import block_reduce_sum

comptime WARP = 32
comptime ROWS_PER_BLOCK = 8
comptime MAX_HIDDEN = 8192
# Column bound for the kernels that stage x from GLOBAL memory (plain +
# residual variants): 16 KiB int8 + 4 KiB scale/sum pairs of shared memory.
# Zbiór roboczy aktywacji w LDS. NIE podnosić, żeby objąć `ffn_down` (17408
# kolumn): zmierzone 29,2 -> 28,1 tok/s na Qwen3.6-27B Q4_K_M. Większy bufor
# zabiera zajętość WSZYSTKIM kernelom dp4a, a `ffn_down` na ścieżce dp4a zyskuje
# przy tym 0,1 tok/s — bilans wychodzi mocno na minus.
comptime X_MAX = 16384
comptime XDS_MAX = X_MAX // 32 * 2
comptime XDS_HID = MAX_HIDDEN // 32 * 2


# Iloczyn int8 ma jedną implementację w `src/arch_dot.mojo` (dp4a na NVIDII,
# v_dot4_i32_i8 na AMD). Alias utrzymuje dotychczasową nazwę w 31 miejscach
# wywołań tej rodziny kerneli.
comptime _dp4a = dot4_i8


def _prefetch_l2(p: UnsafePointer[UInt8, MutAnyOrigin]):
    """Touch the cache line at `p` (prefetch into L2, no register result).
    Issued for the first weight tiles BEFORE the x-staging barrier so DRAM
    streaming overlaps the staging phase instead of idling behind it."""
    llvm_intrinsic["llvm.prefetch.p0", NoneType](
        p, Int32(0), Int32(2), Int32(1)
    )


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
    # 4x unroll with a single accumulator chain: the FADD sequence (and thus
    # the bit-exact total) is identical to the scalar loop, but the four
    # independent loads issue together instead of serializing on L2 latency.
    while i + 3 * bdim < n_cols:
        var v0: Float32
        var v1: Float32
        var v2: Float32
        var v3: Float32
        if ss_from_h16 == 1:
            v0 = Float32(h[i])
            v1 = Float32(h[i + bdim])
            v2 = Float32(h[i + 2 * bdim])
            v3 = Float32(h[i + 3 * bdim])
        else:
            v0 = h32[i]
            v1 = h32[i + bdim]
            v2 = h32[i + 2 * bdim]
            v3 = h32[i + 3 * bdim]
        ss += v0 * v0
        ss += v1 * v1
        ss += v2 * v2
        ss += v3 * v3
        i += 4 * bdim
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


def _q4k_scale_pair(
    w: UnsafePointer[UInt8, MutAnyOrigin], off: Int, j: Int
) -> Tuple[SIMD[DType.float32, 2], SIMD[DType.float32, 2]]:
    """(scale, min) pairs j*2 and j*2+1 of one Q4_K superblock (j = 0..3).
    16-bit formulation of get_scale_min_k4 over the packed scales bytes
    (llama.cpp vec_dot_q4_K_q8_1's aux extraction): only the two uint16
    words a lane actually needs are read from the header."""
    s16 = (w + off + 4).bitcast[UInt16]()
    var aux0: UInt32
    var aux1: UInt32
    if j < 2:
        aux0 = UInt32(s16[j]) & 0x3F3F
        aux1 = UInt32(s16[j + 2]) & 0x3F3F
    else:
        aux0 = (UInt32(s16[j + 2]) & 0x0F0F) | ((UInt32(s16[j - 2]) & 0xC0C0) >> 2)
        aux1 = ((UInt32(s16[j + 2]) >> 4) & 0x0F0F) | ((UInt32(s16[j]) & 0xC0C0) >> 2)
    sc = SIMD[DType.float32, 2](Float32(aux0 & 0xFF), Float32(aux0 >> 8))
    mn = SIMD[DType.float32, 2](Float32(aux1 & 0xFF), Float32(aux1 >> 8))
    return (sc, mn)


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
    # Warp-cooperative Q4_K row dot in llama.cpp's mmvq decomposition
    # (vec_dot_q4_K_q8_1, VDR=2): 16 lanes share one superblock, each lane
    # owns two int32 quant words 16 bytes apart, so a warp's weight loads are
    # consecutive 4-byte words instead of 72-byte-strided vector loads. The
    # dmin term uses the true f32 segment sums staged in xds (more precise
    # than llama.cpp's quantized dot2 sum); lane w4==0 adds it once per
    # segment pair.
    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 144
    i = lane % 16
    j = i // 4  # scale/chunk pair index; lo/hi segments are 2j / 2j+1
    w4 = i % 4
    xqi = xq.bitcast[Int32]()

    var acc: Float32 = 0.0
    var min_acc: Float32 = 0.0
    var b = lane // 16
    while b < blocks_per_row:
        off = row_base + b * 144
        dm = (w + off).bitcast[Float16]().load[width=2, alignment=4]()
        d = Float32(dm[0])
        dmin = Float32(dm[1])
        sc, mn = _q4k_scale_pair(w, off, j)
        qp = (w + off + 16 + j * 32 + 4 * w4).bitcast[Int32]()
        v0 = qp[0]
        v1 = qp[4]
        # Low nibbles = segment 2j, high nibbles = segment 2j+1 of the
        # 32-byte chunk; each lane dots 2x8 int8 columns of each.
        lo0 = v0 & 0x0F0F0F0F
        lo1 = v1 & 0x0F0F0F0F
        hi0 = (v0 >> 4) & 0x0F0F0F0F
        hi1 = (v1 >> 4) & 0x0F0F0F0F
        seg = b * 8 + 2 * j
        u0a = xqi[seg * 8 + w4]
        u0b = xqi[seg * 8 + w4 + 4]
        u1a = xqi[seg * 8 + 8 + w4]
        u1b = xqi[seg * 8 + 12 + w4]
        s_lo = _dp4a(lo1, u0b, _dp4a(lo0, u0a, 0))
        s_hi = _dp4a(hi1, u1b, _dp4a(hi0, u1a, 0))
        acc += d * sc[0] * xds[2 * seg] * Float32(s_lo)
        acc += d * sc[1] * xds[2 * seg + 2] * Float32(s_hi)
        if w4 == 0:
            min_acc += dmin * (mn[0] * xds[2 * seg + 1] + mn[1] * xds[2 * seg + 3])
        b += 2
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
    # Two-row Q4_K mmvq dot (gate/up pair): per-row math and accumulation
    # order identical to _dot_q4k_i8, u loads shared and the two rows' weight
    # loads overlapped in the memory pipeline.
    blocks_per_row = n_cols // 256
    base_g = row_g * blocks_per_row * 144
    base_u = row_u * blocks_per_row * 144
    i = lane % 16
    j = i // 4
    w4 = i % 4
    xqi = xq.bitcast[Int32]()

    var acc_g: Float32 = 0.0
    var min_g: Float32 = 0.0
    var acc_u: Float32 = 0.0
    var min_u: Float32 = 0.0
    var b = lane // 16
    while b < blocks_per_row:
        off_g = base_g + b * 144
        off_u = base_u + b * 144
        dm_g = (w + off_g).bitcast[Float16]().load[width=2, alignment=4]()
        dm_u = (w + off_u).bitcast[Float16]().load[width=2, alignment=4]()
        sc_g, mn_g = _q4k_scale_pair(w, off_g, j)
        sc_u, mn_u = _q4k_scale_pair(w, off_u, j)
        qp_g = (w + off_g + 16 + j * 32 + 4 * w4).bitcast[Int32]()
        qp_u = (w + off_u + 16 + j * 32 + 4 * w4).bitcast[Int32]()
        v0g = qp_g[0]
        v1g = qp_g[4]
        v0u = qp_u[0]
        v1u = qp_u[4]
        seg = b * 8 + 2 * j
        u0a = xqi[seg * 8 + w4]
        u0b = xqi[seg * 8 + w4 + 4]
        u1a = xqi[seg * 8 + 8 + w4]
        u1b = xqi[seg * 8 + 12 + w4]
        s_lo_g = _dp4a(v1g & 0x0F0F0F0F, u0b, _dp4a(v0g & 0x0F0F0F0F, u0a, 0))
        s_hi_g = _dp4a((v1g >> 4) & 0x0F0F0F0F, u1b, _dp4a((v0g >> 4) & 0x0F0F0F0F, u1a, 0))
        s_lo_u = _dp4a(v1u & 0x0F0F0F0F, u0b, _dp4a(v0u & 0x0F0F0F0F, u0a, 0))
        s_hi_u = _dp4a((v1u >> 4) & 0x0F0F0F0F, u1b, _dp4a((v0u >> 4) & 0x0F0F0F0F, u1a, 0))
        acc_g += Float32(dm_g[0]) * sc_g[0] * xds[2 * seg] * Float32(s_lo_g)
        acc_g += Float32(dm_g[0]) * sc_g[1] * xds[2 * seg + 2] * Float32(s_hi_g)
        acc_u += Float32(dm_u[0]) * sc_u[0] * xds[2 * seg] * Float32(s_lo_u)
        acc_u += Float32(dm_u[0]) * sc_u[1] * xds[2 * seg + 2] * Float32(s_hi_u)
        if w4 == 0:
            min_g += Float32(dm_g[1]) * (
                mn_g[0] * xds[2 * seg + 1] + mn_g[1] * xds[2 * seg + 3]
            )
            min_u += Float32(dm_u[1]) * (
                mn_u[0] * xds[2 * seg + 1] + mn_u[1] * xds[2 * seg + 3]
            )
        b += 2
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
    n_cols: Int,
    lane: Int,
) -> Float32:
    # Warp-cooperative Q6_K row dot in llama.cpp's mmvq decomposition
    # (vec_dot_q6_K_q8_1, VDR=1): all 32 lanes share one superblock and each
    # lane owns int32 word `lane` of ql — a warp's ql loads are one fully
    # coalesced 128-byte line. Codes are rebuilt as UNSIGNED 6-bit values and
    # the -32 bias is folded per lane as dot(q-32, u) = dot(q, u) - 32*sum(u)
    # with sum(u) = dp4a(0x01010101, u) — exact in integers, so results are
    # identical to biasing the codes. The Q6_K byte layout is only 2-aligned
    # (210-byte superblocks), hence the uint16 pair loads.
    blocks_per_row = n_cols // 256
    row_base = row * blocks_per_row * 210
    qh_w = 8 * (lane // 16) + lane % 8
    qh_shift = 2 * ((lane % 16) // 8)
    bq8 = 4 * (lane // 16) + (lane % 16) // 8
    sc_off = 8 * (lane // 16) + (lane % 16) // 4
    u_w = lane % 8
    xqi = xq.bitcast[Int32]()

    var acc: Float32 = 0.0
    var b = 0
    while b < blocks_per_row:
        off = row_base + b * 210
        d = Float32((w + off + 208).bitcast[Float16]()[0])
        vl = bitcast[DType.int32, 1](
            (w + off + 4 * lane).bitcast[UInt16]().load[width=2]()
        )[0]
        vh = (
            bitcast[DType.int32, 1](
                (w + off + 128 + 4 * qh_w).bitcast[UInt16]().load[width=2]()
            )[0]
            >> Int32(qh_shift)
        )
        sp = (w + off + 192 + sc_off).bitcast[Int8]()
        sc0 = Float32(Int(sp[0]))
        sc1 = Float32(Int(sp[4]))

        var blk: Float32 = 0.0
        comptime for i in range(2):
            vil = (vl >> Int32(4 * i)) & 0x0F0F0F0F
            vih = ((vh >> Int32(4 * i)) << 4) & 0x30303030
            q = vil | vih
            seg = b * 8 + bq8 + 2 * i
            u = xqi[seg * 8 + u_w]
            s = _dp4a(q, u, 0)
            su = _dp4a(0x01010101, u, 0)
            if i == 0:
                blk += xds[2 * seg] * sc0 * Float32(s - 32 * su)
            else:
                blk += xds[2 * seg] * sc1 * Float32(s - 32 * su)
        acc += d * blk
        b += 1
    return warp.sum(acc)


def _dot2_q6k_i8(
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
    # Two-row Q6_K mmvq dot (gate/up pair): per-row math and accumulation
    # order identical to _dot_q6k_i8, u loads shared.
    blocks_per_row = n_cols // 256
    base_g = row_g * blocks_per_row * 210
    base_u = row_u * blocks_per_row * 210
    qh_w = 8 * (lane // 16) + lane % 8
    qh_shift = 2 * ((lane % 16) // 8)
    bq8 = 4 * (lane // 16) + (lane % 16) // 8
    sc_off = 8 * (lane // 16) + (lane % 16) // 4
    u_w = lane % 8
    xqi = xq.bitcast[Int32]()

    var acc_g: Float32 = 0.0
    var acc_u: Float32 = 0.0
    var b = 0
    while b < blocks_per_row:
        off_g = base_g + b * 210
        off_u = base_u + b * 210
        d_g = Float32((w + off_g + 208).bitcast[Float16]()[0])
        d_u = Float32((w + off_u + 208).bitcast[Float16]()[0])
        vl_g = bitcast[DType.int32, 1](
            (w + off_g + 4 * lane).bitcast[UInt16]().load[width=2]()
        )[0]
        vl_u = bitcast[DType.int32, 1](
            (w + off_u + 4 * lane).bitcast[UInt16]().load[width=2]()
        )[0]
        vh_g = (
            bitcast[DType.int32, 1](
                (w + off_g + 128 + 4 * qh_w).bitcast[UInt16]().load[width=2]()
            )[0]
            >> Int32(qh_shift)
        )
        vh_u = (
            bitcast[DType.int32, 1](
                (w + off_u + 128 + 4 * qh_w).bitcast[UInt16]().load[width=2]()
            )[0]
            >> Int32(qh_shift)
        )
        sp_g = (w + off_g + 192 + sc_off).bitcast[Int8]()
        sp_u = (w + off_u + 192 + sc_off).bitcast[Int8]()
        sc0_g = Float32(Int(sp_g[0]))
        sc1_g = Float32(Int(sp_g[4]))
        sc0_u = Float32(Int(sp_u[0]))
        sc1_u = Float32(Int(sp_u[4]))

        var blk_g: Float32 = 0.0
        var blk_u: Float32 = 0.0
        comptime for i in range(2):
            seg = b * 8 + bq8 + 2 * i
            u = xqi[seg * 8 + u_w]
            su = _dp4a(0x01010101, u, 0)
            q_g = ((vl_g >> Int32(4 * i)) & 0x0F0F0F0F) | (
                ((vh_g >> Int32(4 * i)) << 4) & 0x30303030
            )
            q_u = ((vl_u >> Int32(4 * i)) & 0x0F0F0F0F) | (
                ((vh_u >> Int32(4 * i)) << 4) & 0x30303030
            )
            s_g = _dp4a(q_g, u, 0)
            s_u = _dp4a(q_u, u, 0)
            if i == 0:
                blk_g += xds[2 * seg] * sc0_g * Float32(s_g - 32 * su)
                blk_u += xds[2 * seg] * sc0_u * Float32(s_u - 32 * su)
            else:
                blk_g += xds[2 * seg] * sc1_g * Float32(s_g - 32 * su)
                blk_u += xds[2 * seg] * sc1_u * Float32(s_u - 32 * su)
        acc_g += d_g * blk_g
        acc_u += d_u * blk_u
        b += 1
    return SIMD[DType.float32, 2](warp.sum(acc_g), warp.sum(acc_u))


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


def gemv_q4_k_dp4a_f16_gidx(
    y: UnsafePointer[Float16, MutAnyOrigin],
    wtab: UnsafePointer[UnsafePointer[UInt8, MutAnyOrigin], MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    ids: UnsafePointer[Int32, MutAnyOrigin],
    sel: Int,
):
    """Routed-MoE Q4_K expert GEMV: identical dp4a math to gemv_q4_k_dp4a_f16,
    but the expert is chosen ON DEVICE from `ids[sel]` (no host readback of the
    router selection).

    `wtab[e]` is expert `e`'s own weight base pointer, so experts need not be
    contiguous: each may live in VRAM or in pinned host memory, and the
    residency manager can move one without touching the others. The load is
    uniform across the block, so the indirection costs a single dependent
    fetch. Bit-identical to gemv_q4_k_dp4a_f16 launched against that expert's
    block."""
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
    lrow = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if lrow >= n_rows:
        return
    w = wtab[Int(ids[sel])]
    total = _dot_q4k_i8(w, lrow, xq, xds, n_cols, lane)
    if lane == 0:
        y[lrow] = Float16(total)


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
    _stage_quant_global(x, xq, xds, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = _dot_q6k_i8(w, row, xq, xds, n_cols, lane)
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
    _stage_quant_global(x, xq, xds, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = _dot_q6k_i8(w, row, xq, xds, n_cols, lane)
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
    tid = Int(thread_idx.x)
    _norm_quant_to_shared(xq, xds, h, h32, norm_w, n_cols, ss_from_h16, eps)
    lane = tid % WARP
    wid = tid // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < n_rows:
            total = _dot_q6k_i8(w, row, xq, xds, n_cols, lane)
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
    lane = tid % WARP
    wid = tid // WARP
    # Prefetch the first two row pairs' first weight tiles into L2 before the
    # staging barrier: the FFN weight stream (the kernel's dominant DRAM
    # traffic) starts flowing while the block quantizes x.
    bpr = n_cols // 256
    qoff = 16 + ((lane % 16) // 4) * 32 + 4 * (lane % 4)
    b0 = (lane // 16) * 144
    for i in range(2):
        if i < rows_per_warp:
            row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
            if row < inter:
                _prefetch_l2(w + row * bpr * 144 + b0 + qoff)
                _prefetch_l2(w + (inter + row) * bpr * 144 + b0 + qoff)
    _norm_quant_to_shared(xq, xds, h, h32, norm_w, n_cols, 0, eps)
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
    tid = Int(thread_idx.x)
    _norm_quant_to_shared(xq, xds, h, h32, norm_w, n_cols, 0, eps)
    lane = tid % WARP
    wid = tid // WARP
    for i in range(rows_per_warp):
        row = (Int(block_idx.x) * rows_per_warp + i) * ROWS_PER_BLOCK + wid
        if row < inter:
            gu = _dot2_q6k_i8(w, row, inter + row, xq, xds, n_cols, lane)
            if lane == 0:
                g = Float32(Float16(gu[0]))
                act[row] = Float16(g / (1.0 + exp(-g)) * Float32(Float16(gu[1])))


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
    _stage_quant_global(x, xq, xds, n_cols, tid)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return
    total = _dot_q6k_i8(w, row, xq, xds, n_cols, lane)
    if lane == 0:
        v = Float32(h_io[row]) + Float32(Float16(total))
        h_io[row] = Float16(v)
        h32[row] = v

@always_inline
def _dot_mixed_i8(
    fmt: Int,
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
    """Iloczyn wiersza wagi o formacie wybranym W CZASIE WYKONANIA.

    Format jest staly dla calego bloku (jeden slot = jedna macierz), wiec
    rozgalezienie jest jednorodne w obrebie fali i nie kosztuje dywergencji.
    """
    if fmt == 0:
        return _dot_q4k_i8(w, row, xq, xds, n_cols, lane)
    if fmt == 1:
        return _dot_q6k_i8(w, row, xq, xds, n_cols, lane)
    return _dot_q8_0_i8(w, row, xq, xds, n_cols, lane)


def gemv_mixed_dp4a_group4_f16(
    y0: UnsafePointer[Float16, MutAnyOrigin],
    w0: UnsafePointer[UInt8, MutAnyOrigin],
    rows0: Int,
    fmt0: Int,
    y1: UnsafePointer[Float16, MutAnyOrigin],
    w1: UnsafePointer[UInt8, MutAnyOrigin],
    rows1: Int,
    fmt1: Int,
    y2: UnsafePointer[Float16, MutAnyOrigin],
    w2: UnsafePointer[UInt8, MutAnyOrigin],
    rows2: Int,
    fmt2: Int,
    y3: UnsafePointer[Float16, MutAnyOrigin],
    w3: UnsafePointer[UInt8, MutAnyOrigin],
    rows3: Int,
    fmt3: Int,
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
):
    """Do czterech projekcji o ROZNYCH formatach, jednym uruchomieniem.

    Grupowanie dotad wymagalo jednego formatu we wszystkich slotach, a `Q4_K_M`
    dobiera format PER TENSOR: q/k sa w Q4_K, a v w Q6_K; wejsciowa projekcja
    DeltaNet w Q6_K, a bramka w Q4_K. Przez to najliczniejsze trojki i czworki
    projekcji szly pojedynczo, z siatka za waska, zeby wypelnic karte.

    Format slotu: 0 = Q4_K, 1 = Q6_K, 2 = Q8_0. Nieuzyte sloty maja `rows = 0`.
    Siatka to suma `ceil(rows_i / 8)`. Kwantyzacja aktywacji do LDS jest
    wspolna, wiec zlozenie jej nie powiela.
    """
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

    var tile = Int(block_idx.x)
    blocks0 = (rows0 + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK
    blocks1 = (rows1 + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK
    blocks2 = (rows2 + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK

    if tile < blocks0:
        row0 = tile * ROWS_PER_BLOCK + wid
        if row0 < rows0:
            t0 = _dot_mixed_i8(fmt0, w0, row0, xq, xds, n_cols, lane)
            if lane == 0:
                y0[row0] = Float16(t0)
        return
    tile -= blocks0
    if tile < blocks1:
        row1 = tile * ROWS_PER_BLOCK + wid
        if row1 < rows1:
            t1 = _dot_mixed_i8(fmt1, w1, row1, xq, xds, n_cols, lane)
            if lane == 0:
                y1[row1] = Float16(t1)
        return
    tile -= blocks1
    if tile < blocks2:
        row2 = tile * ROWS_PER_BLOCK + wid
        if row2 < rows2:
            t2 = _dot_mixed_i8(fmt2, w2, row2, xq, xds, n_cols, lane)
            if lane == 0:
                y2[row2] = Float16(t2)
        return
    tile -= blocks2
    row3 = tile * ROWS_PER_BLOCK + wid
    if row3 < rows3:
        t3 = _dot_mixed_i8(fmt3, w3, row3, xq, xds, n_cols, lane)
        if lane == 0:
            y3[row3] = Float16(t3)


def gemv_q4_k_dp4a_group4_f16(
    y0: UnsafePointer[Float16, MutAnyOrigin],
    w0: UnsafePointer[UInt8, MutAnyOrigin],
    rows0: Int,
    y1: UnsafePointer[Float16, MutAnyOrigin],
    w1: UnsafePointer[UInt8, MutAnyOrigin],
    rows1: Int,
    y2: UnsafePointer[Float16, MutAnyOrigin],
    w2: UnsafePointer[UInt8, MutAnyOrigin],
    rows2: Int,
    y3: UnsafePointer[Float16, MutAnyOrigin],
    w3: UnsafePointer[UInt8, MutAnyOrigin],
    rows3: Int,
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
):
    """Do czterech projekcji Q4_K na WSPOLNEJ aktywacji, jednym uruchomieniem.

    Odpowiednik `gemv_q8_0_dp4a_group4_f16` dla Q4_K. Q4_K_M trzyma w tym
    formacie projekcje uwagi, wejsciowe projekcje DeltaNet oraz `gate`/`up`
    FFN — wszystkie czytaja ten sam znormalizowany `x`. Osobno kazda ma za
    mala siatke, zeby wypelnic karte; zlozone daja siatke o sumie wierszy.
    Kwantyzacja aktywacji do LDS jest per blok, wiec zlozenie jej nie powiela.

    Nieuzyte sloty przekazuje sie z `rows = 0`. Siatka to suma `ceil(rows_i / 8)`.
    """
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

    var tile = Int(block_idx.x)
    blocks0 = (rows0 + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK
    blocks1 = (rows1 + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK
    blocks2 = (rows2 + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK

    if tile < blocks0:
        row0 = tile * ROWS_PER_BLOCK + wid
        if row0 < rows0:
            total0 = _dot_q4k_i8(w0, row0, xq, xds, n_cols, lane)
            if lane == 0:
                y0[row0] = Float16(total0)
        return
    tile -= blocks0
    if tile < blocks1:
        row1 = tile * ROWS_PER_BLOCK + wid
        if row1 < rows1:
            total1 = _dot_q4k_i8(w1, row1, xq, xds, n_cols, lane)
            if lane == 0:
                y1[row1] = Float16(total1)
        return
    tile -= blocks1
    if tile < blocks2:
        row2 = tile * ROWS_PER_BLOCK + wid
        if row2 < rows2:
            total2 = _dot_q4k_i8(w2, row2, xq, xds, n_cols, lane)
            if lane == 0:
                y2[row2] = Float16(total2)
        return
    tile -= blocks2
    row3 = tile * ROWS_PER_BLOCK + wid
    if row3 < rows3:
        total3 = _dot_q4k_i8(w3, row3, xq, xds, n_cols, lane)
        if lane == 0:
            y3[row3] = Float16(total3)


def gemv_q8_0_dp4a_group4_f16(
    y0: UnsafePointer[Float16, MutAnyOrigin],
    w0: UnsafePointer[UInt8, MutAnyOrigin],
    rows0: Int,
    y1: UnsafePointer[Float16, MutAnyOrigin],
    w1: UnsafePointer[UInt8, MutAnyOrigin],
    rows1: Int,
    y2: UnsafePointer[Float16, MutAnyOrigin],
    w2: UnsafePointer[UInt8, MutAnyOrigin],
    rows2: Int,
    y3: UnsafePointer[Float16, MutAnyOrigin],
    w3: UnsafePointer[UInt8, MutAnyOrigin],
    rows3: Int,
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
):
    """Do czterech projekcji Q8_0 na WSPOLNEJ aktywacji, jednym uruchomieniem.

    DeltaNet liczy cztery projekcje wejsciowe (in/gate/alpha/beta) z tego samego
    znormalizowanego `x`. Osobno kazda ma za mala siatke, zeby wypelnic karte;
    zlozone razem daja siatke o sumie wierszy. Kwantyzacja aktywacji do LDS jest
    i tak per blok, wiec zlozenie jej nie powiela.

    Nieuzyte sloty przekazuje sie z `rows = 0`. Siatka to suma `ceil(rows_i / 8)`.
    """
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

    var tile = Int(block_idx.x)
    blocks0 = (rows0 + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK
    blocks1 = (rows1 + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK
    blocks2 = (rows2 + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK

    if tile < blocks0:
        row0 = tile * ROWS_PER_BLOCK + wid
        if row0 < rows0:
            total0 = _dot_q8_0_i8(w0, row0, xq, xds, n_cols, lane)
            if lane == 0:
                y0[row0] = Float16(total0)
        return
    tile -= blocks0
    if tile < blocks1:
        row1 = tile * ROWS_PER_BLOCK + wid
        if row1 < rows1:
            total1 = _dot_q8_0_i8(w1, row1, xq, xds, n_cols, lane)
            if lane == 0:
                y1[row1] = Float16(total1)
        return
    tile -= blocks1
    if tile < blocks2:
        row2 = tile * ROWS_PER_BLOCK + wid
        if row2 < rows2:
            total2 = _dot_q8_0_i8(w2, row2, xq, xds, n_cols, lane)
            if lane == 0:
                y2[row2] = Float16(total2)
        return
    tile -= blocks2
    row3 = tile * ROWS_PER_BLOCK + wid
    if row3 < rows3:
        total3 = _dot_q8_0_i8(w3, row3, xq, xds, n_cols, lane)
        if lane == 0:
            y3[row3] = Float16(total3)

