# ===== File: prefill.mojo — batched KV append + causal prefill attention (paged) =====

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.primitives import warp
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import exp
from src.kv_fp8 import kv_row8_f16

comptime WARP_SIZE = 32
comptime MAX_WARPS = 8
comptime NEG_INF: Float32 = -1e30


def kv_append_batch[kv_dtype: DType](
    k_cache: UnsafePointer[Scalar[kv_dtype], MutAnyOrigin],
    v_cache: UnsafePointer[Scalar[kv_dtype], MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    base_pos: Int,
    n_kv_heads: Int,
    page_size: Int,
    head_dim: Int,
):
    """Scatter T tokens' K/V ([T, n_kv_heads, head_dim]) at positions
    base_pos..base_pos+T. Grid: (T, n_kv_heads). kv_dtype = float16 stores
    the f16 rows verbatim; float8_e4m3fn casts per value (RN, satfinite) —
    the scale-free e4m3 range (±448, min denormal 2^-9) covers post-norm
    K/V magnitudes."""
    tok = Int(block_idx.x)
    kvh = Int(block_idx.y)
    pos = base_pos + tok
    page = Int(page_table[pos // page_size])
    slot = pos % page_size

    dst = ((page * n_kv_heads + kvh) * page_size + slot) * head_dim
    src = (tok * n_kv_heads + kvh) * head_dim

    var i = Int(thread_idx.x)
    while i < head_dim:
        k_cache[dst + i] = Scalar[kv_dtype](Float32(k_in[src + i]))
        v_cache[dst + i] = Scalar[kv_dtype](Float32(v_in[src + i]))
        i += Int(block_dim.x)


comptime kv_append_batch_f16 = kv_append_batch[DType.float16]
comptime kv_append_batch_fp8 = kv_append_batch[DType.float8_e4m3fn]


comptime QT = 16  # query tokens per block
comptime PT = WARP_SIZE  # cached positions per smem tile (one lane per position)
comptime QPW = QT // MAX_WARPS  # queries owned by one warp


def attn_prefill[head_dim: Int, kv_dtype: DType](
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    q: UnsafePointer[Float16, MutAnyOrigin],
    k_cache: UnsafePointer[Scalar[kv_dtype], MutAnyOrigin],
    v_cache: UnsafePointer[Scalar[kv_dtype], MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    base_pos: Int,
    n_q_heads: Int,
    n_kv_heads: Int,
    page_size: Int,
    scale: Float32,
    n_tokens: Int,
):
    """Causal attention for a prefill chunk over the paged cache.

    q/out: [T, n_q_heads, head_dim]; query token tok attends positions
    0..base_pos+tok (its K/V must already be in the cache).
    Grid: (ceil(T/QT), heads), block 256. K/V stream through shared-memory
    tiles of PT positions, so each cache byte is fetched once per QT queries
    instead of once per query. Each warp owns QPW queries; per tile, lane j
    scores position j (K rows XOR-swizzled per 16-byte chunk so lane-strided
    row reads are bank-conflict-free), the softmax exp runs 32-wide, the
    online-softmax rescale happens once per TILE (not per position), and the
    P·V accumulation broadcasts p_j via warp shuffle with lanes splitting the
    head dimension.

    kv_dtype = float8_e4m3fn reads an FP8 cache: rows are widened to f16 in
    the shared-memory tiles (e4m3 values are exactly representable in f16),
    so the attention arithmetic is bit-identical to the f16 kernel run on a
    dequantized cache."""
    comptime epl = head_dim // WARP_SIZE
    comptime row_chunks = head_dim // 8
    comptime tile_chunks = PT * row_chunks
    comptime block_threads = MAX_WARPS * WARP_SIZE
    comptime chunks_per_thread = tile_chunks // block_threads
    comptime q_chunks = QT * row_chunks

    tok0 = Int(block_idx.x) * QT
    qh = Int(block_idx.y)
    kvh = qh // (n_q_heads // n_kv_heads)
    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    wid = tid // WARP_SIZE

    qs = stack_allocation[
        QT * head_dim, Float16, address_space = AddressSpace.SHARED
    ]()
    ks = stack_allocation[
        PT * head_dim, Float16, address_space = AddressSpace.SHARED
    ]()
    vs = stack_allocation[
        PT * head_dim, Float16, address_space = AddressSpace.SHARED
    ]()

    # Stage the block's Q tile once; rows are read chunk-broadcast (all lanes
    # of a warp hit the same address), so no swizzle is needed. Out-of-range
    # token rows duplicate the last row — their queries are masked off below.
    comptime for it in range((q_chunks + block_threads - 1) // block_threads):
        c = tid + it * block_threads
        if c < q_chunks:
            row = c // row_chunks
            off = (c % row_chunks) * 8
            var tq = tok0 + row
            if tq > n_tokens - 1:
                tq = n_tokens - 1
            (qs + row * head_dim + off).store[width=8, alignment=16](
                (q + (tq * n_q_heads + qh) * head_dim + off).load[
                    width=8, alignment=16
                ]()
            )

    var m = InlineArray[Float32, QPW](fill=NEG_INF)
    var l = InlineArray[Float32, QPW](fill=0.0)
    var acc = InlineArray[SIMD[DType.float32, epl], QPW](
        fill=SIMD[DType.float32, epl](0.0)
    )

    # Highest position any query in this block attends.
    var tok_hi = tok0 + QT
    if tok_hi > n_tokens:
        tok_hi = n_tokens
    max_abs = base_pos + tok_hi - 1

    var pos0 = 0
    while pos0 <= max_abs:
        var n_valid = max_abs + 1 - pos0
        if n_valid > PT:
            n_valid = PT
        barrier()

        comptime for it in range(chunks_per_thread):
            c = tid + it * block_threads
            row = c // row_chunks
            off = c % row_chunks
            if row < n_valid:
                pos = pos0 + row
                page = Int(page_table[pos // page_size])
                kv_base = (
                    (page * n_kv_heads + kvh) * page_size + pos % page_size
                ) * head_dim + off * 8
                (ks + row * head_dim + ((off ^ (row % row_chunks)) * 8)).store[
                    width=8, alignment=16
                ](kv_row8_f16[kv_dtype](k_cache + kv_base))
                (vs + row * head_dim + off * 8).store[width=8, alignment=16](
                    kv_row8_f16[kv_dtype](v_cache + kv_base)
                )
        barrier()

        comptime for i in range(QPW):
            tq = tok0 + wid * QPW + i
            var h = base_pos + tq - pos0 + 1
            if tq >= n_tokens:
                h = 0
            if h > n_valid:
                h = n_valid
            if h > 0:
                # Lane `lane` scores position pos0+lane against query i.
                var dotv = SIMD[DType.float32, 8](0.0)

                comptime for c in range(row_chunks):
                    kv8 = (
                        ks
                        + lane * head_dim
                        + ((c ^ (lane % row_chunks)) * 8)
                    ).load[width=8, alignment=16]().cast[DType.float32]()
                    qv8 = (
                        qs + (wid * QPW + i) * head_dim + c * 8
                    ).load[width=8, alignment=16]().cast[DType.float32]()
                    dotv += qv8 * kv8
                var score = dotv.reduce_add() * scale
                if lane >= h:
                    score = NEG_INF
                mtile = warp.max(score)
                if mtile > m[i]:
                    rescale = exp(m[i] - mtile)
                    l[i] *= rescale
                    acc[i] = acc[i] * rescale
                    m[i] = mtile
                p = exp(score - m[i])
                l[i] += warp.sum(p)

                var jj = 0
                while jj < h:
                    pj = warp.shuffle_idx(p, UInt32(jj))
                    vfj = (vs + jj * head_dim + lane * epl).load[
                        width=epl, alignment = epl * 2
                    ]().cast[DType.float32]()
                    acc[i] += vfj * pj
                    jj += 1

        pos0 += PT

    comptime for i in range(QPW):
        tq = tok0 + wid * QPW + i
        if tq < n_tokens:
            q_base = (tq * n_q_heads + qh) * head_dim
            inv_l = 1.0 / l[i]
            (out_ptr + q_base + lane * epl).store[width=epl](
                (acc[i] * inv_l).cast[DType.float16]()
            )


comptime attn_prefill_f16_hd64 = attn_prefill[64, DType.float16]
comptime attn_prefill_f16_hd128 = attn_prefill[128, DType.float16]
comptime attn_prefill_f16_hd256 = attn_prefill[256, DType.float16]
comptime attn_prefill_fp8_hd64 = attn_prefill[64, DType.float8_e4m3fn]
comptime attn_prefill_fp8_hd128 = attn_prefill[128, DType.float8_e4m3fn]
