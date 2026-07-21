# =============================================================================
# Plik: attention_gqa.mojo
# Opis: Split attention współdzielący K/V między czterema głowicami GQA.
# Przykład: siatka [sekwencja, głowica KV, split], blok 256 wątków.
# =============================================================================

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.primitives import warp
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import exp, cos, sin, pow

comptime WARP = 32
comptime MAX_WARPS = 8
comptime GROUP = 4
comptime HD = 128
comptime EPL = HD // WARP
comptime NEG_INF: Float32 = -1e30


def attn_decode_split_gqa4_f16_hd128(
    parts: UnsafePointer[Float32, MutAnyOrigin],
    q_in: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    q_norm_w: UnsafePointer[Float16, MutAnyOrigin],
    k_norm_w: UnsafePointer[Float16, MutAnyOrigin],
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    seq_lens: UnsafePointer[Int32, MutAnyOrigin],
    positions: UnsafePointer[Int32, MutAnyOrigin],
    n_q_heads: Int,
    n_kv_heads: Int,
    page_size: Int,
    max_pages: Int,
    n_splits: Int,
    has_q_norm: Int,
    has_k_norm: Int,
    eps: Float32,
    theta_base: Float32,
    scale: Float32,
):
    """Liczy cztery głowice Q współdzielące jeden strumień K/V."""
    seq = Int(block_idx.x)
    kvh = Int(block_idx.y)
    split = Int(block_idx.z)
    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    n_warps = Int(block_dim.x) // WARP
    ctx_len = Int(seq_lens[seq])
    chunk = (ctx_len + n_splits - 1) // n_splits
    start = split * chunk
    var end = start + chunk
    if end > ctx_len:
        end = ctx_len

    staged_q = stack_allocation[
        GROUP * HD, Float16, address_space = AddressSpace.SHARED
    ]()
    staged_k = stack_allocation[HD, Float16, address_space = AddressSpace.SHARED]()

    # Pierwszy prototyp jest routowany wyłącznie dla modeli bez Q/K norm.
    if tid < HD:
        for g in range(GROUP):
            qh = kvh * GROUP + g
            staged_q[g * HD + tid] = q_in[(seq * n_q_heads + qh) * HD + tid]
        staged_k[tid] = k_in[(seq * n_kv_heads + kvh) * HD + tid]
    barrier()

    if tid < HD // 2:
        freq = pow(theta_base, Float32(-2 * tid) / Float32(HD))
        angle = Float32(positions[seq]) * freq
        c = cos(angle)
        s = sin(angle)
        for g in range(GROUP):
            base = g * HD
            a = Float32(staged_q[base + tid])
            b = Float32(staged_q[base + HD // 2 + tid])
            staged_q[base + tid] = Float16(a * c - b * s)
            staged_q[base + HD // 2 + tid] = Float16(a * s + b * c)

        pos_cur = ctx_len - 1
        page_cur = Int(page_table[seq * max_pages + pos_cur // page_size])
        dst = ((page_cur * n_kv_heads + kvh) * page_size + pos_cur % page_size) * HD
        ak = Float32(staged_k[tid])
        bk = Float32(staged_k[HD // 2 + tid])
        k_cache[dst + tid] = Float16(ak * c - bk * s)
        k_cache[dst + HD // 2 + tid] = Float16(ak * s + bk * c)
    pos_cur = ctx_len - 1
    page_cur = Int(page_table[seq * max_pages + pos_cur // page_size])
    dst = ((page_cur * n_kv_heads + kvh) * page_size + pos_cur % page_size) * HD
    if tid < HD:
        v_cache[dst + tid] = v_in[(seq * n_kv_heads + kvh) * HD + tid]
    barrier()

    var qfrag = SIMD[DType.float32, GROUP * EPL](0.0)
    comptime for g in range(GROUP):
        comptime for e in range(EPL):
            qfrag[g * EPL + e] = Float32(staged_q[g * HD + e * WARP + lane])

    var m = SIMD[DType.float32, GROUP](NEG_INF)
    var l = SIMD[DType.float32, GROUP](0.0)
    var acc = SIMD[DType.float32, GROUP * EPL](0.0)
    var pos = start + wid
    while pos < end:
        page = Int(page_table[seq * max_pages + pos // page_size])
        kv_base = ((page * n_kv_heads + kvh) * page_size + pos % page_size) * HD
        var kv = SIMD[DType.float32, EPL](0.0)
        var vv = SIMD[DType.float32, EPL](0.0)
        comptime for e in range(EPL):
            kv[e] = Float32(k_cache[kv_base + e * WARP + lane])
            vv[e] = Float32(v_cache[kv_base + e * WARP + lane])

        comptime for g in range(GROUP):
            dot = (qfrag.slice[EPL, offset=g * EPL]() * kv).reduce_add()
            score = warp.sum(dot) * scale
            var m_new = m[g]
            if score > m_new:
                m_new = score
            factor = exp(m[g] - m_new)
            prob = exp(score - m_new)
            l[g] = l[g] * factor + prob
            comptime for e in range(EPL):
                idx = g * EPL + e
                acc[idx] = acc[idx] * factor + prob * vv[e]
            m[g] = m_new
        pos += n_warps

    shared_m = stack_allocation[
        MAX_WARPS * GROUP, Float32, address_space = AddressSpace.SHARED
    ]()
    shared_l = stack_allocation[
        MAX_WARPS * GROUP, Float32, address_space = AddressSpace.SHARED
    ]()
    shared_acc = stack_allocation[
        MAX_WARPS * GROUP * HD, Float32, address_space = AddressSpace.SHARED
    ]()
    comptime for g in range(GROUP):
        if lane == 0:
            shared_m[wid * GROUP + g] = m[g]
            shared_l[wid * GROUP + g] = l[g]
        comptime for e in range(EPL):
            shared_acc[(wid * GROUP + g) * HD + e * WARP + lane] = acc[g * EPL + e]
    barrier()

    if wid == 0:
        comptime for g in range(GROUP):
            var m_star: Float32 = NEG_INF
            for w in range(n_warps):
                if shared_m[w * GROUP + g] > m_star:
                    m_star = shared_m[w * GROUP + g]
            var l_star: Float32 = 0.0
            var outfrag = SIMD[DType.float32, EPL](0.0)
            for w in range(n_warps):
                f = exp(shared_m[w * GROUP + g] - m_star)
                l_star += shared_l[w * GROUP + g] * f
                comptime for e in range(EPL):
                    outfrag[e] += shared_acc[(w * GROUP + g) * HD + e * WARP + lane] * f
            qh = kvh * GROUP + g
            base = ((seq * n_q_heads + qh) * n_splits + split) * (HD + 2)
            comptime for e in range(EPL):
                parts[base + e * WARP + lane] = outfrag[e]
            if lane == 0:
                parts[base + HD] = m_star
                parts[base + HD + 1] = l_star
