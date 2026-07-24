# =============================================================================
# Plik: prefill_fa_hd256.mojo
# Opis: Tensor-core causal Flash Attention F16 dla glow HD256 i stronicowanego KV.
# Przyklad: attn_prefill_fa_mojo_f16_hd256 obsluguje pelny prefill jednej sekwencji.
# =============================================================================

from std.gpu import WARP_SIZE, block_idx, thread_idx
from std.gpu.compute.mma import ld_matrix, mma
from std.gpu.memory import AddressSpace
from std.gpu.primitives.warp import shuffle_xor
from std.gpu.sync import barrier
from std.math import exp
from std.memory import stack_allocation

comptime HEAD_DIM = 256
comptime QUERY_TILE = 64
comptime KEY_TILE = 16
comptime BLOCK_THREADS = 128
comptime NEG_INF: Float32 = -1e30


@always_inline
def _attn_prefill_fa_hd256[
    query_tile: Int, key_tile: Int, block_threads: Int, transpose_values: Bool,
](
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
    n_tokens: Int,
):
    comptime QK_CHUNKS = HEAD_DIM // 16
    comptime SCORE_SUBTILES = key_tile // 8
    comptime VALUE_CHUNKS = key_tile // 16
    comptime OUTPUT_SUBTILES = HEAD_DIM // 8

    tid = Int(thread_idx.x)
    warp_id = tid // WARP_SIZE
    lane = tid % WARP_SIZE
    lane_row = lane & 7
    sub = lane >> 3
    query0 = Int(block_idx.x) * query_tile
    query_head = Int(block_idx.y)
    kv_head = query_head // (n_q_heads // n_kv_heads)
    warp_query0 = query0 + warp_id * 16

    comptime QUERY_ELEMENTS = query_tile * HEAD_DIM
    comptime KV_ELEMENTS = 2 * key_tile * HEAD_DIM
    comptime WORKSPACE_ELEMENTS = max(QUERY_ELEMENTS, KV_ELEMENTS)
    workspace = stack_allocation[
        WORKSPACE_ELEMENTS, Float16, address_space=AddressSpace.SHARED
    ]()
    queries = workspace
    keys = workspace
    values = workspace + key_tile * HEAD_DIM

    var element = tid * 8
    while element < query_tile * HEAD_DIM:
        row = element // HEAD_DIM
        column = element % HEAD_DIM
        var token = query0 + row
        if token > n_tokens - 1:
            token = n_tokens - 1
        (queries + element).store[width=8, alignment=16](
            (q + (token * n_q_heads + query_head) * HEAD_DIM + column).load[
                width=8, alignment=16
            ]()
        )
        element += block_threads * 8
    barrier()

    group = lane >> 3
    query_row = warp_id * 16 + (lane & 7) + ((group & 1) << 3)
    query_column = (group >> 1) << 3
    var query_fragments = InlineArray[SIMD[DType.float16, 8], QK_CHUNKS](
        fill=SIMD[DType.float16, 8](0.0)
    )
    comptime for chunk in range(QK_CHUNKS):
        query_fragments[chunk] = ld_matrix[8](
            queries + query_row * HEAD_DIM + chunk * 16 + query_column
        )

    var max_a = NEG_INF
    var max_b = NEG_INF
    var sum_a: Float32 = 0.0
    var sum_b: Float32 = 0.0
    var output = InlineArray[SIMD[DType.float32, 4], OUTPUT_SUBTILES](
        fill=SIMD[DType.float32, 4](0.0)
    )

    var token_high = query0 + query_tile
    if token_high > n_tokens:
        token_high = n_tokens
    max_position = base_pos + token_high - 1

    var position0 = 0
    while position0 <= max_position:
        var valid_keys = max_position + 1 - position0
        if valid_keys > key_tile:
            valid_keys = key_tile
        barrier()

        var cache_element = tid * 8
        while cache_element < key_tile * HEAD_DIM:
            row = cache_element // HEAD_DIM
            column = cache_element % HEAD_DIM
            position = position0 + row
            if row < valid_keys:
                page = Int(page_table[position // page_size])
                cache_base = (
                    (page * n_kv_heads + kv_head) * page_size
                    + position % page_size
                ) * HEAD_DIM + column
                (keys + cache_element).store[width=8, alignment=16](
                    (k_cache + cache_base).load[width=8, alignment=16]()
                )
                value8 = (v_cache + cache_base).load[width=8, alignment=16]()
                comptime if transpose_values:
                    (values + cache_element).store[width=8, alignment=16](value8)
                else:
                    comptime for index in range(8):
                        values[(column + index) * key_tile + row] = value8[index]
            else:
                (keys + cache_element).store[width=8, alignment=16](
                    SIMD[DType.float16, 8](0.0)
                )
                comptime if transpose_values:
                    (values + cache_element).store[width=8, alignment=16](
                        SIMD[DType.float16, 8](0.0)
                    )
                else:
                    comptime for index in range(8):
                        values[(column + index) * key_tile + row] = Float16(0.0)
            cache_element += block_threads * 8
        barrier()

        var scores = InlineArray[SIMD[DType.float32, 4], SCORE_SUBTILES](
            fill=SIMD[DType.float32, 4](0.0)
        )
        comptime for score_tile in range(SCORE_SUBTILES):
            comptime for chunk in range(QK_CHUNKS):
                key_fragment = ld_matrix[4](
                    keys
                    + (score_tile * 8 + lane_row) * HEAD_DIM
                    + chunk * 16
                    + (sub & 1) * 8
                )
                mma(
                    scores[score_tile],
                    query_fragments[chunk],
                    key_fragment,
                    scores[score_tile],
                )

        var local_a = NEG_INF
        var local_b = NEG_INF
        global_a = query0 + warp_id * 16 + (lane >> 2)
        global_b = global_a + 8
        horizon_a = base_pos + global_a - position0
        horizon_b = base_pos + global_b - position0
        comptime for score_tile in range(SCORE_SUBTILES):
            key_a = score_tile * 8 + (lane & 3) * 2
            var score = scores[score_tile]
            if key_a >= valid_keys or key_a > horizon_a:
                score[0] = NEG_INF
            if key_a + 1 >= valid_keys or key_a + 1 > horizon_a:
                score[1] = NEG_INF
            if key_a >= valid_keys or key_a > horizon_b:
                score[2] = NEG_INF
            if key_a + 1 >= valid_keys or key_a + 1 > horizon_b:
                score[3] = NEG_INF
            score[0] *= scale
            score[1] *= scale
            score[2] *= scale
            score[3] *= scale
            scores[score_tile] = score
            local_a = max(local_a, max(score[0], score[1]))
            local_b = max(local_b, max(score[2], score[3]))

        local_a = max(local_a, shuffle_xor(local_a, 1))
        local_a = max(local_a, shuffle_xor(local_a, 2))
        local_b = max(local_b, shuffle_xor(local_b, 1))
        local_b = max(local_b, shuffle_xor(local_b, 2))
        new_max_a = max(max_a, local_a)
        new_max_b = max(max_b, local_b)
        correction_a = Float32(1.0) if max_a <= NEG_INF else exp(max_a - new_max_a)
        correction_b = Float32(1.0) if max_b <= NEG_INF else exp(max_b - new_max_b)
        max_a = new_max_a
        max_b = new_max_b
        sum_a *= correction_a
        sum_b *= correction_b
        comptime for output_tile in range(OUTPUT_SUBTILES):
            var accumulator = output[output_tile]
            accumulator[0] *= correction_a
            accumulator[1] *= correction_a
            accumulator[2] *= correction_b
            accumulator[3] *= correction_b
            output[output_tile] = accumulator

        var tile_sum_a: Float32 = 0.0
        var tile_sum_b: Float32 = 0.0
        comptime for score_tile in range(SCORE_SUBTILES):
            var score = scores[score_tile]
            probability0 = exp(score[0] - max_a)
            probability1 = exp(score[1] - max_a)
            probability2 = exp(score[2] - max_b)
            probability3 = exp(score[3] - max_b)
            score[0] = probability0
            score[1] = probability1
            score[2] = probability2
            score[3] = probability3
            scores[score_tile] = score
            tile_sum_a += probability0 + probability1
            tile_sum_b += probability2 + probability3
        tile_sum_a += shuffle_xor(tile_sum_a, 1)
        tile_sum_a += shuffle_xor(tile_sum_a, 2)
        tile_sum_b += shuffle_xor(tile_sum_b, 1)
        tile_sum_b += shuffle_xor(tile_sum_b, 2)
        sum_a += tile_sum_a
        sum_b += tile_sum_b

        comptime for value_chunk in range(VALUE_CHUNKS):
            score0 = scores[2 * value_chunk]
            score1 = scores[2 * value_chunk + 1]
            var probabilities = SIMD[DType.float16, 8](0.0)
            probabilities[0] = Float16(score0[0])
            probabilities[1] = Float16(score0[1])
            probabilities[2] = Float16(score0[2])
            probabilities[3] = Float16(score0[3])
            probabilities[4] = Float16(score1[0])
            probabilities[5] = Float16(score1[1])
            probabilities[6] = Float16(score1[2])
            probabilities[7] = Float16(score1[3])
            comptime for output_tile in range(OUTPUT_SUBTILES):
                comptime if transpose_values:
                    value_fragment = ld_matrix[4, transpose=True](
                        values
                        + (
                            value_chunk * 16
                            + (sub & 1) * 8
                            + lane_row
                        ) * HEAD_DIM
                        + output_tile * 8
                    )
                else:
                    value_fragment = ld_matrix[4](
                        values
                        + (output_tile * 8 + lane_row) * key_tile
                        + value_chunk * 16
                        + (sub & 1) * 8
                    )
                mma(
                    output[output_tile],
                    probabilities,
                    value_fragment,
                    output[output_tile],
                )
        position0 += key_tile

    row_a = warp_query0 + (lane >> 2)
    row_b = row_a + 8
    inverse_a = (1.0 / sum_a) if sum_a > 0.0 else Float32(0.0)
    inverse_b = (1.0 / sum_b) if sum_b > 0.0 else Float32(0.0)
    comptime for output_tile in range(OUTPUT_SUBTILES):
        column = output_tile * 8 + (lane & 3) * 2
        accumulator = output[output_tile]
        if row_a < n_tokens:
            destination = (row_a * n_q_heads + query_head) * HEAD_DIM + column
            out_ptr[destination] = Float16(accumulator[0] * inverse_a)
            out_ptr[destination + 1] = Float16(accumulator[1] * inverse_a)
        if row_b < n_tokens:
            destination = (row_b * n_q_heads + query_head) * HEAD_DIM + column
            out_ptr[destination] = Float16(accumulator[2] * inverse_b)
            out_ptr[destination + 1] = Float16(accumulator[3] * inverse_b)


def attn_prefill_fa_mojo_f16_hd256(
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
    n_tokens: Int,
):
    _attn_prefill_fa_hd256[QUERY_TILE, KEY_TILE, BLOCK_THREADS, False](
        out_ptr, q, k_cache, v_cache, page_table, base_pos, n_q_heads,
        n_kv_heads, page_size, scale, n_tokens,
    )


def attn_prefill_fa_mojo_device_pos_f16_hd256(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    q: UnsafePointer[Float16, MutAnyOrigin],
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    base_pos: UnsafePointer[Int32, MutAnyOrigin],
    n_q_heads: Int,
    n_kv_heads: Int,
    page_size: Int,
    scale: Float32,
    n_tokens: Int,
):
    _attn_prefill_fa_hd256[QUERY_TILE, KEY_TILE, BLOCK_THREADS, False](
        out_ptr, q, k_cache, v_cache, page_table, Int(base_pos[0]), n_q_heads,
        n_kv_heads, page_size, scale, n_tokens,
    )


def attn_prefill_fa_mojo_device_pos_f16_hd256_bk32(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    q: UnsafePointer[Float16, MutAnyOrigin],
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    base_pos: UnsafePointer[Int32, MutAnyOrigin],
    n_q_heads: Int,
    n_kv_heads: Int,
    page_size: Int,
    scale: Float32,
    n_tokens: Int,
):
    _attn_prefill_fa_hd256[QUERY_TILE, 32, BLOCK_THREADS, False](
        out_ptr, q, k_cache, v_cache, page_table, Int(base_pos[0]), n_q_heads,
        n_kv_heads, page_size, scale, n_tokens,
    )


def attn_prefill_fa_mojo_f16_hd256_bk32(
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
    n_tokens: Int,
):
    _attn_prefill_fa_hd256[QUERY_TILE, 32, BLOCK_THREADS, False](
        out_ptr, q, k_cache, v_cache, page_table, base_pos, n_q_heads,
        n_kv_heads, page_size, scale, n_tokens,
    )


def attn_prefill_fa_mojo_f16_hd256_vtrans(
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
    n_tokens: Int,
):
    _attn_prefill_fa_hd256[QUERY_TILE, KEY_TILE, BLOCK_THREADS, True](
        out_ptr, q, k_cache, v_cache, page_table, base_pos, n_q_heads,
        n_kv_heads, page_size, scale, n_tokens,
    )


def attn_prefill_fa_mojo_device_pos_f16_hd256_vtrans(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    q: UnsafePointer[Float16, MutAnyOrigin],
    k_cache: UnsafePointer[Float16, MutAnyOrigin],
    v_cache: UnsafePointer[Float16, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    base_pos: UnsafePointer[Int32, MutAnyOrigin],
    n_q_heads: Int,
    n_kv_heads: Int,
    page_size: Int,
    scale: Float32,
    n_tokens: Int,
):
    _attn_prefill_fa_hd256[QUERY_TILE, KEY_TILE, BLOCK_THREADS, True](
        out_ptr, q, k_cache, v_cache, page_table, Int(base_pos[0]), n_q_heads,
        n_kv_heads, page_size, scale, n_tokens,
    )
