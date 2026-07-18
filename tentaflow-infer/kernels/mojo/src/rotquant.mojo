# ===== File: rotquant.mojo — Walsh-Hadamard rotation + low-bit KV quantizer =====
# TurboQuant-class rotational KV quantization (SPEC.md §5.5). The Walsh-Hadamard
# transform R = H/sqrt(d) is orthogonal AND self-inverse (R^2 = I), so for
# attention scores (R·q)·(R·k) = q·k: K is stored ROTATED+quantized and only q
# is rotated once at read time — no inverse-rotate for K. V is stored
# rotated+quantized and the attention output accumulator is inverse-rotated once
# per (seq,head) at the end. Rotation decorrelates the head-dim coordinates and
# spreads outlier energy, so scalar low-bit (3/4-bit) quantization of the
# rotated vector keeps far more accuracy than quantizing the raw vector.
#
# `bits` is a compile-time parameter (3 or 4); `head_dim` is compile-time too
# (64/128/256). Codes are packed densely (4-bit: 2/byte; 3-bit: 8 codes per
# 3 bytes) with one f16 amax scale per (token, head).

from std.gpu import block_idx, thread_idx
from std.math import sqrt


# ---------------------------------------------------------------------------
# Fast Walsh-Hadamard butterfly (unnormalized). H is symmetric with H^2 = d·I,
# so the normalized rotation R = H/sqrt(d) satisfies R = R^T and R^2 = I.
# Operates in place on a length-`d` shared/register buffer. `d` must be a power
# of two; the stage count log2(d) is unrolled at compile time.
# ---------------------------------------------------------------------------
def fht_inplace[d: Int](mut buf: InlineArray[Float32, d]):
    var h = 1
    while h < d:
        var i = 0
        while i < d:
            var j = i
            while j < i + h:
                a = buf[j]
                b = buf[j + h]
                buf[j] = a + b
                buf[j + h] = a - b
                j += 1
            i += h + h
        h += h


# Orthonormal rotation R·x = H·x / sqrt(d), in place.
def rotate_inplace[d: Int](mut buf: InlineArray[Float32, d]):
    fht_inplace[d](buf)
    inv = 1.0 / sqrt(Float32(d))
    for i in range(d):
        buf[i] = buf[i] * inv


# ---------------------------------------------------------------------------
# Round-trip verification kernel: one block per (token,head) vector. Thread 0
# performs rotate → amax-scale quantize → dense pack → unpack → dequantize →
# inverse-rotate, writing:
#   xr_hat  = dequantized value in ROTATED space (what K attention dots against)
#   xhat    = full inverse-rotated reconstruction (what V contributes, decoded)
#   scale   = per-vector f16 amax scale
#   packed  = the dense low-bit code stream actually stored
# This exercises the exact store/pack/unpack path the cache uses, so the
# measured error is the real deployment error, not an idealized one.
# ---------------------------------------------------------------------------
def rotquant_roundtrip[head_dim: Int, bits: Int](
    x: UnsafePointer[Float16, MutAnyOrigin],
    xr_hat: UnsafePointer[Float16, MutAnyOrigin],
    xhat: UnsafePointer[Float16, MutAnyOrigin],
    scale_out: UnsafePointer[Float16, MutAnyOrigin],
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    n_vecs: Int,
):
    comptime levels = (1 << (bits - 1)) - 1  # 4-bit -> 7, 3-bit -> 3
    comptime bias = levels
    # Dense packing: 4-bit -> head_dim/2 bytes; 3-bit -> head_dim/8 * 3 bytes.
    comptime packed_bytes = (head_dim * bits) // 8

    v = Int(block_idx.x)
    if v >= n_vecs:
        return
    if Int(thread_idx.x) != 0:
        return

    base = v * head_dim
    var r = InlineArray[Float32, head_dim](fill=0.0)
    for i in range(head_dim):
        r[i] = Float32(x[base + i])

    rotate_inplace[head_dim](r)

    var amax: Float32 = 0.0
    for i in range(head_dim):
        a = r[i]
        if a < 0.0:
            a = -a
        if a > amax:
            amax = a
    var scale = amax / Float32(levels)
    if scale == 0.0:
        scale = 1.0
    inv_scale = 1.0 / scale
    scale_out[v] = Float16(scale)

    # Quantize to signed codes, then bias into unsigned [0, 2*levels].
    var codes = InlineArray[Int32, head_dim](fill=0)
    for i in range(head_dim):
        q = Int32(round(r[i] * inv_scale))
        if q > Int32(levels):
            q = Int32(levels)
        if q < -Int32(levels):
            q = -Int32(levels)
        codes[i] = q + Int32(bias)

    # Dense bit-packing (LSB-first within the code stream).
    pbase = v * packed_bytes
    for b in range(packed_bytes):
        packed[pbase + b] = 0

    comptime if bits == 4:
        for i in range(head_dim // 2):
            lo = UInt8(codes[2 * i] & 0xF)
            hi = UInt8(codes[2 * i + 1] & 0xF)
            packed[pbase + i] = lo | (hi << 4)
    else:
        # 3-bit: 8 codes -> 24 bits -> 3 bytes.
        for g in range(head_dim // 8):
            var acc: UInt32 = 0
            for k in range(8):
                acc |= (UInt32(codes[g * 8 + k] & 0x7)) << UInt32(3 * k)
            packed[pbase + g * 3 + 0] = UInt8(acc & 0xFF)
            packed[pbase + g * 3 + 1] = UInt8((acc >> 8) & 0xFF)
            packed[pbase + g * 3 + 2] = UInt8((acc >> 16) & 0xFF)

    # Unpack -> dequantize into rotated space.
    var deq = InlineArray[Float32, head_dim](fill=0.0)

    comptime if bits == 4:
        for i in range(head_dim // 2):
            byte = packed[pbase + i]
            c0 = Int32(byte & 0xF) - Int32(bias)
            c1 = Int32((byte >> 4) & 0xF) - Int32(bias)
            deq[2 * i] = Float32(c0) * scale
            deq[2 * i + 1] = Float32(c1) * scale
    else:
        for g in range(head_dim // 8):
            acc = (
                UInt32(packed[pbase + g * 3 + 0])
                | (UInt32(packed[pbase + g * 3 + 1]) << 8)
                | (UInt32(packed[pbase + g * 3 + 2]) << 16)
            )
            for k in range(8):
                c = Int32((acc >> UInt32(3 * k)) & 0x7) - Int32(bias)
                deq[g * 8 + k] = Float32(c) * scale

    for i in range(head_dim):
        xr_hat[base + i] = Float16(deq[i])

    # Inverse-rotate (R self-inverse) to reconstruct the original-space value.
    rotate_inplace[head_dim](deq)
    for i in range(head_dim):
        xhat[base + i] = Float16(deq[i])


comptime rotquant_roundtrip_hd64_b4 = rotquant_roundtrip[64, 4]
comptime rotquant_roundtrip_hd64_b3 = rotquant_roundtrip[64, 3]
comptime rotquant_roundtrip_hd128_b4 = rotquant_roundtrip[128, 4]
comptime rotquant_roundtrip_hd128_b3 = rotquant_roundtrip[128, 3]
comptime rotquant_roundtrip_hd256_b4 = rotquant_roundtrip[256, 4]
comptime rotquant_roundtrip_hd256_b3 = rotquant_roundtrip[256, 3]


# Rotate-only kernel (produces R·q for the attention dot-product test).
def rotate_vec[head_dim: Int](
    x: UnsafePointer[Float16, MutAnyOrigin],
    dst: UnsafePointer[Float16, MutAnyOrigin],
    n_vecs: Int,
):
    v = Int(block_idx.x)
    if v >= n_vecs:
        return
    if Int(thread_idx.x) != 0:
        return
    base = v * head_dim
    var r = InlineArray[Float32, head_dim](fill=0.0)
    for i in range(head_dim):
        r[i] = Float32(x[base + i])
    rotate_inplace[head_dim](r)
    for i in range(head_dim):
        dst[base + i] = Float16(r[i])


comptime rotate_vec_hd128 = rotate_vec[128]
