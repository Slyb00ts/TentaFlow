# =============================================================================
# Plik: deltanet_scan_persistent.mojo
# Opis: Wykonuje pełny skan Gated-DeltaNet d128 ze stanem utrzymywanym
#       w rejestrach i dwoma kolumnami stanu przypisanymi do warpa.
# Przykład: deltanet_gated_scan_persistent_d128_f16 skanuje cały prefill.
# =============================================================================

from std.gpu import WARP_SIZE, block_idx, thread_idx
from std.gpu.primitives import warp
from std.math import exp, rsqrt

comptime WARPS_PER_BLOCK = 2
comptime D_STATE = 128
comptime ROWS_PER_LANE = 4
comptime COLUMNS_PER_WARP = 2
comptime COLUMN_TILES = D_STATE // (WARPS_PER_BLOCK * COLUMNS_PER_WARP)


def deltanet_gated_scan_persistent_d128_f16(
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
    """Skanuje tokeny kolejno, rozdzielając iloczyny skalarne na cały warp."""
    comptime if WARP_SIZE != 32:
        return

    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    warp_id = tid // WARP_SIZE
    block = Int(block_idx.x)
    head = block // COLUMN_TILES
    column_tile = block % COLUMN_TILES
    first_column = (column_tile * WARPS_PER_BLOCK + warp_id) * COLUMNS_PER_WARP
    if head >= n_v_heads or warp_id >= WARPS_PER_BLOCK or d_state != D_STATE:
        return

    head_state = head * D_STATE * D_STATE
    head_vector = head * D_STATE
    inv_sqrt = rsqrt(Float32(D_STATE))
    var state = InlineArray[Float32, ROWS_PER_LANE * COLUMNS_PER_WARP](fill=0.0)
    comptime for column_offset in range(COLUMNS_PER_WARP):
        comptime for row in range(ROWS_PER_LANE):
            key = row * WARP_SIZE + lane
            state[column_offset * ROWS_PER_LANE + row] = state_io[
                head_state + key * D_STATE + first_column + column_offset
            ]

    for token in range(n_steps):
        vector_base = token * n_v_heads * D_STATE + head_vector
        gate_index = token * n_v_heads + head
        decay = exp(g_in[gate_index])
        beta = beta_in[gate_index]
        var key = InlineArray[Float32, ROWS_PER_LANE](fill=0.0)
        var query = InlineArray[Float32, ROWS_PER_LANE](fill=0.0)
        comptime for row in range(ROWS_PER_LANE):
            index = vector_base + row * WARP_SIZE + lane
            key[row] = Float32(k_in[index])
            query[row] = Float32(q_in[index])

        comptime for column_offset in range(COLUMNS_PER_WARP):
            var partial: Float32 = 0.0
            comptime for row in range(ROWS_PER_LANE):
                partial += state[column_offset * ROWS_PER_LANE + row] * key[row]
            dot = warp.sum(partial)
            column = first_column + column_offset
            delta = beta * (Float32(v_in[vector_base + column]) - decay * dot)

            partial = 0.0
            comptime for row in range(ROWS_PER_LANE):
                state_index = column_offset * ROWS_PER_LANE + row
                state[state_index] = decay * state[state_index] + key[row] * delta
                partial += state[state_index] * query[row]
            output = warp.sum(partial) * inv_sqrt
            if lane == 0:
                out_ptr[vector_base + column] = Float16(output)

    comptime for column_offset in range(COLUMNS_PER_WARP):
        comptime for row in range(ROWS_PER_LANE):
            key = row * WARP_SIZE + lane
            state_io[head_state + key * D_STATE + first_column + column_offset] = state[
                column_offset * ROWS_PER_LANE + row
            ]
