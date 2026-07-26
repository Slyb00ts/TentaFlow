# ===== File: attention.mojo — paged flash-decode attention (GQA/MQA/MHA) =====
# Decode-step attention over a paged KV cache with online softmax. One block
# per (sequence, q-head); each warp owns a strided subset of context positions
# and keeps a distributed accumulator (head_dim spread across lanes), so no
# score matrix is ever materialized. head_dim is a compile-time parameter —
# specializations are registered in build_kernels.mojo per supported size.

from std.gpu import WARP_SIZE, block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.primitives import warp
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import exp, rsqrt, cos, sin, pow
from src.reduce import block_reduce_sum
from src.kv_fp8 import kv_frag_f32

comptime MAX_WARPS = 8
comptime NEG_INF: Float32 = -1e30
comptime HD256 = 256
comptime HD256_ELEMENTS_PER_LANE = 8
comptime HD256_SPLITS = 8
comptime HD256_PARTIAL_STRIDE = 260


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
    window: Int,
):
    """out[seq, qh] = softmax(q[seq, qh] · K^T * scale) · V.

    `window` > 0 włącza okno przesuwne. W decode zapytanie siedzi na ostatniej
    pozycji, więc okno sprowadza się do PRZESUNIĘCIA STARTU skanu — pozycje
    starsze niż `ctx_len - window` są pomijane, a nie maskowane, więc kernel
    robi wtedy mniej pracy, nie więcej.

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

    var first = 0
    if window > 0 and ctx_len > window:
        first = ctx_len - window
    var pos = first + wid
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
comptime attn_decode_f16_hd256 = attn_decode_f16[256]
# head_dim 512 — warstwy globalne Gemmy 4 (16 głowic Q na jedną głowicę KV).
comptime attn_decode_f16_hd512 = attn_decode_f16[512]


def attn_decode_split8_f16[head_dim: Int](
    partial_ptr: UnsafePointer[Float32, MutAnyOrigin],
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
    window: Int,
):
    """Dekodowanie z podzialem kontekstu na HD256_SPLITS partycji.

    Wariant generyczny ma siatke (sekwencje, glowice), czyli przy JEDNEJ
    sekwencji 16 grup roboczych na 80 CU — profiler pokazal 50 GB/s na
    odczytach KV, gdy GEMV wag wyciaga 397 GB/s. Podzial kontekstu mnozy
    liczbe grup przez HD256_SPLITS i dopiero to wysyca pamiec.

    `window` > 0 ogranicza widoczne pozycje do `[ctx - window, ctx)` (warstwy
    okienne Gemmy 4). 0 oznacza pelny kontekst.
    """
    comptime epl = head_dim // WARP_SIZE
    seq = Int(block_idx.x)
    query_head = Int(block_idx.y)
    partition = Int(block_idx.z)
    kv_head = query_head // (n_q_heads // n_kv_heads)
    lane = Int(thread_idx.x) % WARP_SIZE
    warp_id = Int(thread_idx.x) // WARP_SIZE
    warps = Int(block_dim.x) // WARP_SIZE
    context = Int(seq_lens[seq])
    var first = 0
    if window > 0 and context > window:
        first = context - window
    query_base = (seq * n_q_heads + query_head) * head_dim
    lane_element = lane * epl
    query = (q + query_base + lane_element).load[
        width=epl, alignment=16
    ]().cast[DType.float32]()

    var maximum: Float32 = NEG_INF
    var denominator: Float32 = 0.0
    var output = SIMD[DType.float32, epl](0.0)
    var position = first + partition + warp_id * HD256_SPLITS
    while position < context:
        page = Int(page_table[seq * max_pages + position // page_size])
        cache_base = (
            (page * n_kv_heads + kv_head) * page_size + position % page_size
        ) * head_dim + lane_element
        key = (k_cache + cache_base).load[
            width=epl, alignment=16
        ]().cast[DType.float32]()
        value = (v_cache + cache_base).load[
            width=epl, alignment=16
        ]().cast[DType.float32]()
        score = warp.sum((query * key).reduce_add()) * scale
        next_maximum = max(maximum, score)
        correction = exp(maximum - next_maximum)
        probability = exp(score - next_maximum)
        denominator = denominator * correction + probability
        output = output * correction + value * probability
        maximum = next_maximum
        position += warps * HD256_SPLITS

    shared_maximum = stack_allocation[
        MAX_WARPS, Float32, address_space=AddressSpace.SHARED
    ]()
    shared_denominator = stack_allocation[
        MAX_WARPS, Float32, address_space=AddressSpace.SHARED
    ]()
    shared_output = stack_allocation[
        MAX_WARPS * head_dim, Float32, address_space=AddressSpace.SHARED
    ]()
    if lane == 0:
        shared_maximum[warp_id] = maximum
        shared_denominator[warp_id] = denominator
    (shared_output + warp_id * head_dim + lane_element).store[
        width=epl, alignment=16
    ](output)
    barrier()

    if warp_id == 0:
        var block_maximum: Float32 = NEG_INF
        for index in range(warps):
            block_maximum = max(block_maximum, shared_maximum[index])
        var block_denominator: Float32 = 0.0
        var block_output = SIMD[DType.float32, epl](0.0)
        for index in range(warps):
            factor = exp(shared_maximum[index] - block_maximum)
            block_denominator += shared_denominator[index] * factor
            partial = (shared_output + index * head_dim + lane_element).load[
                width=epl, alignment=16
            ]()
            block_output += partial * factor
        partial_base = (
            (seq * n_q_heads + query_head) * HD256_SPLITS + partition
        ) * (head_dim + 4)
        (partial_ptr + partial_base + lane_element).store[
            width=epl, alignment=16
        ](block_output)
        if lane == 0:
            partial_ptr[partial_base + head_dim] = block_maximum
            partial_ptr[partial_base + head_dim + 1] = block_denominator


def attn_decode_split8_combine_f16[head_dim: Int](
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    partial_ptr: UnsafePointer[Float32, MutAnyOrigin],
    n_q_heads: Int,
):
    comptime epl = head_dim // WARP_SIZE
    comptime stride = head_dim + 4
    seq = Int(block_idx.x)
    query_head = Int(block_idx.y)
    lane = Int(thread_idx.x)
    lane_element = lane * epl
    query_base = (seq * n_q_heads + query_head) * head_dim
    head_base = (seq * n_q_heads + query_head) * HD256_SPLITS * stride
    var maximum: Float32 = NEG_INF
    for partition in range(HD256_SPLITS):
        maximum = max(
            maximum,
            partial_ptr[head_base + partition * stride + head_dim],
        )
    var denominator: Float32 = 0.0
    var output = SIMD[DType.float32, epl](0.0)
    for partition in range(HD256_SPLITS):
        partial_base = head_base + partition * stride
        factor = exp(partial_ptr[partial_base + head_dim] - maximum)
        denominator += partial_ptr[partial_base + head_dim + 1] * factor
        partial = (partial_ptr + partial_base + lane_element).load[
            width=epl, alignment=16
        ]()
        output += partial * factor
    (out_ptr + query_base + lane_element).store[
        width=epl, alignment=16
    ]((output / denominator).cast[DType.float16]())


comptime attn_decode_split8_f16_hd256 = attn_decode_split8_f16[256]
comptime attn_decode_split8_f16_hd512 = attn_decode_split8_f16[512]
comptime attn_decode_split8_combine_f16_hd256 = attn_decode_split8_combine_f16[256]
comptime attn_decode_split8_combine_f16_hd512 = attn_decode_split8_combine_f16[512]


def attn_decode_batch_exact_f16[head_dim: Int](
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
    """Dokładny batch krótkich zapytań korzystających ze wspólnej tablicy stron."""
    comptime epl = head_dim // WARP_SIZE

    token = Int(block_idx.x)
    qh = Int(block_idx.y)
    kvh = qh // (n_q_heads // n_kv_heads)
    lane = Int(thread_idx.x) % WARP_SIZE
    wid = Int(thread_idx.x) // WARP_SIZE
    n_warps = Int(block_dim.x) // WARP_SIZE
    ctx_len = Int(seq_lens[token])
    q_base = (token * n_q_heads + qh) * head_dim
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


comptime attn_decode_batch_exact_f16_hd256 = attn_decode_batch_exact_f16[256]


def attn_verify_segmented_f16[head_dim: Int](
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    q: UnsafePointer[Float16, MutAnyOrigin],
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    page_tables: UnsafePointer[Int32, MutAnyOrigin],
    visible_lens: UnsafePointer[Int32, MutAnyOrigin],
    n_tokens: Int,
    n_q_heads: Int,
    n_kv_heads: Int,
    page_size: Int,
    max_pages: Int,
    scale: Float32,
):
    """Przenośna atencja dla segmentów sequence-major `[B,T]`."""
    token = Int(block_idx.x)
    qh = Int(block_idx.y)
    kvh = qh // (n_q_heads // n_kv_heads)
    element = Int(thread_idx.x)
    lane = token // n_tokens
    ctx_len = Int(visible_lens[token])
    q_base = (token * n_q_heads + qh) * head_dim
    shared = stack_allocation[head_dim, Float32, address_space=AddressSpace.SHARED]()
    var maximum: Float32 = NEG_INF
    var denominator: Float32 = 0.0
    var output: Float32 = 0.0

    for pos in range(ctx_len):
        page = Int(page_tables[lane * max_pages + pos // page_size])
        kv_base = ((page * n_kv_heads + kvh) * page_size + pos % page_size) * head_dim
        shared[element] = Float32(q[q_base + element]) * Float32(k_cache[kv_base + element])
        barrier()
        var stride = head_dim // 2
        while stride > 0:
            if element < stride:
                shared[element] += shared[element + stride]
            barrier()
            stride //= 2
        score = shared[0] * scale
        next_maximum = max(maximum, score)
        correction = exp(maximum - next_maximum) if maximum > NEG_INF else Float32(0.0)
        probability = exp(score - next_maximum)
        denominator = denominator * correction + probability
        output = output * correction + probability * Float32(v_cache[kv_base + element])
        maximum = next_maximum
        barrier()

    out_ptr[q_base + element] = Float16(output / denominator)


def attn_verify_segmented_f16_warp32[head_dim: Int](
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    q: UnsafePointer[Float16, MutAnyOrigin],
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    page_tables: UnsafePointer[Int32, MutAnyOrigin],
    visible_lens: UnsafePointer[Int32, MutAnyOrigin],
    n_tokens: Int,
    n_q_heads: Int,
    n_kv_heads: Int,
    page_size: Int,
    max_pages: Int,
    scale: Float32,
):
    """Dokładna atencja NVIDIA dla osobnych tablic stron."""
    comptime elements_per_lane = head_dim // WARP_SIZE
    token = Int(block_idx.x)
    qh = Int(block_idx.y)
    kvh = qh // (n_q_heads // n_kv_heads)
    sequence = token // n_tokens
    lane_id = Int(thread_idx.x) % WARP_SIZE
    warp_id = Int(thread_idx.x) // WARP_SIZE
    n_warps = Int(block_dim.x) // WARP_SIZE
    ctx_len = Int(visible_lens[token])
    q_base = (token * n_q_heads + qh) * head_dim
    var q_frag = SIMD[DType.float32, elements_per_lane](0.0)
    comptime for element in range(elements_per_lane):
        q_frag[element] = Float32(q[q_base + element * WARP_SIZE + lane_id])

    var maximum: Float32 = NEG_INF
    var denominator: Float32 = 0.0
    var output = SIMD[DType.float32, elements_per_lane](0.0)
    var position = warp_id

    while position < ctx_len:
        page = Int(page_tables[sequence * max_pages + position // page_size])
        kv_base = (
            (page * n_kv_heads + kvh) * page_size + position % page_size
        ) * head_dim
        var partial: Float32 = 0.0
        comptime for element in range(elements_per_lane):
            offset = element * WARP_SIZE + lane_id
            partial += q_frag[element] * Float32(k_cache[kv_base + offset])
        score = warp.sum(partial) * scale
        next_maximum = max(maximum, score)
        correction = exp(maximum - next_maximum)
        probability = exp(score - next_maximum)
        denominator = denominator * correction + probability
        comptime for element in range(elements_per_lane):
            offset = element * WARP_SIZE + lane_id
            output[element] = output[element] * correction + probability * Float32(
                v_cache[kv_base + offset]
            )
        maximum = next_maximum
        position += n_warps

    shared_maximum = stack_allocation[MAX_WARPS, Float32, address_space=AddressSpace.SHARED]()
    shared_denominator = stack_allocation[MAX_WARPS, Float32, address_space=AddressSpace.SHARED]()
    shared_output = stack_allocation[
        MAX_WARPS * head_dim, Float32, address_space=AddressSpace.SHARED
    ]()

    if lane_id == 0:
        shared_maximum[warp_id] = maximum
        shared_denominator[warp_id] = denominator
    comptime for element in range(elements_per_lane):
        shared_output[warp_id * head_dim + element * WARP_SIZE + lane_id] = output[element]
    barrier()

    if warp_id == 0:
        var merged_maximum: Float32 = NEG_INF
        for source_warp in range(n_warps):
            if shared_maximum[source_warp] > merged_maximum:
                merged_maximum = shared_maximum[source_warp]
        var merged_denominator: Float32 = 0.0
        var merged_output = SIMD[DType.float32, elements_per_lane](0.0)
        for source_warp in range(n_warps):
            correction = exp(shared_maximum[source_warp] - merged_maximum)
            merged_denominator += shared_denominator[source_warp] * correction
            comptime for element in range(elements_per_lane):
                merged_output[element] += shared_output[
                    source_warp * head_dim + element * WARP_SIZE + lane_id
                ] * correction
        inverse_denominator = 1.0 / merged_denominator
        comptime for element in range(elements_per_lane):
            out_ptr[q_base + element * WARP_SIZE + lane_id] = Float16(
                merged_output[element] * inverse_denominator
            )


comptime attn_verify_segmented_f16_hd128 = attn_verify_segmented_f16[128]
comptime attn_verify_segmented_f16_hd256 = attn_verify_segmented_f16[256]
comptime attn_verify_segmented_f16_hd128_warp32 = attn_verify_segmented_f16_warp32[128]
comptime attn_verify_segmented_f16_hd256_warp32 = attn_verify_segmented_f16_warp32[256]


def attn_decode_split[head_dim: Int, kv_dtype: DType](
    parts: UnsafePointer[Float32, MutAnyOrigin],
    q_in: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    q_norm_w: UnsafePointer[Float16, MutAnyOrigin],
    k_norm_w: UnsafePointer[Float16, MutAnyOrigin],
    k_cache: UnsafePointer[Scalar[kv_dtype], MutAnyOrigin],
    v_cache: UnsafePointer[Scalar[kv_dtype], MutAnyOrigin],
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
    """Split-context flash-decode attention with the qkv_post stage folded
    into a per-block prologue: optional q/k RMSNorm, neox RoPE and the paged
    k/v append happen before the attention loop, removing the separate
    qkv_post launch. Grid.z splits the context into n_splits contiguous
    chunks; each block writes an UNNORMALIZED partial (acc, m, l) to
    `parts` ([n_seqs, n_q_heads, n_splits, head_dim + 2] f32) and
    attn_decode_combine_f16 merges them.

    n_splits == 1 reproduces attn_decode_f16's arithmetic bit-exactly (the
    partial is carried in f32 and the combine multiplies by exp(0) == 1.0);
    n_splits > 1 shortens each warp's sequential online-softmax chain by the
    split factor at the cost of a regrouped (differently rounded) softmax.

    Each (seq, q-head, split) block processes the RAW q head from the QKV
    GEMV (staged and rotated in shared memory — never written back) and its
    kv head's k/v append. GQA groups and splits repeat the k/v work per
    block; the duplicate stores write identical bytes, so the race is
    benign. All rounding points (f16 staging between norm and rope, f16
    cache stores) reproduce qkv_post_f16's dataflow bit-exactly; the extra
    zero-valued warps in the block-level norm reductions add exact 0.0
    terms. Block: n_warps x 32 with block >= head_dim required.

    kv_dtype = float8_e4m3fn stores the appended k/v as FP8: the value is
    rounded to f16 first (mirroring the f16 cache dataflow), then cast per
    value to e4m3 (RN, satfinite; no scale — e4m3's ±448 range covers
    post-norm K/V). Cache reads widen e4m3 exactly, so the attention math is
    bit-identical to the f16 kernel run on a dequantized cache.
    """
    comptime epl = head_dim // WARP_SIZE
    comptime half = head_dim // 2

    seq = Int(block_idx.x)
    qh = Int(block_idx.y)
    split = Int(block_idx.z)
    kvh = qh // (n_q_heads // n_kv_heads)
    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    wid = tid // WARP_SIZE
    n_warps = Int(block_dim.x) // WARP_SIZE
    ctx_len = Int(seq_lens[seq])
    # Short contexts collapse to fewer effective splits (>= 256 tokens per
    # split): the prologue (q/k norm reductions, RoPE, paged append) repeats
    # per split block, so at ctx ~200 a fixed 8-way split pays 8x prologue
    # for ~25-token attention chunks. Surplus split blocks publish a neutral
    # partial (m = -inf, l = 0 — zero weight in the combine) and exit before
    # the prologue; the surviving blocks each still perform the full benign
    # duplicate append. At ctx >= 256 * n_splits the chunking (and rounding)
    # is bit-identical to the fixed split.
    var eff_splits = (ctx_len + 255) // 256
    if eff_splits > n_splits:
        eff_splits = n_splits
    if eff_splits < 1:
        eff_splits = 1
    if split >= eff_splits:
        parts_base = ((seq * n_q_heads + qh) * n_splits + split) * (head_dim + 2)
        var pi = tid
        while pi < head_dim:
            parts[parts_base + pi] = 0.0
            pi += Int(block_dim.x)
        if tid == 0:
            parts[parts_base + head_dim] = NEG_INF
            parts[parts_base + head_dim + 1] = 0.0
        return
    chunk = (ctx_len + eff_splits - 1) // eff_splits
    start = split * chunk
    var end = start + chunk
    if end > ctx_len:
        end = ctx_len

    staged_q = stack_allocation[
        head_dim, Float16, address_space = AddressSpace.SHARED
    ]()
    staged_k = stack_allocation[
        head_dim, Float16, address_space = AddressSpace.SHARED
    ]()

    q_base = (seq * n_q_heads + qh) * head_dim
    k_base = (seq * n_kv_heads + kvh) * head_dim

    var q_raw: Float32 = 0.0
    var k_raw: Float32 = 0.0
    if tid < head_dim:
        q_raw = Float32(q_in[q_base + tid])
        k_raw = Float32(k_in[k_base + tid])
    var q_val = q_raw
    if has_q_norm == 1:
        total = block_reduce_sum(q_raw * q_raw)
        inv = rsqrt(total / Float32(head_dim) + eps)
        if tid < head_dim:
            q_val = q_raw * inv * Float32(q_norm_w[tid])
    var k_val = k_raw
    if has_k_norm == 1:
        total = block_reduce_sum(k_raw * k_raw)
        inv = rsqrt(total / Float32(head_dim) + eps)
        if tid < head_dim:
            k_val = k_raw * inv * Float32(k_norm_w[tid])
    if tid < head_dim:
        staged_q[tid] = Float16(q_val)
        staged_k[tid] = Float16(k_val)
    barrier()

    pos_cur = ctx_len - 1
    page_cur = Int(page_table[seq * max_pages + pos_cur // page_size])
    dst = (
        (page_cur * n_kv_heads + kvh) * page_size + pos_cur % page_size
    ) * head_dim
    if tid < half:
        freq = pow(theta_base, Float32(-2 * tid) / Float32(head_dim))
        angle = Float32(positions[seq]) * freq
        c = cos(angle)
        s = sin(angle)
        a = Float32(staged_q[tid])
        b = Float32(staged_q[half + tid])
        staged_q[tid] = Float16(a * c - b * s)
        staged_q[half + tid] = Float16(a * s + b * c)
        ak = Float32(staged_k[tid])
        bk = Float32(staged_k[half + tid])
        k_cache[dst + tid] = Scalar[kv_dtype](Float32(Float16(ak * c - bk * s)))
        k_cache[dst + half + tid] = Scalar[kv_dtype](
            Float32(Float16(ak * s + bk * c))
        )
    if tid < head_dim:
        v_cache[dst + tid] = Scalar[kv_dtype](Float32(v_in[k_base + tid]))
    barrier()

    var q_frag = SIMD[DType.float32, epl](0.0)

    comptime for e in range(epl):
        q_frag[e] = Float32(staged_q[e * WARP_SIZE + lane])

    var m: Float32 = NEG_INF
    var l: Float32 = 0.0
    var acc = SIMD[DType.float32, epl](0.0)

    # Decode occupancy is low (n_heads blocks of n_warps warps), so the
    # position loop is latency-bound on the k/v loads and the per-position
    # shuffle reduction. Processing UNROLL positions per iteration issues all
    # their k/v loads up front (overlapping the warp.sum chains) while the
    # online-softmax updates stay strictly sequential in the original
    # position order — the f32 math is bit-identical to the 1-per-iteration
    # loop, just faster.
    comptime UNROLL = 8
    step = UNROLL * n_warps
    var pos = start + wid
    var kf = SIMD[DType.float32, UNROLL * epl](0.0)
    var vf = SIMD[DType.float32, UNROLL * epl](0.0)
    if pos + (UNROLL - 1) * n_warps < end:
        comptime for j in range(UNROLL):
            pj = pos + j * n_warps
            page_j = Int(page_table[seq * max_pages + pj // page_size])
            base_j = ((page_j * n_kv_heads + kvh) * page_size + (pj % page_size)) * head_dim
            kf_j = kv_frag_f32[kv_dtype, epl](k_cache, base_j, lane)
            vf_j = kv_frag_f32[kv_dtype, epl](v_cache, base_j, lane)

            comptime for e in range(epl):
                kf[j * epl + e] = kf_j[e]
                vf[j * epl + e] = vf_j[e]
    while pos + (UNROLL - 1) * n_warps < end:
        var scores = SIMD[DType.float32, UNROLL](0.0)

        comptime for j in range(UNROLL):
            var dot: Float32 = 0.0

            comptime for e in range(epl):
                dot += q_frag[e] * kf[j * epl + e]
            scores[j] = warp.sum(dot) * scale

        # Prefetch the next group's k/v while this group's (strictly
        # sequential, bit-exact) softmax updates run — v for the current
        # group is already in registers, so overwriting kf2/vf2 is safe.
        next_pos = pos + step
        var kf2 = SIMD[DType.float32, UNROLL * epl](0.0)
        var vf2 = SIMD[DType.float32, UNROLL * epl](0.0)
        if next_pos + (UNROLL - 1) * n_warps < end:
            comptime for j in range(UNROLL):
                pj = next_pos + j * n_warps
                page_j = Int(page_table[seq * max_pages + pj // page_size])
                base_j = (
                    (page_j * n_kv_heads + kvh) * page_size + (pj % page_size)
                ) * head_dim
                kf2_j = kv_frag_f32[kv_dtype, epl](k_cache, base_j, lane)
                vf2_j = kv_frag_f32[kv_dtype, epl](v_cache, base_j, lane)

                comptime for e in range(epl):
                    kf2[j * epl + e] = kf2_j[e]
                    vf2[j * epl + e] = vf2_j[e]

        comptime for j in range(UNROLL):
            var m_new = m
            if scores[j] > m_new:
                m_new = scores[j]
            factor = exp(m - m_new)
            p = exp(scores[j] - m_new)
            l = l * factor + p

            comptime for e in range(epl):
                acc[e] = acc[e] * factor + p * vf[j * epl + e]
            m = m_new

        kf = kf2
        vf = vf2
        pos = next_pos

    while pos < end:
        page = Int(page_table[seq * max_pages + pos // page_size])
        kv_base = ((page * n_kv_heads + kvh) * page_size + (pos % page_size)) * head_dim
        kfrag = kv_frag_f32[kv_dtype, epl](k_cache, kv_base, lane)

        var dot: Float32 = 0.0

        comptime for e in range(epl):
            dot += q_frag[e] * kfrag[e]
        score = warp.sum(dot) * scale

        var m_new = m
        if score > m_new:
            m_new = score
        factor = exp(m - m_new)
        p = exp(score - m_new)
        l = l * factor + p
        vfrag = kv_frag_f32[kv_dtype, epl](v_cache, kv_base, lane)

        comptime for e in range(epl):
            acc[e] = acc[e] * factor + p * vfrag[e]
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

        parts_base = ((seq * n_q_heads + qh) * n_splits + split) * (head_dim + 2)

        comptime for e in range(epl):
            parts[parts_base + e * WARP_SIZE + lane] = out_frag[e]
        if lane == 0:
            parts[parts_base + head_dim] = m_star
            parts[parts_base + head_dim + 1] = l_star


comptime attn_decode_split_f16_hd64 = attn_decode_split[64, DType.float16]
comptime attn_decode_split_f16_hd128 = attn_decode_split[128, DType.float16]
# head_dim 512 — warstwy globalne Gemmy 4 na ścieżce decode w batchu mieszanym.
comptime attn_decode_split_f16_hd512 = attn_decode_split[512, DType.float16]
comptime attn_decode_split_fp8_hd64 = attn_decode_split[64, DType.float8_e4m3fn]
comptime attn_decode_split_fp8_hd128 = attn_decode_split[128, DType.float8_e4m3fn]


def attn_decode_combine_f16[head_dim: Int](
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    parts: UnsafePointer[Float32, MutAnyOrigin],
    n_q_heads: Int,
    n_splits: Int,
):
    """Merge attn_decode_split_f16's per-split partials into the final head
    output. One warp per (seq, q-head); splits merge sequentially in split
    order (deterministic). With n_splits == 1 the merge multiplies by
    exp(0) == 1.0 and reproduces attn_decode_f16's normalization exactly.
    Grid: (n_seqs, n_q_heads); block: 32.
    """
    comptime epl = head_dim // WARP_SIZE

    seq = Int(block_idx.x)
    qh = Int(block_idx.y)
    lane = Int(thread_idx.x)
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

        comptime for e in range(epl):
            out_frag[e] += parts[sbase + e * WARP_SIZE + lane] * f

    inv_l = 1.0 / l_total
    out_base = (seq * n_q_heads + qh) * head_dim

    comptime for e in range(epl):
        out_ptr[out_base + e * WARP_SIZE + lane] = Float16(out_frag[e] * inv_l)


comptime attn_decode_combine_f16_hd64 = attn_decode_combine_f16[64]
comptime attn_decode_combine_f16_hd128 = attn_decode_combine_f16[128]
comptime attn_decode_combine_f16_hd512 = attn_decode_combine_f16[512]
