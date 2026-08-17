# =============================================================================
# Plik: attention_gqa_hd256.mojo
# Opis: Dekod uwagi HD256 dzielący jeden strumień K/V między głowice Q grupy GQA.
# Przykład: siatka [sekwencja, głowica KV, partycja], blok 256 wątków.
# =============================================================================
#
# Wariant `attn_decode_split8_f16[256]` ma siatkę [sekwencja, głowica Q, partycja]
# i każda grupa robocza czyta CAŁY strumień K/V swojej głowicy KV. Przy GQA 24:4
# ten sam cache czyta więc sześć grup, czyli model przemiata sześć razy więcej
# bajtów KV, niż wynosi jego rozmiar. Zmierzone na 32k kontekstu: 22 ms na token
# w samym dekodzie uwagi, co daje ~95 GB/s przy 551 GB/s osiągalnych.
#
# Tutaj siatka idzie po głowicach KV, a grupa robocza liczy WSZYSTKIE głowice Q
# tej grupy: pozycja jest odczytywana raz, a iloczyn skalarny powtarza się per
# głowica. Ruch pamięci spada `GROUP` razy, praca ALU zostaje ta sama.
#
# Kolejność akumulacji per głowica jest identyczna jak w wariancie rozdzielnym
# (ten sam krok pozycji, ta sama redukcja międzywarpowa, ten sam układ
# partiali), więc wynik jest BITOWO ten sam — i to jest bramka
# tego kernela, nie tolerancja numeryczna.
#
# `SPLITS` jest parametrem, bo siatka wariantu dzielonego idzie po głowicach KV,
# a tych jest `GROUP` razy mniej niż głowic Q: przy 4 głowicach KV i 8 partycjach
# zostają 32 grupy robocze na 64 CU, czyli połowa karty stoi. Większe `SPLITS`
# zmienia podział kontekstu, więc wynik przestaje być bitowo zgodny z ósemką.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.primitives import warp
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import exp

comptime WARP = 32
comptime MAX_WARPS = 8
comptime HD = 256
comptime EPL = HD // WARP
comptime SPLITS = 8
comptime NEG_INF: Float32 = -1e30


def attn_decode_split_gqa_f16_hd256[GROUP: Int](
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
    """`GROUP` głowic Q na jeden odczyt K/V; partiale jak w wariancie split8."""
    seq = Int(block_idx.x)
    kv_head = Int(block_idx.y)
    partition = Int(block_idx.z)
    lane = Int(thread_idx.x) % WARP
    warp_id = Int(thread_idx.x) // WARP
    warps = Int(block_dim.x) // WARP
    context = Int(seq_lens[seq])
    var first = 0
    if window > 0 and context > window:
        first = context - window
    lane_element = lane * EPL

    # Głowice Q tej grupy leżą obok siebie, tak samo jak wylicza je wariant
    # rozdzielny (`kv_head = query_head // (n_q_heads // n_kv_heads)`).
    var query = InlineArray[SIMD[DType.float32, EPL], GROUP](
        fill=SIMD[DType.float32, EPL](0.0)
    )
    comptime for g in range(GROUP):
        query_base = (seq * n_q_heads + kv_head * GROUP + g) * HD
        query[g] = (q + query_base + lane_element).load[
            width=EPL, alignment=16
        ]().cast[DType.float32]()

    var maximum = InlineArray[Float32, GROUP](fill=NEG_INF)
    var denominator = InlineArray[Float32, GROUP](fill=0.0)
    var output = InlineArray[SIMD[DType.float32, EPL], GROUP](
        fill=SIMD[DType.float32, EPL](0.0)
    )

    var position = first + partition + warp_id * SPLITS
    while position < context:
        page = Int(page_table[seq * max_pages + position // page_size])
        cache_base = (
            (page * n_kv_heads + kv_head) * page_size + position % page_size
        ) * HD + lane_element
        key = (k_cache + cache_base).load[
            width=EPL, alignment=16
        ]().cast[DType.float32]()
        value = (v_cache + cache_base).load[
            width=EPL, alignment=16
        ]().cast[DType.float32]()
        comptime for g in range(GROUP):
            score = warp.sum((query[g] * key).reduce_add()) * scale
            next_maximum = max(maximum[g], score)
            correction = exp(maximum[g] - next_maximum)
            probability = exp(score - next_maximum)
            denominator[g] = denominator[g] * correction + probability
            output[g] = output[g] * correction + value * probability
            maximum[g] = next_maximum
        position += warps * SPLITS

    # Redukcja międzywarpowa idzie GŁOWICA PO GŁOWICY na jednym buforze zamiast
    # trzymać `GROUP` kompletów naraz: komplet dla sześciu głowic to 48 KiB LDS,
    # co samo w sobie ścięłoby zajętość.
    shared_maximum = stack_allocation[
        MAX_WARPS, Float32, address_space=AddressSpace.SHARED
    ]()
    shared_denominator = stack_allocation[
        MAX_WARPS, Float32, address_space=AddressSpace.SHARED
    ]()
    shared_output = stack_allocation[
        MAX_WARPS * HD, Float32, address_space=AddressSpace.SHARED
    ]()
    comptime for g in range(GROUP):
        barrier()
        if lane == 0:
            shared_maximum[warp_id] = maximum[g]
            shared_denominator[warp_id] = denominator[g]
        (shared_output + warp_id * HD + lane_element).store[
            width=EPL, alignment=16
        ](output[g])
        barrier()
        if warp_id == 0:
            var block_maximum: Float32 = NEG_INF
            for index in range(warps):
                block_maximum = max(block_maximum, shared_maximum[index])
            var block_denominator: Float32 = 0.0
            var block_output = SIMD[DType.float32, EPL](0.0)
            for index in range(warps):
                factor = exp(shared_maximum[index] - block_maximum)
                block_denominator += shared_denominator[index] * factor
                partial = (shared_output + index * HD + lane_element).load[
                    width=EPL, alignment=16
                ]()
                block_output += partial * factor
            partial_base = (
                (seq * n_q_heads + kv_head * GROUP + g) * SPLITS + partition
            ) * (HD + 4)
            (partial_ptr + partial_base + lane_element).store[
                width=EPL, alignment=16
            ](block_output)
            if lane == 0:
                partial_ptr[partial_base + HD] = block_maximum
                partial_ptr[partial_base + HD + 1] = block_denominator




comptime attn_decode_split_gqa2_f16_hd256 = attn_decode_split_gqa_f16_hd256[2]
comptime attn_decode_split_gqa3_f16_hd256 = attn_decode_split_gqa_f16_hd256[3]
comptime attn_decode_split_gqa6_f16_hd256 = attn_decode_split_gqa_f16_hd256[6]
