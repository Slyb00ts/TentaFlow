# ===== File: pack_gguf_fp8.mojo — GPU pack of resident GGUF weights to e4m3 fp8 =====
# Builds the Modular-fp8 prefill packs straight from the RESIDENT raw GGUF
# bytes (Q4_K / Q6_K / Q8_0), replacing the CPU dequant + host pack that took
# tens of seconds per 7B model. One 256-thread block packs one output row:
# pass 1 reduces the row absmax over on-the-fly dequantized values, pass 2
# encodes `x * 448/absmax` to e4m3. The dequant formulas and the e4m3
# conversion mirror forge-formats (`dq_q4_k`/`dq_q6_k`/`dq_q8_0`,
# `f32_to_f8e4m3`) BRANCH FOR BRANCH so the GPU codes and scales are
# bit-identical to the CPU pack (golden-gated).

from std.gpu import block_idx, thread_idx
from std.gpu.primitives import warp
from std.math import floor
from std.memory import bitcast
from src.reduce import block_reduce_max

comptime BLOCK = 256


def _round_ties_even(x: Float32) -> Float32:
    # Mirror of forge-formats round_ties_even: `round` rounds halves away from
    # zero; an exact .5 fraction picks the even neighbor instead.
    r = round(x)
    lo = floor(x)
    if abs(x - lo - 0.5) < Float32(1.1920929e-07):
        if Int64(lo) % 2 == 0:
            return lo
        return lo + 1.0
    return r


def _f32_to_f8e4m3(v: Float32) -> UInt8:
    if v != v:
        return UInt8(0x7F)
    bits_v = bitcast[DType.uint32, 1](SIMD[DType.float32, 1](v))[0]
    var sign = UInt8(0)
    if (bits_v >> 31) != 0:
        sign = UInt8(0x80)
    a = abs(v)
    if a == 0.0:
        return sign
    if a >= 448.0:
        return sign | 0x7E
    bits = bitcast[DType.uint32, 1](SIMD[DType.float32, 1](a))[0]
    e = Int32((bits >> 23) & 0xFF) - 127
    if e < -9:
        return sign
    if e < -6:
        man_f = _round_ties_even(a * 512.0)
        if man_f > 8.0:
            man_f = 8.0
        if man_f < 0.0:
            man_f = 0.0
        sman = UInt32(man_f)
        if sman == 8:
            return sign | UInt8(1 << 3)
        return sign | (UInt8(sman) & 0x07)
    # 2^e built from raw bits — exact for every reachable exponent.
    p = bitcast[DType.float32, 1](SIMD[DType.uint32, 1](UInt32(e + 127) << 23))[0]
    mant_f = (a / p) - 1.0
    man_f = _round_ties_even(mant_f * 8.0)
    var exp_field: Int32
    var man: UInt32
    if man_f >= 8.0:
        exp_field = e + 8
        man = 0
    else:
        exp_field = e + 7
        man = UInt32(man_f)
    if exp_field >= 15 and not (exp_field == 15 and man <= 6):
        return sign | 0x7E
    if exp_field > 15:
        exp_field = 15
    return sign | (UInt8(exp_field) << 3) | (UInt8(man) & 0x07)


def _q4k_scale_min(
    j: Int, s: UnsafePointer[UInt8, MutAnyOrigin]
) -> Tuple[UInt8, UInt8]:
    # llama.cpp get_scale_min_k4 over the 12 packed scale bytes.
    if j < 4:
        return (s[j] & 63, s[j + 4] & 63)
    return (
        (s[j + 4] & 0x0F) | ((s[j - 4] >> 6) << 4),
        (s[j + 4] >> 4) | ((s[j] >> 6) << 4),
    )


def _dequant_q4k(
    w: UnsafePointer[UInt8, MutAnyOrigin], off: Int, r: Int
) -> Float32:
    d = Float32((w + off).bitcast[Float16]()[0])
    mn = Float32((w + off + 2).bitcast[Float16]()[0])
    j = (r // 64) * 64
    k = r % 64
    var is_ = (r // 64) * 2
    var qb: UInt8
    if k < 32:
        qb = (w + off + 16 + j // 2 + k)[0] & 0x0F
    else:
        qb = (w + off + 16 + j // 2 + (k - 32))[0] >> 4
        is_ += 1
    sc, m = _q4k_scale_min(is_, w + off + 4)
    return d * Float32(Int(sc)) * Float32(Int(qb)) - mn * Float32(Int(m))


def _dequant_q6k(
    w: UnsafePointer[UInt8, MutAnyOrigin], off: Int, r: Int
) -> Float32:
    d = Float32((w + off + 208).bitcast[Float16]()[0])
    n = r // 128
    i = r % 128
    seg = i // 32
    l = i % 32
    is_ = l // 16
    ql = w + off + n * 64
    qh = w + off + 128 + n * 32
    sc = (w + off + 192 + n * 8).bitcast[Int8]()
    var q: Int32
    var s: Int32
    if seg == 0:
        q = Int32((ql[l] & 0x0F) | ((qh[l] & 3) << 4))
        s = Int32(sc[is_])
    elif seg == 1:
        q = Int32((ql[l + 32] & 0x0F) | (((qh[l] >> 2) & 3) << 4))
        s = Int32(sc[is_ + 2])
    elif seg == 2:
        q = Int32((ql[l] >> 4) | (((qh[l] >> 4) & 3) << 4))
        s = Int32(sc[is_ + 4])
    else:
        q = Int32((ql[l + 32] >> 4) | (((qh[l] >> 6) & 3) << 4))
        s = Int32(sc[is_ + 6])
    return d * Float32(s) * Float32(q - 32)


def _dequant_q8_0(
    w: UnsafePointer[UInt8, MutAnyOrigin], off: Int, r: Int
) -> Float32:
    d = Float32((w + off).bitcast[Float16]()[0])
    return d * Float32(Int((w + off + 2).bitcast[Int8]()[r]))


# FMT: 0 = Q4_K (256-elem, 144 B), 1 = Q6_K (256-elem, 210 B),
# 2 = Q8_0 (32-elem, 34 B).
def pack_gguf_fp8_impl[FMT: Int](
    codes: UnsafePointer[UInt8, MutAnyOrigin],
    scales_out: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    row = Int(block_idx.x)
    if row >= n_rows:
        return
    tid = Int(thread_idx.x)
    comptime blk_elems = 32 if FMT == 2 else 256
    comptime blk_bytes = 34 if FMT == 2 else (144 if FMT == 0 else 210)
    row_base = row * (n_cols // blk_elems) * blk_bytes

    var m: Float32 = 0.0
    var i = tid
    while i < n_cols:
        off = row_base + (i // blk_elems) * blk_bytes
        r = i % blk_elems
        var v: Float32
        if FMT == 0:
            v = _dequant_q4k(w, off, r)
        elif FMT == 1:
            v = _dequant_q6k(w, off, r)
        else:
            v = _dequant_q8_0(w, off, r)
        m = max(m, abs(v))
        i += BLOCK
    m = block_reduce_max(m)

    if m == 0.0:
        if tid == 0:
            scales_out[row] = 0.0
        i = tid
        while i < n_cols:
            codes[row * n_cols + i] = 0
            i += BLOCK
        return
    if tid == 0:
        scales_out[row] = m / 448.0
    inv = 448.0 / m
    i = tid
    while i < n_cols:
        off = row_base + (i // blk_elems) * blk_bytes
        r = i % blk_elems
        var v: Float32
        if FMT == 0:
            v = _dequant_q4k(w, off, r)
        elif FMT == 1:
            v = _dequant_q6k(w, off, r)
        else:
            v = _dequant_q8_0(w, off, r)
        codes[row * n_cols + i] = _f32_to_f8e4m3(v * inv)
        i += BLOCK


comptime pack_q4_k_fp8 = pack_gguf_fp8_impl[0]
comptime pack_q6_k_fp8 = pack_gguf_fp8_impl[1]
comptime pack_q8_0_fp8 = pack_gguf_fp8_impl[2]
