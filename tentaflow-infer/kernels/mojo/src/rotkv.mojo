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

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.primitives import warp
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import sqrt, exp
from src.rotquant import rotate_inplace

comptime WARP_SIZE = 32
comptime MAX_WARPS = 8
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


# Vectorized dequant of a CONTIGUOUS run of `epl` codes starting at index
# `start` of a densely packed low-bit vector, returned as an f32 SIMD fragment
# (rotated space). One lane owns the run [start, start+epl); with the contiguous
# lane layout (start = lane*epl) 4-bit reads epl/2 bytes (2 codes each) and 3-bit
# reads one 3-byte group (8 codes) and shifts out its slice — a group of codes
# per load, never a byte read per element. `scale` is the vector's f16 amax
# scale. Requires start even (4-bit) and the run within one 8-code group (3-bit),
# both guaranteed by start = lane*(head_dim/32) for head_dim in {64,128,256}.
def _unpack_frag[bits: Int, epl: Int](
    packed: UnsafePointer[UInt8, MutAnyOrigin], start: Int, scale: Float32
) -> SIMD[DType.float32, epl]:
    comptime levels = (1 << (bits - 1)) - 1
    comptime bias = levels
    var out = SIMD[DType.float32, epl](0.0)
    comptime if bits == 4:
        base_byte = start // 2

        comptime for j in range(epl // 2):
            byte = packed[base_byte + j]
            c0 = Int32(byte & 0xF) - Int32(bias)
            c1 = Int32((byte >> 4) & 0xF) - Int32(bias)
            out[2 * j] = Float32(c0) * scale
            out[2 * j + 1] = Float32(c1) * scale
    else:
        g = start // 8
        sh = (start % 8) * 3
        acc = (
            UInt32(packed[g * 3 + 0])
            | (UInt32(packed[g * 3 + 1]) << 8)
            | (UInt32(packed[g * 3 + 2]) << 16)
        ) >> UInt32(sh)

        comptime for j in range(epl):
            c = Int32((acc >> UInt32(3 * j)) & 0x7) - Int32(bias)
            out[j] = Float32(c) * scale
    return out


# ---------------------------------------------------------------------------
# kv_pack_rot: rotate+quant+pack a batch of T tokens' K/V ([T, n_kv_heads,
# head_dim] f16, rope'd) into the paged rotational store at the ABSOLUTE
# positions carried in `positions` ([T] i32, one per token), AND write the
# rotated f16 vectors into the residual ring at pos % ring_slots. Reading the
# position from a device buffer (instead of a host scalar) keeps the launch
# position-independent so the whole rot decode step can be CUDA-graph captured
# and replayed across tokens. Grid (T, n_kv_heads); thread 0 of each block owns
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
    positions: UnsafePointer[Int32, MutAnyOrigin],
    n_kv_heads: Int,
    page_size: Int,
    ring_slots: Int,
):
    comptime packed_bytes = (head_dim * bits) // 8
    if Int(thread_idx.x) != 0:
        return
    tok = Int(block_idx.x)
    kvh = Int(block_idx.y)
    pos = Int(positions[tok])
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
# attn_decode_rot: split-K rotational decode attention over the dual-region
# store (packed older tokens + residual f16 ring for the recent window),
# mirroring attn_decode_split. Grid.z splits the context into `n_splits`
# contiguous chunks; each (seq, q-head, split) block runs n_warps warps that
# each stride a subset of its chunk with an online-softmax accumulator, then
# combines the warps and writes an UNNORMALIZED, still-ROTATED partial
# (acc, m, l) to `parts` ([n_seqs, n_q_heads, n_splits, head_dim + 2] f32).
# attn_decode_combine_rot merges the splits and inverse-rotates once.
#
# The whole score/accumulate is in rotated space:
#   * q is rotated once (R·q) with a block-cooperative Walsh-Hadamard butterfly,
#   * older positions dequant their low-bit code to the rotated-space value
#     (one grouped load per lane via _unpack_frag, contiguous lane layout),
#   * recent positions (pos >= ctx_len - ring_slots) read the rotated f16 ring
#     directly (no unpack, higher fidelity),
#   * scores use (R·q)·k_rot = q·k (minus quant error for the packed region),
#   * the rotated V accumulator is left un-inverted for the combine stage.
# `ring_slots == 0` degrades to the pure packed path (older == everything).
# Layouts:
#   parts:            [n_seqs, n_q_heads, n_splits, head_dim + 2] f32
#   q:                [n_seqs, n_q_heads, head_dim] f16
#   k_packed/v_packed:[n_pages, n_kv_heads, page_size, packed_bytes] u8
#   k_scale/v_scale:  [n_pages, n_kv_heads, page_size] f16
#   k_ring/v_ring:    [ring_slots, n_kv_heads, head_dim] f16 (rotated)
#   page_table:       [n_seqs, max_pages] i32; seq_lens: [n_seqs] i32
# Grid (n_seqs, n_q_heads, n_splits); block n_warps × 32 (block >= head_dim).
# Lane layout is CONTIGUOUS: lane owns head_dim indices [lane*epl, lane*epl+epl).
# ---------------------------------------------------------------------------
def attn_decode_rot[head_dim: Int, bits: Int](
    parts: UnsafePointer[Float32, MutAnyOrigin],
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
    n_splits: Int,
    ring_slots: Int,
    scale: Float32,
):
    comptime epl = head_dim // WARP_SIZE
    comptime packed_bytes = (head_dim * bits) // 8

    seq = Int(block_idx.x)
    qh = Int(block_idx.y)
    split = Int(block_idx.z)
    kvh = qh // (n_q_heads // n_kv_heads)
    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    wid = tid // WARP_SIZE
    nthr = Int(block_dim.x)
    n_warps = nthr // WARP_SIZE
    ctx_len = Int(seq_lens[seq])

    var ring_start = ctx_len - ring_slots
    if ring_start < 0:
        ring_start = 0
    chunk = (ctx_len + n_splits - 1) // n_splits
    start_ctx = split * chunk
    var end_ctx = start_ctx + chunk
    if end_ctx > ctx_len:
        end_ctx = ctx_len

    inv_rot = 1.0 / sqrt(Float32(head_dim))

    shared_q = stack_allocation[
        head_dim, Float32, address_space = AddressSpace.SHARED
    ]()

    # Rotate q once into shared with a block-cooperative WHT butterfly (all
    # threads share the work; barriers separate the log2(head_dim) stages).
    q_base = (seq * n_q_heads + qh) * head_dim
    var i = tid
    while i < head_dim:
        shared_q[i] = Float32(q[q_base + i])
        i += nthr
    barrier()
    var h = 1
    while h < head_dim:
        var bi = tid
        while bi < head_dim // 2:
            blk = bi // h
            j = blk * (h + h) + (bi % h)
            va = shared_q[j]
            vb = shared_q[j + h]
            shared_q[j] = va + vb
            shared_q[j + h] = va - vb
            bi += nthr
        barrier()
        h += h
    var jn = tid
    while jn < head_dim:
        shared_q[jn] = shared_q[jn] * inv_rot
        jn += nthr
    barrier()

    lane_off = lane * epl
    var q_frag = SIMD[DType.float32, epl](0.0)

    comptime for c in range(epl):
        q_frag[c] = shared_q[lane_off + c]

    var m: Float32 = NEG_INF
    var l: Float32 = 0.0
    var acc = SIMD[DType.float32, epl](0.0)

    var pos = start_ctx + wid
    while pos < end_ctx:
        var kf = SIMD[DType.float32, epl](0.0)
        var vf = SIMD[DType.float32, epl](0.0)
        if pos >= ring_start:
            rbase = ((pos % ring_slots) * n_kv_heads + kvh) * head_dim + lane_off
            kf = (k_ring + rbase).load[width=epl, alignment = 2 * epl]().cast[
                DType.float32
            ]()
            vf = (v_ring + rbase).load[width=epl, alignment = 2 * epl]().cast[
                DType.float32
            ]()
        else:
            page = Int(page_table[seq * max_pages + pos // page_size])
            slot = pos % page_size
            base_idx = (page * n_kv_heads + kvh) * page_size + slot
            ks = Float32(k_scale[base_idx])
            vs = Float32(v_scale[base_idx])
            kf = _unpack_frag[bits, epl](
                k_packed + base_idx * packed_bytes, lane_off, ks
            )
            vf = _unpack_frag[bits, epl](
                v_packed + base_idx * packed_bytes, lane_off, vs
            )

        var dot: Float32 = 0.0

        comptime for c in range(epl):
            dot += q_frag[c] * kf[c]
        score = warp.sum(dot) * scale

        var m_new = m
        if score > m_new:
            m_new = score
        factor = exp(m - m_new)
        p = exp(score - m_new)
        l = l * factor + p

        comptime for c in range(epl):
            acc[c] = acc[c] * factor + p * vf[c]
        m = m_new
        pos += n_warps

    # Cross-warp combine within the block, then write the rotated partial.
    shared_m = stack_allocation[MAX_WARPS, Float32, address_space = AddressSpace.SHARED]()
    shared_l = stack_allocation[MAX_WARPS, Float32, address_space = AddressSpace.SHARED]()
    shared_acc = stack_allocation[
        MAX_WARPS * head_dim, Float32, address_space = AddressSpace.SHARED
    ]()

    if lane == 0:
        shared_m[wid] = m
        shared_l[wid] = l

    comptime for c in range(epl):
        shared_acc[wid * head_dim + lane_off + c] = acc[c]
    barrier()

    if wid == 0:
        var m_star: Float32 = NEG_INF
        for w in range(n_warps):
            if shared_m[w] > m_star:
                m_star = shared_m[w]
        var l_star: Float32 = 0.0
        var out_frag = SIMD[DType.float32, epl](0.0)
        for w in range(n_warps):
            f = exp(shared_m[w] - m_star)
            l_star += shared_l[w] * f

            comptime for c in range(epl):
                out_frag[c] += shared_acc[w * head_dim + lane_off + c] * f

        parts_base = ((seq * n_q_heads + qh) * n_splits + split) * (head_dim + 2)

        comptime for c in range(epl):
            parts[parts_base + lane_off + c] = out_frag[c]
        if lane == 0:
            parts[parts_base + head_dim] = m_star
            parts[parts_base + head_dim + 1] = l_star


# ---------------------------------------------------------------------------
# attn_decode_combine_rot: merge attn_decode_rot's per-split rotated partials
# into the final head output. One warp per (seq, q-head) rescales every split to
# the block-global max (online-softmax), normalizes, and then inverse-rotates
# the merged rotated-space accumulator ONCE via a warp-cooperative WHT (R is
# self-inverse). Grid (n_seqs, n_q_heads); block 32.
# ---------------------------------------------------------------------------
def attn_decode_combine_rot[head_dim: Int](
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    parts: UnsafePointer[Float32, MutAnyOrigin],
    n_q_heads: Int,
    n_splits: Int,
):
    comptime epl = head_dim // WARP_SIZE

    seq = Int(block_idx.x)
    qh = Int(block_idx.y)
    lane = Int(thread_idx.x)
    lane_off = lane * epl
    head_base = (seq * n_q_heads + qh) * n_splits * (head_dim + 2)

    var m_star: Float32 = NEG_INF
    for s in range(n_splits):
        m_s = parts[head_base + s * (head_dim + 2) + head_dim]
        if m_s > m_star:
            m_star = m_s

    var l_total: Float32 = 0.0
    var out_frag = SIMD[DType.float32, epl](0.0)
    for s in range(n_splits):
        sbase = head_base + s * (head_dim + 2)
        f = exp(parts[sbase + head_dim] - m_star)
        l_total += parts[sbase + head_dim + 1] * f

        comptime for c in range(epl):
            out_frag[c] += parts[sbase + lane_off + c] * f
    inv_l = 1.0 / l_total

    # Stage the normalized rotated accumulator, inverse-rotate (WHT), store.
    shared = stack_allocation[
        head_dim, Float32, address_space = AddressSpace.SHARED
    ]()

    comptime for c in range(epl):
        shared[lane_off + c] = out_frag[c] * inv_l
    barrier()
    var h = 1
    while h < head_dim:
        var bi = lane
        while bi < head_dim // 2:
            blk = bi // h
            j = blk * (h + h) + (bi % h)
            va = shared[j]
            vb = shared[j + h]
            shared[j] = va + vb
            shared[j + h] = va - vb
            bi += WARP_SIZE
        barrier()
        h += h
    inv_rot = 1.0 / sqrt(Float32(head_dim))
    out_base = (seq * n_q_heads + qh) * head_dim

    comptime for c in range(epl):
        out_ptr[out_base + lane_off + c] = Float16(shared[lane_off + c] * inv_rot)


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
    lane_off = lane * epl
    ctx_len = base_pos + tok + 1

    shared_q = stack_allocation[
        head_dim, Float32, address_space = AddressSpace.SHARED
    ]()

    inv_rot = 1.0 / sqrt(Float32(head_dim))

    # Rotate q once with a warp-cooperative WHT (32 lanes share the butterflies).
    q_base = (tok * n_q_heads + qh) * head_dim
    var i = lane
    while i < head_dim:
        shared_q[i] = Float32(q[q_base + i])
        i += WARP_SIZE
    barrier()
    var h = 1
    while h < head_dim:
        var bi = lane
        while bi < head_dim // 2:
            blk = bi // h
            j = blk * (h + h) + (bi % h)
            va = shared_q[j]
            vb = shared_q[j + h]
            shared_q[j] = va + vb
            shared_q[j + h] = va - vb
            bi += WARP_SIZE
        barrier()
        h += h
    var jn = lane
    while jn < head_dim:
        shared_q[jn] = shared_q[jn] * inv_rot
        jn += WARP_SIZE
    barrier()

    var q_frag = SIMD[DType.float32, epl](0.0)

    comptime for c in range(epl):
        q_frag[c] = shared_q[lane_off + c]

    var m: Float32 = NEG_INF
    var l: Float32 = 0.0
    var acc = SIMD[DType.float32, epl](0.0)

    var pos = 0
    while pos < ctx_len:
        page = Int(page_table[pos // page_size])
        slot = pos % page_size
        base_idx = (page * n_kv_heads + kvh) * page_size + slot
        ks = Float32(k_scale[base_idx])
        vs = Float32(v_scale[base_idx])
        kf = _unpack_frag[bits, epl](
            k_packed + base_idx * packed_bytes, lane_off, ks
        )
        vf = _unpack_frag[bits, epl](
            v_packed + base_idx * packed_bytes, lane_off, vs
        )

        var dot: Float32 = 0.0

        comptime for c in range(epl):
            dot += q_frag[c] * kf[c]
        score = warp.sum(dot) * scale

        var m_new = m
        if score > m_new:
            m_new = score
        factor = exp(m - m_new)
        p = exp(score - m_new)
        l = l * factor + p

        comptime for c in range(epl):
            acc[c] = acc[c] * factor + p * vf[c]
        m = m_new
        pos += 1

    # Inverse-rotate the rotated accumulator (warp-cooperative WHT), normalize.
    comptime for c in range(epl):
        shared_q[lane_off + c] = acc[c]
    barrier()
    var h2 = 1
    while h2 < head_dim:
        var bi = lane
        while bi < head_dim // 2:
            blk = bi // h2
            j = blk * (h2 + h2) + (bi % h2)
            va = shared_q[j]
            vb = shared_q[j + h2]
            shared_q[j] = va + vb
            shared_q[j + h2] = va - vb
            bi += WARP_SIZE
        barrier()
        h2 += h2
    inv_l = 1.0 / l

    comptime for c in range(epl):
        out_ptr[q_base + lane_off + c] = Float16(shared_q[lane_off + c] * inv_rot * inv_l)


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

comptime attn_decode_combine_rot_hd64 = attn_decode_combine_rot[64]
comptime attn_decode_combine_rot_hd128 = attn_decode_combine_rot[128]

comptime attn_prefill_rot_hd64_b4 = attn_prefill_rot[64, 4]
comptime attn_prefill_rot_hd64_b3 = attn_prefill_rot[64, 3]
comptime attn_prefill_rot_hd128_b4 = attn_prefill_rot[128, 4]
comptime attn_prefill_rot_hd128_b3 = attn_prefill_rot[128, 3]
