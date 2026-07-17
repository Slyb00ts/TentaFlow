# ===== File: prefill.mojo — batched KV append + causal prefill attention (paged) =====

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.primitives import warp
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import exp

comptime WARP_SIZE = 32
comptime MAX_WARPS = 8
comptime NEG_INF: Float32 = -1e30


def kv_append_batch_f16(
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    base_pos: Int,
    n_kv_heads: Int,
    page_size: Int,
    head_dim: Int,
):
    """Scatter T tokens' K/V ([T, n_kv_heads, head_dim]) at positions
    base_pos..base_pos+T. Grid: (T, n_kv_heads)."""
    tok = Int(block_idx.x)
    kvh = Int(block_idx.y)
    pos = base_pos + tok
    page = Int(page_table[pos // page_size])
    slot = pos % page_size

    dst = ((page * n_kv_heads + kvh) * page_size + slot) * head_dim
    src = (tok * n_kv_heads + kvh) * head_dim

    var i = Int(thread_idx.x)
    while i < head_dim:
        k_cache[dst + i] = k_in[src + i]
        v_cache[dst + i] = v_in[src + i]
        i += Int(block_dim.x)


def attn_prefill_f16[head_dim: Int](
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    q: UnsafePointer[Float16, MutAnyOrigin],
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    base_pos: Int,
    n_q_heads: Int,
    n_kv_heads: Int,
    page_size: Int,
    scale: Float32,
):
    """Causal attention for a prefill chunk over the paged cache.

    q/out: [T, n_q_heads, head_dim]; query token tok attends positions
    0..base_pos+tok (its K/V must already be in the cache). Grid: (T, heads).
    """
    comptime epl = head_dim // WARP_SIZE

    tok = Int(block_idx.x)
    qh = Int(block_idx.y)
    kvh = qh // (n_q_heads // n_kv_heads)
    lane = Int(thread_idx.x) % WARP_SIZE
    wid = Int(thread_idx.x) // WARP_SIZE
    n_warps = Int(block_dim.x) // WARP_SIZE
    ctx_len = base_pos + tok + 1

    q_base = (tok * n_q_heads + qh) * head_dim
    var q_frag = SIMD[DType.float32, epl](0.0)

    comptime for e in range(epl):
        q_frag[e] = Float32(q[q_base + e * WARP_SIZE + lane])

    var m: Float32 = NEG_INF
    var l: Float32 = 0.0
    var acc = SIMD[DType.float32, epl](0.0)

    var pos = wid
    while pos < ctx_len:
        page = Int(page_table[pos // page_size])
        kv_base = ((page * n_kv_heads + kvh) * page_size + (pos % page_size)) * head_dim

        var dot: Float32 = 0.0

        comptime for e in range(epl):
            dot += q_frag[e] * Float32(k_cache[kv_base + e * WARP_SIZE + lane])
        score = warp.sum(dot) * scale

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


comptime attn_prefill_f16_hd64 = attn_prefill_f16[64]
comptime attn_prefill_f16_hd128 = attn_prefill_f16[128]
