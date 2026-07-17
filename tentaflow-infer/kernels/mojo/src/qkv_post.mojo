# ===== File: qkv_post.mojo — fused decode QKV post-processing =====
# Everything between the QKV GEMV and attention in ONE launch: optional
# per-head q/k RMSNorm, neox RoPE on q and k, and the paged-cache append of
# k/v — replacing five small kernels per layer on the decode path. All
# addressing state (position, page id) comes from device buffers, so the
# launch is CUDA-graph-replayable.
#
# Grid: n_heads + n_kv_heads blocks; block = head_dim threads (one element
# per thread, head_dim <= 256 and a multiple of 32). Blocks [0, n_heads)
# process q heads in place; blocks [n_heads, n_heads+n_kv_heads) process one
# kv head: k is normed+rotated straight into k_cache, v is copied verbatim.
#
# Bit-exactness contract: values are rounded to f16 between the norm and the
# rope stages (staged in shared memory), reproducing the separate-kernel
# dataflow (rmsnorm_f16 store -> rope_neox_f16 load) exactly.

from std.gpu import block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import rsqrt, cos, sin, pow
from src.reduce import block_reduce_sum

comptime MAX_HEAD_DIM = 256


def qkv_post_f16(
    q_io: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    q_norm_w: UnsafePointer[Float16, MutAnyOrigin],
    k_norm_w: UnsafePointer[Float16, MutAnyOrigin],
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    positions: UnsafePointer[Int32, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    seq_len: UnsafePointer[Int32, MutAnyOrigin],
    n_heads: Int,
    n_kv_heads: Int,
    head_dim: Int,
    page_size: Int,
    has_q_norm: Int,
    has_k_norm: Int,
    eps: Float32,
    theta_base: Float32,
):
    """Fused rmsnorm(q/k) + rope(q/k) + kv_append for the single-token step.

    Layouts match the separate kernels: q_io/k_in/v_in are [heads, head_dim]
    rows (sections of a fused qkv buffer or separate buffers), k_cache/v_cache
    are [n_pages, n_kv_heads, page_size, head_dim]. The current position is
    seq_len[0]-1; the rope angle uses positions[0].
    """
    blk = Int(block_idx.x)
    tid = Int(thread_idx.x)
    half = head_dim // 2
    staged = stack_allocation[
        MAX_HEAD_DIM, Float16, address_space = AddressSpace.SHARED
    ]()

    if blk < n_heads:
        base = blk * head_dim
        raw = Float32(q_io[base + tid])
        var val = raw
        if has_q_norm == 1:
            total = block_reduce_sum(raw * raw)
            inv = rsqrt(total / Float32(head_dim) + eps)
            val = raw * inv * Float32(q_norm_w[tid])
        staged[tid] = Float16(val)
        barrier()
        if tid < half:
            freq = pow(theta_base, Float32(-2 * tid) / Float32(head_dim))
            angle = Float32(positions[0]) * freq
            c = cos(angle)
            s = sin(angle)
            a = Float32(staged[tid])
            b = Float32(staged[half + tid])
            q_io[base + tid] = Float16(a * c - b * s)
            q_io[base + half + tid] = Float16(a * s + b * c)
    else:
        kvh = blk - n_heads
        base = kvh * head_dim
        raw = Float32(k_in[base + tid])
        var val = raw
        if has_k_norm == 1:
            total = block_reduce_sum(raw * raw)
            inv = rsqrt(total / Float32(head_dim) + eps)
            val = raw * inv * Float32(k_norm_w[tid])
        staged[tid] = Float16(val)
        barrier()

        pos = Int(seq_len[0]) - 1
        page = Int(page_table[pos // page_size])
        slot = pos % page_size
        dst = ((page * n_kv_heads + kvh) * page_size + slot) * head_dim
        if tid < half:
            freq = pow(theta_base, Float32(-2 * tid) / Float32(head_dim))
            angle = Float32(positions[0]) * freq
            c = cos(angle)
            s = sin(angle)
            a = Float32(staged[tid])
            b = Float32(staged[half + tid])
            k_cache[dst + tid] = Float16(a * c - b * s)
            k_cache[dst + half + tid] = Float16(a * s + b * c)
        v_cache[dst + tid] = v_in[base + tid]
