# ===== File: rotkv.mojo — rotational low-bit KV cache: pack + dual-region decode attention =====
# Production kernels for the `rot4`/`rot3` KV cache modes (SPEC.md §5.5). Builds
# on the committed rotquant.mojo core (Walsh-Hadamard rotation R = H/sqrt(d),
# orthogonal AND self-inverse) so K is stored ROTATED+quantized and only q is
# rotated at read time — (R·q)·(R·k) = q·k. V is stored rotated+quantized and
# the attention output accumulator is inverse-rotated once per (seq,head) at the
# end (R self-inverse). Codes are packed densely (4-bit: 2/byte; 3-bit: 8 codes
# per 3 bytes) with one f16 amax scale per (token, head).
#
# Residual-window reclaim (SPEC.md §5.5): the full-context f16 slab is gone. A
# token is packed into the low-bit store AND its ROTATED f16 vector is written to
# a small per-layer residual ring (`ring_slots` most-recent positions). Decode
# attention reads the ring (higher-fidelity rotated f16) for the recent window
# and the packed store for everything older — a single online-softmax pass in
# rotated space, so the ring merge needs no separate accumulator. The ring is
# stored ROTATED, exactly like the packed store after dequant, so both regions
# feed one rotated accumulator that is inverse-rotated once at the end.
#
# Paged store layout (parallel to the f16/fp8 caches in kv.rs):
#   k_packed / v_packed : [n_pages, n_kv_heads, page_size, packed_bytes] u8
#   k_scale  / v_scale  : [n_pages, n_kv_heads, page_size]              f16
# Residual ring layout (contiguous, indexed by pos % ring_slots):
#   k_ring   / v_ring   : [ring_slots, n_kv_heads, head_dim]            f16 (rotated)
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


# Same as `_pack_vec` but also writes the ROTATED f16 vector to `ring`
# (length head_dim). The residual window keeps the recent tokens at f16
# fidelity in rotated space; decode attention reads them straight (no unpack),
# consistent with the packed store's rotated-space math.
def _pack_vec_ring[head_dim: Int, bits: Int](
    x: UnsafePointer[Float16, MutAnyOrigin],
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    scale_out: UnsafePointer[Float16, MutAnyOrigin],
    ring: UnsafePointer[Float16, MutAnyOrigin],
):
    comptime levels = (1 << (bits - 1)) - 1
    comptime bias = levels
    comptime packed_bytes = (head_dim * bits) // 8

    var r = InlineArray[Float32, head_dim](fill=0.0)
    for i in range(head_dim):
        r[i] = Float32(x[i])
    rotate_inplace[head_dim](r)

    for i in range(head_dim):
        ring[i] = Float16(r[i])

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
# head_dim] f16, rope'd) into the paged rotational store at positions
# base_pos..base_pos+T, AND write the rotated f16 vectors into the residual
# ring at pos % ring_slots. Grid (T, n_kv_heads); thread 0 of each block owns
# one (token, head) vector. Used by both the prefill append and the decode
# eviction path — the residual window keeps the recent tokens at f16 while the
# packed store holds the full history for reclaim.
# ---------------------------------------------------------------------------
def kv_pack_rot[head_dim: Int, bits: Int](
    k_packed: UnsafePointer[UInt8, MutAnyOrigin],
    v_packed: UnsafePointer[UInt8, MutAnyOrigin],
    k_scale: UnsafePointer[Float16, MutAnyOrigin],
    v_scale: UnsafePointer[Float16, MutAnyOrigin],
    k_ring: UnsafePointer[Float16, MutAnyOrigin],
    v_ring: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    base_pos: Int,
    n_kv_heads: Int,
    page_size: Int,
    ring_slots: Int,
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
    rslot = (pos % ring_slots) * n_kv_heads + kvh
    src = (tok * n_kv_heads + kvh) * head_dim
    _pack_vec_ring[head_dim, bits](
        k_in + src, k_packed + dst * packed_bytes, k_scale + dst, k_ring + rslot * head_dim
    )
    _pack_vec_ring[head_dim, bits](
        v_in + src, v_packed + dst * packed_bytes, v_scale + dst, v_ring + rslot * head_dim
    )


# ---------------------------------------------------------------------------
# attn_decode_rot: single-block (one warp) rotational decode attention over the
# dual-region store (packed older tokens + residual f16 ring for the recent
# window). out[seq, qh] = softmax(q·K^T * scale) · V computed entirely in
# rotated space:
#   * q is rotated once into shared memory (R·q),
#   * older positions unpack their low-bit code to the rotated-space value,
#   * recent positions (pos >= ctx_len - ring_slots) read the rotated f16 ring
#     directly (no unpack, higher fidelity),
#   * scores use (R·q)·k_rot = q·k (minus quant error for the packed region),
#   * one rotated V accumulator, inverse-rotated once at the end.
# `ring_slots == 0` degrades to the pure packed path (older == everything).
# Layouts:
#   q/out:            [n_seqs, n_q_heads, head_dim] f16
#   k_packed/v_packed:[n_pages, n_kv_heads, page_size, packed_bytes] u8
#   k_scale/v_scale:  [n_pages, n_kv_heads, page_size] f16
#   k_ring/v_ring:    [ring_slots, n_kv_heads, head_dim] f16 (rotated)
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
    k_ring: UnsafePointer[Float16, MutAnyOrigin],
    v_ring: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    seq_lens: UnsafePointer[Int32, MutAnyOrigin],
    n_q_heads: Int,
    n_kv_heads: Int,
    page_size: Int,
    max_pages: Int,
    ring_slots: Int,
    scale: Float32,
):
    comptime epl = head_dim // WARP_SIZE
    comptime packed_bytes = (head_dim * bits) // 8

    seq = Int(block_idx.x)
    qh = Int(block_idx.y)
    kvh = qh // (n_q_heads // n_kv_heads)
    lane = Int(thread_idx.x)
    ctx_len = Int(seq_lens[seq])

    var ring_start = ctx_len - ring_slots
    if ring_start < 0:
        ring_start = 0

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
        var use_ring = pos >= ring_start
        var dot: Float32 = 0.0
        var rbase = 0
        var base_idx = 0
        if use_ring:
            rbase = ((pos % ring_slots) * n_kv_heads + kvh) * head_dim

            comptime for e in range(epl):
                idx = e * WARP_SIZE + lane
                dot += q_frag[e] * Float32(k_ring[rbase + idx])
        else:
            page = Int(page_table[seq * max_pages + pos // page_size])
            slot = pos % page_size
            base_idx = (page * n_kv_heads + kvh) * page_size + slot
            pk = k_packed + base_idx * packed_bytes
            ks = Float32(k_scale[base_idx])

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

        if use_ring:
            comptime for e in range(epl):
                idx = e * WARP_SIZE + lane
                acc[e] = acc[e] * factor + p * Float32(v_ring[rbase + idx])
        else:
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
# attn_prefill_rot: causal prefill attention for a chunk of T query tokens over
# the packed rotational store (older-region only — the ring's recent window
# would be overwritten within a chunk, so prefill reads the full-history packed
# store, matching the committed prompt fidelity). Query token `tok` attends
# positions 0..base_pos+tok. Grid (T, n_q_heads); block 32 (one warp per query,
# head). Reuses attn_decode_rot's rotated-space math per query token.
#   q/out: [T, n_q_heads, head_dim] f16
# ---------------------------------------------------------------------------
def attn_prefill_rot[head_dim: Int, bits: Int](
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    q: UnsafePointer[Float16, MutAnyOrigin],
    k_packed: UnsafePointer[UInt8, MutAnyOrigin],
    v_packed: UnsafePointer[UInt8, MutAnyOrigin],
    k_scale: UnsafePointer[Float16, MutAnyOrigin],
    v_scale: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    base_pos: Int,
    n_q_heads: Int,
    n_kv_heads: Int,
    page_size: Int,
    scale: Float32,
):
    comptime epl = head_dim // WARP_SIZE
    comptime packed_bytes = (head_dim * bits) // 8

    tok = Int(block_idx.x)
    qh = Int(block_idx.y)
    kvh = qh // (n_q_heads // n_kv_heads)
    lane = Int(thread_idx.x)
    ctx_len = base_pos + tok + 1

    shared_q = stack_allocation[
        head_dim, Float32, address_space = AddressSpace.SHARED
    ]()

    inv_rot = 1.0 / sqrt(Float32(head_dim))

    q_base = (tok * n_q_heads + qh) * head_dim
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
        page = Int(page_table[pos // page_size])
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
# store. Retained for the golden kernel test (forge-kernels/tests/rotkv.rs),
# which packs a paged f16 region and validates the decode-attention math; the
# engine uses the ring-writing `kv_pack_rot` on linear rope'd K/V. Grid
# (T, n_kv_heads); thread 0 per block.
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

comptime attn_prefill_rot_hd64_b4 = attn_prefill_rot[64, 4]
comptime attn_prefill_rot_hd64_b3 = attn_prefill_rot[64, 3]
comptime attn_prefill_rot_hd128_b4 = attn_prefill_rot[128, 4]
comptime attn_prefill_rot_hd128_b3 = attn_prefill_rot[128, 3]
