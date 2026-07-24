# =============================================================================
# Plik: deltanet_value_key.mojo
# Opis: Obsługuje stan Gated-DeltaNet w układzie [głowa, kolumna, klucz],
#       utrzymując kolumny stanu w rejestrach podczas skanu.
# Przykład: deltanet_value_key_scan_inplace_f16 wykonuje decode albo prefill.
# =============================================================================

from std.gpu import WARP_SIZE, block_idx, thread_idx
from std.gpu.primitives import warp
from std.math import exp, fma, rsqrt

comptime D_STATE = 128
comptime WARPS_PER_BLOCK = 4
comptime ROWS_PER_LANE = D_STATE // WARP_SIZE
comptime COLUMN_TILES = D_STATE // WARPS_PER_BLOCK


@always_inline
def _scan_coordinates(n_v_heads: Int) -> Tuple[Int, Int, Int, Int, Bool]:
    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    warp_id = tid // WARP_SIZE
    block = Int(block_idx.x)
    head = block // COLUMN_TILES
    column = (block % COLUMN_TILES) * WARPS_PER_BLOCK + warp_id
    return lane, warp_id, head, column, head < n_v_heads and warp_id < WARPS_PER_BLOCK


def deltanet_value_key_scan_inplace_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    state_out: UnsafePointer[Float32, MutAnyOrigin],
    state_in: UnsafePointer[Float32, MutAnyOrigin],
    q_in: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    g_in: UnsafePointer[Float32, MutAnyOrigin],
    beta_in: UnsafePointer[Float32, MutAnyOrigin],
    n_sequences: Int,
    n_steps: Int,
    n_v_heads: Int,
    d_state: Int,
):
    """Skanuje `[B,T]`, zapisując wyłącznie końcowy stan każdej sekwencji."""
    comptime if D_STATE % WARP_SIZE != 0:
        return
    lane, warp_id, head, column, active = _scan_coordinates(n_v_heads)
    sequence = Int(block_idx.y)
    if not active or sequence >= n_sequences or d_state != D_STATE:
        return

    state_elements = n_v_heads * D_STATE * D_STATE
    vector_elements = n_steps * n_v_heads * D_STATE
    gate_elements = n_steps * n_v_heads
    state_base = sequence * state_elements + head * D_STATE * D_STATE + column * D_STATE
    vector_base = sequence * vector_elements
    gate_base = sequence * gate_elements
    head_vector = head * D_STATE
    inv_sqrt = rsqrt(Float32(D_STATE))
    var state = InlineArray[Float32, ROWS_PER_LANE](fill=0.0)

    comptime for row in range(ROWS_PER_LANE):
        key_index = row * WARP_SIZE + lane
        state[row] = state_in[state_base + key_index]

    for token in range(n_steps):
        vector = vector_base + token * n_v_heads * D_STATE + head_vector
        gate = gate_base + token * n_v_heads + head
        decay = exp(g_in[gate])
        beta = beta_in[gate]
        var key = InlineArray[Float32, ROWS_PER_LANE](fill=0.0)
        var query = InlineArray[Float32, ROWS_PER_LANE](fill=0.0)
        var partial: Float32 = 0.0
        comptime for row in range(ROWS_PER_LANE):
            key_index = row * WARP_SIZE + lane
            key[row] = Float32(k_in[vector + key_index])
            query[row] = Float32(q_in[vector + key_index])
            state[row] = fma(state[row], decay, 0.0)
            partial += key[row] * state[row]
        predicted = warp.sum(partial)
        delta = beta * (Float32(v_in[vector + column]) - predicted)

        partial = 0.0
        comptime for row in range(ROWS_PER_LANE):
            state[row] += key[row] * delta
            partial += query[row] * state[row]
        output = warp.sum(partial)
        if lane == 0:
            out_ptr[vector + column] = Float16(output * inv_sqrt)

    comptime for row in range(ROWS_PER_LANE):
        key_index = row * WARP_SIZE + lane
        state_out[state_base + key_index] = state[row]


def deltanet_value_key_scan_persistent_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    state_io: UnsafePointer[Float32, MutAnyOrigin],
    q_in: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    g_in: UnsafePointer[Float32, MutAnyOrigin],
    beta_in: UnsafePointer[Float32, MutAnyOrigin],
    n_steps: Int,
    n_v_heads: Int,
    d_state: Int,
):
    """Skanuje długi prefill, przetwarzając dwie kolumny na warp."""
    comptime PREFILL_WARPS = 2
    comptime COLUMNS_PER_WARP = 2
    comptime PREFILL_TILES = D_STATE // (PREFILL_WARPS * COLUMNS_PER_WARP)
    comptime if D_STATE % WARP_SIZE != 0:
        return

    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    warp_id = tid // WARP_SIZE
    block = Int(block_idx.x)
    head = block // PREFILL_TILES
    first_column = (block % PREFILL_TILES * PREFILL_WARPS + warp_id) * COLUMNS_PER_WARP
    if head >= n_v_heads or warp_id >= PREFILL_WARPS or d_state != D_STATE:
        return

    state_head = head * D_STATE * D_STATE
    head_vector = head * D_STATE
    inv_sqrt = rsqrt(Float32(D_STATE))
    var state = InlineArray[Float32, ROWS_PER_LANE * COLUMNS_PER_WARP](fill=0.0)
    comptime for column_offset in range(COLUMNS_PER_WARP):
        comptime for row in range(ROWS_PER_LANE):
            key_index = row * WARP_SIZE + lane
            state[column_offset * ROWS_PER_LANE + row] = state_io[
                state_head + (first_column + column_offset) * D_STATE + key_index
            ]

    for token in range(n_steps):
        vector = token * n_v_heads * D_STATE + head_vector
        gate = token * n_v_heads + head
        decay = exp(g_in[gate])
        beta = beta_in[gate]
        var key = InlineArray[Float32, ROWS_PER_LANE](fill=0.0)
        var query = InlineArray[Float32, ROWS_PER_LANE](fill=0.0)
        comptime for row in range(ROWS_PER_LANE):
            key_index = row * WARP_SIZE + lane
            key[row] = Float32(k_in[vector + key_index])
            query[row] = Float32(q_in[vector + key_index])

        comptime for column_offset in range(COLUMNS_PER_WARP):
            var partial: Float32 = 0.0
            comptime for row in range(ROWS_PER_LANE):
                index = column_offset * ROWS_PER_LANE + row
                state[index] = fma(state[index], decay, 0.0)
                partial += key[row] * state[index]
            predicted = warp.sum(partial)
            column = first_column + column_offset
            delta = beta * (Float32(v_in[vector + column]) - predicted)

            partial = 0.0
            comptime for row in range(ROWS_PER_LANE):
                index = column_offset * ROWS_PER_LANE + row
                state[index] += key[row] * delta
                partial += query[row] * state[index]
            output = warp.sum(partial)
            if lane == 0:
                out_ptr[vector + column] = Float16(output * inv_sqrt)

    comptime for column_offset in range(COLUMNS_PER_WARP):
        comptime for row in range(ROWS_PER_LANE):
            key_index = row * WARP_SIZE + lane
            state_io[state_head + (first_column + column_offset) * D_STATE + key_index] = state[
                column_offset * ROWS_PER_LANE + row
            ]


def deltanet_value_key_scan_checkpoints_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    checkpoints: UnsafePointer[Float32, MutAnyOrigin],
    state_in: UnsafePointer[Float32, MutAnyOrigin],
    q_in: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    g_in: UnsafePointer[Float32, MutAnyOrigin],
    beta_in: UnsafePointer[Float32, MutAnyOrigin],
    n_sequences: Int,
    n_steps: Int,
    n_v_heads: Int,
    d_state: Int,
):
    """Skanuje `[B,T]` i zapisuje stan ValueKey po każdym tokenie."""
    comptime if D_STATE % WARP_SIZE != 0:
        return
    lane, warp_id, head, column, active = _scan_coordinates(n_v_heads)
    sequence = Int(block_idx.y)
    if not active or sequence >= n_sequences or d_state != D_STATE:
        return

    state_elements = n_v_heads * D_STATE * D_STATE
    vector_elements = n_steps * n_v_heads * D_STATE
    gate_elements = n_steps * n_v_heads
    state_base = sequence * state_elements + head * D_STATE * D_STATE + column * D_STATE
    vector_base = sequence * vector_elements
    gate_base = sequence * gate_elements
    head_vector = head * D_STATE
    inv_sqrt = rsqrt(Float32(D_STATE))
    var state = InlineArray[Float32, ROWS_PER_LANE](fill=0.0)

    comptime for row in range(ROWS_PER_LANE):
        key_index = row * WARP_SIZE + lane
        state[row] = state_in[state_base + key_index]

    for token in range(n_steps):
        vector = vector_base + token * n_v_heads * D_STATE + head_vector
        gate = gate_base + token * n_v_heads + head
        decay = exp(g_in[gate])
        beta = beta_in[gate]
        var key = InlineArray[Float32, ROWS_PER_LANE](fill=0.0)
        var query = InlineArray[Float32, ROWS_PER_LANE](fill=0.0)
        var partial: Float32 = 0.0
        comptime for row in range(ROWS_PER_LANE):
            key_index = row * WARP_SIZE + lane
            key[row] = Float32(k_in[vector + key_index])
            query[row] = Float32(q_in[vector + key_index])
            state[row] = fma(state[row], decay, 0.0)
            partial += key[row] * state[row]
        predicted = warp.sum(partial)
        delta = beta * (Float32(v_in[vector + column]) - predicted)

        partial = 0.0
        checkpoint_base = (
            (sequence * n_steps + token) * state_elements
            + head * D_STATE * D_STATE
            + column * D_STATE
        )
        comptime for row in range(ROWS_PER_LANE):
            key_index = row * WARP_SIZE + lane
            state[row] += key[row] * delta
            checkpoints[checkpoint_base + key_index] = state[row]
            partial += query[row] * state[row]
        output = warp.sum(partial)
        if lane == 0:
            out_ptr[vector + column] = Float16(output * inv_sqrt)


def deltanet_value_key_commit_recompute_f32(
    state_out: UnsafePointer[Float32, MutAnyOrigin],
    state_in: UnsafePointer[Float32, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    g_in: UnsafePointer[Float32, MutAnyOrigin],
    beta_in: UnsafePointer[Float32, MutAnyOrigin],
    decisions: UnsafePointer[Int32, MutAnyOrigin],
    n_sequences: Int,
    max_steps: Int,
    n_v_heads: Int,
    d_state: Int,
):
    """Odtwarza zaakceptowany prefiks i zapisuje końcowy stan ValueKey."""
    comptime if D_STATE % WARP_SIZE != 0:
        return
    lane, warp_id, head, column, active = _scan_coordinates(n_v_heads)
    sequence = Int(block_idx.y)
    if not active or sequence >= n_sequences or d_state != D_STATE:
        return

    state_elements = n_v_heads * D_STATE * D_STATE
    vector_elements = max_steps * n_v_heads * D_STATE
    gate_elements = max_steps * n_v_heads
    state_base = sequence * state_elements + head * D_STATE * D_STATE + column * D_STATE
    vector_base = sequence * vector_elements
    gate_base = sequence * gate_elements
    head_vector = head * D_STATE
    selected_steps = min(max(Int(decisions[2 * sequence]), 0), max_steps)
    var state = InlineArray[Float32, ROWS_PER_LANE](fill=0.0)

    comptime for row in range(ROWS_PER_LANE):
        key_index = row * WARP_SIZE + lane
        state[row] = state_in[state_base + key_index]

    for token in range(selected_steps):
        vector = vector_base + token * n_v_heads * D_STATE + head_vector
        gate = gate_base + token * n_v_heads + head
        decay = exp(g_in[gate])
        beta = beta_in[gate]
        var key = InlineArray[Float32, ROWS_PER_LANE](fill=0.0)
        var partial: Float32 = 0.0
        comptime for row in range(ROWS_PER_LANE):
            key_index = row * WARP_SIZE + lane
            key[row] = Float32(k_in[vector + key_index])
            state[row] = fma(state[row], decay, 0.0)
            partial += key[row] * state[row]
        predicted = warp.sum(partial)
        delta = beta * (Float32(v_in[vector + column]) - predicted)
        comptime for row in range(ROWS_PER_LANE):
            state[row] += key[row] * delta

    comptime for row in range(ROWS_PER_LANE):
        key_index = row * WARP_SIZE + lane
        state_out[state_base + key_index] = state[row]
