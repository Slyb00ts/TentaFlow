# =============================================================================
# Plik: attention_verify_split8.mojo
# Opis: Dzielona atencja verifiera HD256 współdzieląca odczyty KV między tokenami MTP.
# Przykład: Import specjalizacji T3 lub T4 w programie testowym Mojo.
# =============================================================================

from std.gpu import WARP_SIZE, block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.primitives import warp
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import exp

comptime HEAD_DIM = 256
comptime ELEMENTS_PER_LANE = 8
comptime SPLITS = 8
comptime PARTIAL_STRIDE = 260
comptime MAX_WARPS = 8
comptime NEG_INF: Float32 = -1e30


def attn_verify_split8_f16_hd256[tokens: Int](
    partial_ptr: UnsafePointer[Float32, MutAnyOrigin],
    q: UnsafePointer[Float16, MutAnyOrigin],
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    page_tables: UnsafePointer[Int32, MutAnyOrigin],
    visible_lens: UnsafePointer[Int32, MutAnyOrigin],
    n_q_heads: Int,
    n_kv_heads: Int,
    page_size: Int,
    max_pages: Int,
    scale: Float32,
):
    """Oblicza wszystkie tokeny MTP, ponownie używając jednego odczytu K/V."""
    sequence = Int(block_idx.x)
    query_head = Int(block_idx.y)
    partition = Int(block_idx.z)
    kv_head = query_head // (n_q_heads // n_kv_heads)
    lane = Int(thread_idx.x) % WARP_SIZE
    warp_id = Int(thread_idx.x) // WARP_SIZE
    warps = Int(block_dim.x) // WARP_SIZE
    lane_element = lane * ELEMENTS_PER_LANE

    var queries = SIMD[DType.float32, tokens * ELEMENTS_PER_LANE](0.0)
    comptime for token in range(tokens):
        query_base = (
            (sequence * tokens + token) * n_q_heads + query_head
        ) * HEAD_DIM + lane_element
        query = (q + query_base).load[
            width=ELEMENTS_PER_LANE, alignment=16
        ]().cast[DType.float32]()
        comptime for element in range(ELEMENTS_PER_LANE):
            queries[token * ELEMENTS_PER_LANE + element] = query[element]

    var maxima = SIMD[DType.float32, tokens](NEG_INF)
    var denominators = SIMD[DType.float32, tokens](0.0)
    var outputs = SIMD[DType.float32, tokens * ELEMENTS_PER_LANE](0.0)
    maximum_context = Int(visible_lens[sequence * tokens + tokens - 1])
    var position = partition + warp_id * SPLITS
    while position < maximum_context:
        page = Int(
            page_tables[
                sequence * max_pages + position // page_size
            ]
        )
        cache_base = (
            (page * n_kv_heads + kv_head) * page_size + position % page_size
        ) * HEAD_DIM + lane_element
        key = (k_cache + cache_base).load[
            width=ELEMENTS_PER_LANE, alignment=16
        ]().cast[DType.float32]()
        value = (v_cache + cache_base).load[
            width=ELEMENTS_PER_LANE, alignment=16
        ]().cast[DType.float32]()

        comptime for token in range(tokens):
            if position < Int(visible_lens[sequence * tokens + token]):
                var dot: Float32 = 0.0
                comptime for element in range(ELEMENTS_PER_LANE):
                    dot += (
                        queries[token * ELEMENTS_PER_LANE + element]
                        * key[element]
                    )
                score = warp.sum(dot) * scale
                next_maximum = max(maxima[token], score)
                correction = exp(maxima[token] - next_maximum)
                probability = exp(score - next_maximum)
                denominators[token] = (
                    denominators[token] * correction + probability
                )
                comptime for element in range(ELEMENTS_PER_LANE):
                    index = token * ELEMENTS_PER_LANE + element
                    outputs[index] = (
                        outputs[index] * correction
                        + value[element] * probability
                    )
                maxima[token] = next_maximum
        position += warps * SPLITS

    shared_maxima = stack_allocation[
        MAX_WARPS * tokens, Float32, address_space=AddressSpace.SHARED
    ]()
    shared_denominators = stack_allocation[
        MAX_WARPS * tokens, Float32, address_space=AddressSpace.SHARED
    ]()
    shared_outputs = stack_allocation[
        MAX_WARPS * tokens * HEAD_DIM,
        Float32,
        address_space=AddressSpace.SHARED,
    ]()
    comptime for token in range(tokens):
        if lane == 0:
            shared_maxima[warp_id * tokens + token] = maxima[token]
            shared_denominators[warp_id * tokens + token] = denominators[token]
        comptime for element in range(ELEMENTS_PER_LANE):
            shared_outputs[
                (warp_id * tokens + token) * HEAD_DIM
                + lane_element
                + element
            ] = outputs[token * ELEMENTS_PER_LANE + element]
    barrier()

    if warp_id == 0:
        comptime for token in range(tokens):
            var block_maximum: Float32 = NEG_INF
            for source_warp in range(warps):
                block_maximum = max(
                    block_maximum,
                    shared_maxima[source_warp * tokens + token],
                )
            var block_denominator: Float32 = 0.0
            var block_output = SIMD[
                DType.float32, ELEMENTS_PER_LANE
            ](0.0)
            for source_warp in range(warps):
                factor = exp(
                    shared_maxima[source_warp * tokens + token]
                    - block_maximum
                )
                block_denominator += (
                    shared_denominators[source_warp * tokens + token]
                    * factor
                )
                comptime for element in range(ELEMENTS_PER_LANE):
                    block_output[element] += shared_outputs[
                        (source_warp * tokens + token) * HEAD_DIM
                        + lane_element
                        + element
                    ] * factor
            partial_base = (
                (
                    (sequence * tokens + token) * n_q_heads + query_head
                ) * SPLITS + partition
            ) * PARTIAL_STRIDE
            (partial_ptr + partial_base + lane_element).store[
                width=ELEMENTS_PER_LANE, alignment=16
            ](block_output)
            if lane == 0:
                partial_ptr[partial_base + HEAD_DIM] = block_maximum
                partial_ptr[partial_base + HEAD_DIM + 1] = block_denominator


def attn_verify_split8_combine_f16_hd256(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    partial_ptr: UnsafePointer[Float32, MutAnyOrigin],
    tokens: Int,
    n_q_heads: Int,
):
    sequence = Int(block_idx.x)
    token_head = Int(block_idx.y)
    token = token_head // n_q_heads
    query_head = token_head % n_q_heads
    lane = Int(thread_idx.x)
    lane_element = lane * ELEMENTS_PER_LANE
    query_base = (
        (sequence * tokens + token) * n_q_heads + query_head
    ) * HEAD_DIM
    head_base = (
        (
            (sequence * tokens + token) * n_q_heads + query_head
        ) * SPLITS * PARTIAL_STRIDE
    )
    var maximum: Float32 = NEG_INF
    for partition in range(SPLITS):
        maximum = max(
            maximum,
            partial_ptr[
                head_base + partition * PARTIAL_STRIDE + HEAD_DIM
            ],
        )
    var denominator: Float32 = 0.0
    var output = SIMD[DType.float32, ELEMENTS_PER_LANE](0.0)
    for partition in range(SPLITS):
        partial_base = head_base + partition * PARTIAL_STRIDE
        factor = exp(partial_ptr[partial_base + HEAD_DIM] - maximum)
        denominator += partial_ptr[partial_base + HEAD_DIM + 1] * factor
        partial = (partial_ptr + partial_base + lane_element).load[
            width=ELEMENTS_PER_LANE, alignment=16
        ]()
        output += partial * factor
    (out_ptr + query_base + lane_element).store[
        width=ELEMENTS_PER_LANE, alignment=16
    ]((output / denominator).cast[DType.float16]())


comptime attn_verify_split8_f16_hd256_t3 = attn_verify_split8_f16_hd256[3]
comptime attn_verify_split8_f16_hd256_t4 = attn_verify_split8_f16_hd256[4]
