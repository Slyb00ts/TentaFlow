# ===== File: attention.mojo — paged flash-decode attention (GQA/MQA/MHA) =====
# Decode-step attention over a paged KV cache with online softmax. One block
# per (sequence, q-head); each warp owns a strided subset of context positions
# and keeps a distributed accumulator (head_dim spread across lanes), so no
# score matrix is ever materialized. head_dim is a compile-time parameter —
# specializations are registered in build_kernels.mojo per supported size.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.primitives import warp
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import exp

comptime WARP_SIZE = 32
comptime MAX_WARPS = 8
comptime NEG_INF: Float32 = -1e30


def attn_decode_f16[head_dim: Int](
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    q: UnsafePointer[Float16, MutAnyOrigin],
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    seq_lens: UnsafePointer[Int32, MutAnyOrigin],
    n_q_heads: Int,
    n_kv_heads: Int,
    page_size: Int,
    max_pages: Int,
    scale: Float32,
):
    """out[seq, qh] = softmax(q[seq, qh] · K^T * scale) · V.

    Layouts:
      q/out:            [n_seqs, n_q_heads, head_dim]
      k_cache/v_cache:  [n_pages, n_kv_heads, page_size, head_dim]
      page_table:       [n_seqs, max_pages] (i32 physical page ids)
      seq_lens:         [n_seqs] context length including the current token
    Grid: (n_seqs, n_q_heads); block: n_warps × 32 threads.
    """
    comptime epl = head_dim // WARP_SIZE  # elements per lane

    seq = Int(block_idx.x)
    qh = Int(block_idx.y)
    kvh = qh // (n_q_heads // n_kv_heads)
    lane = Int(thread_idx.x) % WARP_SIZE
    wid = Int(thread_idx.x) // WARP_SIZE
    n_warps = Int(block_dim.x) // WARP_SIZE
    ctx_len = Int(seq_lens[seq])

    # Per-lane q fragment: element e lives at lane + e*32 for coalesced loads.
    q_base = (seq * n_q_heads + qh) * head_dim
    var q_frag = SIMD[DType.float32, epl](0.0)

    comptime for e in range(epl):
        q_frag[e] = Float32(q[q_base + e * WARP_SIZE + lane])

    var m: Float32 = NEG_INF
    var l: Float32 = 0.0
    var acc = SIMD[DType.float32, epl](0.0)

    var pos = wid
    while pos < ctx_len:
        page = Int(page_table[seq * max_pages + pos // page_size])
        kv_base = ((page * n_kv_heads + kvh) * page_size + (pos % page_size)) * head_dim

        # Warp-cooperative dot(q, k_pos): lane partials reduced by shuffle.
        var dot: Float32 = 0.0

        comptime for e in range(epl):
            dot += q_frag[e] * Float32(k_cache[kv_base + e * WARP_SIZE + lane])
        score = warp.sum(dot) * scale

        # Online softmax update; every lane holds identical m/l after warp.sum.
        var m_new = m
        if score > m_new:
            m_new = score
        factor = exp(m - m_new)
        p = exp(score - m_new)
        l = l * factor + p

        comptime for e in range(epl):
            acc[e] = acc[e] * factor + p * Float32(v_cache[kv_base + e * WARP_SIZE + lane])
        m = m_new

        pos += n_warps

    # Cross-warp combine via shared staging: rescale every warp's partial
    # accumulator to the block-global max before summing.
    shared_m = stack_allocation[MAX_WARPS, Float32, address_space = AddressSpace.SHARED]()
    shared_l = stack_allocation[MAX_WARPS, Float32, address_space = AddressSpace.SHARED]()
    shared_acc = stack_allocation[
        MAX_WARPS * head_dim, Float32, address_space = AddressSpace.SHARED
    ]()

    if lane == 0:
        shared_m[wid] = m
        shared_l[wid] = l

    comptime for e in range(epl):
        shared_acc[wid * head_dim + e * WARP_SIZE + lane] = acc[e]
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

            comptime for e in range(epl):
                out_frag[e] += shared_acc[w * head_dim + e * WARP_SIZE + lane] * f
        inv_l = 1.0 / l_star

        comptime for e in range(epl):
            out_ptr[q_base + e * WARP_SIZE + lane] = Float16(out_frag[e] * inv_l)


comptime attn_decode_f16_hd64 = attn_decode_f16[64]
comptime attn_decode_f16_hd128 = attn_decode_f16[128]
