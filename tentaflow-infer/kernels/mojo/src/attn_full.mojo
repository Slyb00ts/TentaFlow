# ===== File: attn_full.mojo — full (non-paged) attention: encoder self-attn, prefill =====
# Contiguous K/V variant of the flash-decode kernel: one block per (query
# position, q-head), warps stride over key positions with online softmax and a
# lane-distributed accumulator. `causal` masks future positions, so the same
# kernel serves bidirectional encoders (causal=0) and LLM/decoder prefill
# (causal=1, with q_offset aligning query rows to absolute positions).

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.primitives import warp
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import exp

comptime WARP_SIZE = 32
comptime MAX_WARPS = 8
comptime NEG_INF: Float32 = -1e30


def attn_full_f16[head_dim: Int](
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    q: UnsafePointer[Float16, MutAnyOrigin],
    k: UnsafePointer[Float16, MutAnyOrigin],
    v: UnsafePointer[Float16, MutAnyOrigin],
    n_q_heads: Int,
    n_kv_heads: Int,
    n_kv: Int,
    causal: Int,
    q_offset: Int,
    scale: Float32,
):
    """out[t, h] = softmax(q[t, h] · K^T * scale) · V.

    Layouts: q/out [n_q, n_q_heads, head_dim]; k/v [n_kv, n_kv_heads, head_dim].
    Causal masking admits key positions <= q_offset + t. Grid: (n_q, n_q_heads).
    """
    comptime epl = head_dim // WARP_SIZE

    t = Int(block_idx.x)
    qh = Int(block_idx.y)
    kvh = qh // (n_q_heads // n_kv_heads)
    lane = Int(thread_idx.x) % WARP_SIZE
    wid = Int(thread_idx.x) // WARP_SIZE
    n_warps = Int(block_dim.x) // WARP_SIZE

    var limit = n_kv
    if causal != 0:
        limit = q_offset + t + 1
        if limit > n_kv:
            limit = n_kv

    q_base = (t * n_q_heads + qh) * head_dim
    var q_frag = SIMD[DType.float32, epl](0.0)

    comptime for e in range(epl):
        q_frag[e] = Float32(q[q_base + e * WARP_SIZE + lane])

    var m: Float32 = NEG_INF
    var l: Float32 = 0.0
    var acc = SIMD[DType.float32, epl](0.0)

    var pos = wid
    while pos < limit:
        kv_base = (pos * n_kv_heads + kvh) * head_dim

        var dot: Float32 = 0.0

        comptime for e in range(epl):
            dot += q_frag[e] * Float32(k[kv_base + e * WARP_SIZE + lane])
        score = warp.sum(dot) * scale

        var m_new = m
        if score > m_new:
            m_new = score
        factor = exp(m - m_new)
        p = exp(score - m_new)
        l = l * factor + p

        comptime for e in range(epl):
            acc[e] = acc[e] * factor + p * Float32(v[kv_base + e * WARP_SIZE + lane])
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


comptime attn_full_f16_hd64 = attn_full_f16[64]
comptime attn_full_f16_hd128 = attn_full_f16[128]
