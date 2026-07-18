# ===== File: rotkv.mojo — rotational low-bit KV cache: pack + decode attention =====
# Production kernels for the `rot4`/`rot3` KV cache modes (SPEC.md §5.5). Builds
# on the committed rotquant.mojo core (Walsh-Hadamard rotation R = H/sqrt(d),
# orthogonal AND self-inverse) so K is stored ROTATED+quantized and only q is
# rotated at read time — (R·q)·(R·k) = q·k. V is stored rotated+quantized and
# the attention output accumulator is inverse-rotated once per (seq,head) at the
# end (R self-inverse). Codes are packed densely (4-bit: 2/byte; 3-bit: 8 codes
# per 3 bytes) with one f16 amax scale per (token, head).
#
# Paged store layout (parallel to the f16/fp8 caches in kv.rs):
#   k_packed / v_packed : [n_pages, n_kv_heads, page_size, packed_bytes] u8
#   k_scale  / v_scale  : [n_pages, n_kv_heads, page_size]              f16
# where packed_bytes = head_dim * bits / 8.

from std.gpu import block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.primitives import warp
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import sqrt, exp
from src.rotquant import rotate_inplace

comptime WARP_SIZE = 32
comptime NEG_INF: Float32 = -1e30


# ---------------------------------------------------------------------------
# Rotate + amax-scale quantize + dense bit-pack one length-`head_dim` f16
# vector. Writes the packed code stream and the per-vector f16 scale. This is
# the exact store path decode attention reads back, so its error is the real
# deployment error (same as rotquant_roundtrip's forward half).
# ---------------------------------------------------------------------------
def _pack_vec[head_dim: Int, bits: Int](
    x: UnsafePointer[Float16, MutAnyOrigin],
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    scale_out: UnsafePointer[Float16, MutAnyOrigin],
):
    comptime levels = (1 << (bits - 1)) - 1  # 4-bit -> 7, 3-bit -> 3
    comptime bias = levels
    comptime packed_bytes = (head_dim * bits) // 8

    var r = InlineArray[Float32, head_dim](fill=0.0)
    for i in range(head_dim):
        r[i] = Float32(x[i])
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
    scale_out[0] = Float16(scale)

    var codes = InlineArray[Int32, head_dim](fill=0)
    for i in range(head_dim):
        q = Int32(round(r[i] * inv_scale))
        if q > Int32(levels):
            q = Int32(levels)
        if q < -Int32(levels):
            q = -Int32(levels)
        codes[i] = q + Int32(bias)

    for b in range(packed_bytes):
        packed[b] = 0

    comptime if bits == 4:
        for i in range(head_dim // 2):
            lo = UInt8(codes[2 * i] & 0xF)
            hi = UInt8(codes[2 * i + 1] & 0xF)
            packed[i] = lo | (hi << 4)
    else:
        for g in range(head_dim // 8):
            var acc: UInt32 = 0
            for k in range(8):
                acc |= (UInt32(codes[g * 8 + k] & 0x7)) << UInt32(3 * k)
            packed[g * 3 + 0] = UInt8(acc & 0xFF)
            packed[g * 3 + 1] = UInt8((acc >> 8) & 0xFF)
            packed[g * 3 + 2] = UInt8((acc >> 16) & 0xFF)


# One signed dequantized element (rotated space) at position `idx` of a densely
# packed low-bit vector. `scale` is the vector's f16 amax scale.
def _unpack_elem[bits: Int](
    packed: UnsafePointer[UInt8, MutAnyOrigin], idx: Int, scale: Float32
) -> Float32:
    comptime levels = (1 << (bits - 1)) - 1
    comptime bias = levels
    comptime if bits == 4:
        byte = packed[idx // 2]
        var nib: Int32
        if idx % 2 == 0:
            nib = Int32(byte & 0xF)
        else:
            nib = Int32((byte >> 4) & 0xF)
        return Float32(nib - Int32(bias)) * scale
    else:
        g = idx // 8
        k = idx % 8
        acc = (
            UInt32(packed[g * 3 + 0])
            | (UInt32(packed[g * 3 + 1]) << 8)
            | (UInt32(packed[g * 3 + 2]) << 16)
        )
        c = Int32((acc >> UInt32(3 * k)) & 0x7) - Int32(bias)
        return Float32(c) * scale


# ---------------------------------------------------------------------------
# kv_pack_rot: rotate+quant+pack a batch of T tokens' K/V ([T, n_kv_heads,
# head_dim] f16) into the paged rotational store at positions
# base_pos..base_pos+T. Grid (T, n_kv_heads); thread 0 of each block owns one
# (token, head) vector. Used by both the prefill append and the decode
# eviction path.
# ---------------------------------------------------------------------------
def kv_pack_rot[head_dim: Int, bits: Int](
    k_packed: UnsafePointer[UInt8, MutAnyOrigin],
    v_packed: UnsafePointer[UInt8, MutAnyOrigin],
    k_scale: UnsafePointer[Float16, MutAnyOrigin],
    v_scale: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    base_pos: Int,
    n_kv_heads: Int,
    page_size: Int,
):
    comptime packed_bytes = (head_dim * bits) // 8
    if Int(thread_idx.x) != 0:
        return
    tok = Int(block_idx.x)
    kvh = Int(block_idx.y)
    pos = base_pos + tok
    page = Int(page_table[pos // page_size])
    slot = pos % page_size
    dst = (page * n_kv_heads + kvh) * page_size + slot
    src = (tok * n_kv_heads + kvh) * head_dim
    _pack_vec[head_dim, bits](
        k_in + src, k_packed + dst * packed_bytes, k_scale + dst
    )
    _pack_vec[head_dim, bits](
        v_in + src, v_packed + dst * packed_bytes, v_scale + dst
    )


# ---------------------------------------------------------------------------
# attn_decode_rot: single-block (one warp) rotational decode attention over the
# paged low-bit store. out[seq, qh] = softmax(q·K^T * scale) · V computed
# entirely in rotated space:
#   * q is rotated once into shared memory (R·q),
#   * each cached K/V element is unpacked to its rotated-space value on the fly,
#   * scores use (R·q)·dequant(R·k) = q·k (minus quant error),
#   * the online-softmax V accumulator lives in rotated space and is
#     inverse-rotated once at the end (R self-inverse) before the ÷l normalize.
# Layouts:
#   q/out:            [n_seqs, n_q_heads, head_dim] f16
#   k_packed/v_packed:[n_pages, n_kv_heads, page_size, packed_bytes] u8
#   k_scale/v_scale:  [n_pages, n_kv_heads, page_size] f16
#   page_table:       [n_seqs, max_pages] i32; seq_lens: [n_seqs] i32
# Grid (n_seqs, n_q_heads); block 32.
# ---------------------------------------------------------------------------
def attn_decode_rot[head_dim: Int, bits: Int](
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    q: UnsafePointer[Float16, MutAnyOrigin],
    k_packed: UnsafePointer[UInt8, MutAnyOrigin],
    v_packed: UnsafePointer[UInt8, MutAnyOrigin],
    k_scale: UnsafePointer[Float16, MutAnyOrigin],
    v_scale: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    seq_lens: UnsafePointer[Int32, MutAnyOrigin],
    n_q_heads: Int,
    n_kv_heads: Int,
    page_size: Int,
    max_pages: Int,
    scale: Float32,
):
    comptime epl = head_dim // WARP_SIZE
    comptime packed_bytes = (head_dim * bits) // 8

    seq = Int(block_idx.x)
    qh = Int(block_idx.y)
    kvh = qh // (n_q_heads // n_kv_heads)
    lane = Int(thread_idx.x)
    ctx_len = Int(seq_lens[seq])

    shared_q = stack_allocation[
        head_dim, Float32, address_space = AddressSpace.SHARED
    ]()

    inv_rot = 1.0 / sqrt(Float32(head_dim))

    q_base = (seq * n_q_heads + qh) * head_dim
    var i = lane
    while i < head_dim:
        shared_q[i] = Float32(q[q_base + i])
        i += WARP_SIZE
    barrier()
    if lane == 0:
        var h = 1
        while h < head_dim:
            var a0 = 0
            while a0 < head_dim:
                var j = a0
                while j < a0 + h:
                    va = shared_q[j]
                    vb = shared_q[j + h]
                    shared_q[j] = va + vb
                    shared_q[j + h] = va - vb
                    j += 1
                a0 += h + h
            h += h
        for t in range(head_dim):
            shared_q[t] = shared_q[t] * inv_rot
    barrier()

    var q_frag = SIMD[DType.float32, epl](0.0)

    comptime for e in range(epl):
        q_frag[e] = shared_q[e * WARP_SIZE + lane]

    var m: Float32 = NEG_INF
    var l: Float32 = 0.0
    var acc = SIMD[DType.float32, epl](0.0)

    var pos = 0
    while pos < ctx_len:
        page = Int(page_table[seq * max_pages + pos // page_size])
        slot = pos % page_size
        base_idx = (page * n_kv_heads + kvh) * page_size + slot
        pk = k_packed + base_idx * packed_bytes
        ks = Float32(k_scale[base_idx])

        var dot: Float32 = 0.0

        comptime for e in range(epl):
            idx = e * WARP_SIZE + lane
            dot += q_frag[e] * _unpack_elem[bits](pk, idx, ks)
        score = warp.sum(dot) * scale

        var m_new = m
        if score > m_new:
            m_new = score
        factor = exp(m - m_new)
        p = exp(score - m_new)
        l = l * factor + p

        pv = v_packed + base_idx * packed_bytes
        vs = Float32(v_scale[base_idx])

        comptime for e in range(epl):
            idx = e * WARP_SIZE + lane
            acc[e] = acc[e] * factor + p * _unpack_elem[bits](pv, idx, vs)
        m = m_new
        pos += 1

    # Inverse-rotate the rotated-space accumulator via shared staging, then
    # normalize and store. l is identical across lanes (warp.sum broadcast).
    comptime for e in range(epl):
        shared_q[e * WARP_SIZE + lane] = acc[e]
    barrier()
    if lane == 0:
        var h = 1
        while h < head_dim:
            var a0 = 0
            while a0 < head_dim:
                var j = a0
                while j < a0 + h:
                    va = shared_q[j]
                    vb = shared_q[j + h]
                    shared_q[j] = va + vb
                    shared_q[j + h] = va - vb
                    j += 1
                a0 += h + h
            h += h
        for t in range(head_dim):
            shared_q[t] = shared_q[t] * inv_rot
    barrier()
    inv_l = 1.0 / l

    comptime for e in range(epl):
        idx = e * WARP_SIZE + lane
        out_ptr[q_base + idx] = Float16(shared_q[idx] * inv_l)


# ---------------------------------------------------------------------------
# kv_pack_rot_from_cache: rotate+quant+pack T tokens already resident in the
# paged f16 K/V cache (positions base_pos..base_pos+T) into the rotational
# store. Source and destination share the paged (page,head,slot) addressing, so
# the engine reuses the proven f16 append path (qkv_post / kv_append_batch) and
# commits the low-bit copy right after. Grid (T, n_kv_heads); thread 0 per block.
# ---------------------------------------------------------------------------
def kv_pack_rot_from_cache[head_dim: Int, bits: Int](
    k_packed: UnsafePointer[UInt8, MutAnyOrigin],
    v_packed: UnsafePointer[UInt8, MutAnyOrigin],
    k_scale: UnsafePointer[Float16, MutAnyOrigin],
    v_scale: UnsafePointer[Float16, MutAnyOrigin],
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    base_pos: Int,
    n_kv_heads: Int,
    page_size: Int,
):
    comptime packed_bytes = (head_dim * bits) // 8
    if Int(thread_idx.x) != 0:
        return
    tok = Int(block_idx.x)
    kvh = Int(block_idx.y)
    pos = base_pos + tok
    page = Int(page_table[pos // page_size])
    slot = pos % page_size
    idx = (page * n_kv_heads + kvh) * page_size + slot
    _pack_vec[head_dim, bits](
        k_cache + idx * head_dim, k_packed + idx * packed_bytes, k_scale + idx
    )
    _pack_vec[head_dim, bits](
        v_cache + idx * head_dim, v_packed + idx * packed_bytes, v_scale + idx
    )


comptime kv_pack_rot_from_cache_hd64_b4 = kv_pack_rot_from_cache[64, 4]
comptime kv_pack_rot_from_cache_hd64_b3 = kv_pack_rot_from_cache[64, 3]
comptime kv_pack_rot_from_cache_hd128_b4 = kv_pack_rot_from_cache[128, 4]
comptime kv_pack_rot_from_cache_hd128_b3 = kv_pack_rot_from_cache[128, 3]

comptime kv_pack_rot_hd64_b4 = kv_pack_rot[64, 4]
comptime kv_pack_rot_hd64_b3 = kv_pack_rot[64, 3]
comptime kv_pack_rot_hd128_b4 = kv_pack_rot[128, 4]
comptime kv_pack_rot_hd128_b3 = kv_pack_rot[128, 3]

comptime attn_decode_rot_hd64_b4 = attn_decode_rot[64, 4]
comptime attn_decode_rot_hd64_b3 = attn_decode_rot[64, 3]
comptime attn_decode_rot_hd128_b4 = attn_decode_rot[128, 4]
comptime attn_decode_rot_hd128_b3 = attn_decode_rot[128, 3]
