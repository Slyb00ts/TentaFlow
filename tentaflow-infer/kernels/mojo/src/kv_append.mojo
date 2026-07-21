# ===== File: kv_append.mojo — scatter current-token K/V rows into the paged cache =====
# Replaces per-head D2D copies with one launch and, crucially, reads position
# and page id from device buffers — a decode step built from this kernel has
# no host-computed addresses, which is what CUDA-graph replay requires.

from std.gpu import block_dim, block_idx, thread_idx


def kv_append_f16(
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    seq_len: UnsafePointer[Int32, MutAnyOrigin],
    n_kv_heads: Int,
    page_size: Int,
    head_dim: Int,
):
    """Write k_in/v_in ([n_kv_heads, head_dim]) at position seq_len[0]-1.

    Cache layout matches attn_decode: [n_pages, n_kv_heads, page_size, head_dim].
    Grid.x = n_kv_heads; threads stride head_dim.
    """
    pos = Int(seq_len[0]) - 1
    page = Int(page_table[pos // page_size])
    slot = pos % page_size
    kvh = Int(block_idx.x)

    dst = ((page * n_kv_heads + kvh) * page_size + slot) * head_dim
    src = kvh * head_dim

    var i = Int(thread_idx.x)
    while i < head_dim:
        k_cache[dst + i] = k_in[src + i]
        v_cache[dst + i] = v_in[src + i]
        i += Int(block_dim.x)
