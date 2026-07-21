# =============================================================================
# Plik: attention_gqa_combine.mojo
# Opis: Laczy partiale attention GQA, przetwarzajac dwie glowice Q w jednym CTA.
# Przyklad: Siatka (sekwencje, ceil(glowice_q / 2)), blok 64 watkow.
# =============================================================================

from std.gpu import block_idx, thread_idx
from std.math import exp

comptime WARP = 32
comptime HEAD_DIM = 128
comptime HEADS_PER_BLOCK = 2
comptime NEGATIVE_INFINITY: Float32 = -3.402823466e38


def attn_decode_combine_gqa2_f16_hd128(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    parts: UnsafePointer[Float32, MutAnyOrigin],
    n_q_heads: Int,
    n_splits: Int,
):
    comptime elements_per_lane = HEAD_DIM // WARP
    lane = Int(thread_idx.x) % WARP
    warp_id = Int(thread_idx.x) // WARP
    qh = Int(block_idx.y) * HEADS_PER_BLOCK + warp_id
    if qh >= n_q_heads:
        return
    seq = Int(block_idx.x)
    head_base = (seq * n_q_heads + qh) * n_splits * (HEAD_DIM + 2)

    var m_star: Float32 = NEGATIVE_INFINITY
    for split in range(n_splits):
        value = parts[head_base + split * (HEAD_DIM + 2) + HEAD_DIM]
        if value > m_star:
            m_star = value

    var l_total: Float32 = 0.0
    var out_frag = SIMD[DType.float32, elements_per_lane](0.0)
    for split in range(n_splits):
        split_base = head_base + split * (HEAD_DIM + 2)
        factor = exp(parts[split_base + HEAD_DIM] - m_star)
        l_total += parts[split_base + HEAD_DIM + 1] * factor
        comptime for element in range(elements_per_lane):
            out_frag[element] += parts[split_base + element * WARP + lane] * factor

    inv_l = 1.0 / l_total
    out_base = (seq * n_q_heads + qh) * HEAD_DIM
    comptime for element in range(elements_per_lane):
        out_ptr[out_base + element * WARP + lane] = Float16(
            out_frag[element] * inv_l
        )
