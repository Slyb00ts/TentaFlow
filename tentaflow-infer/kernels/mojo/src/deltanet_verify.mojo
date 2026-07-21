# =============================================================================
# Plik: deltanet_verify.mojo
# Opis: Wykonuje krótki przyczynowy skan Gated-DeltaNet i zapisuje checkpoint
#       stanu po każdym tokenie bez modyfikowania stanu wejściowego.
# Przykład: deltanet_gated_scan_t4_f16 zapisuje cztery stany do zatwierdzenia.
# =============================================================================

from std.gpu import block_dim, block_idx, thread_idx, grid_dim
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import rsqrt, exp


def _deltanet_gated_scan_f16[steps: Int](
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    checkpoints: UnsafePointer[Float32, MutAnyOrigin],
    state_in: UnsafePointer[Float32, MutAnyOrigin],
    q_in: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    g_in: UnsafePointer[Float32, MutAnyOrigin],
    beta_in: UnsafePointer[Float32, MutAnyOrigin],
    n_v_heads: Int,
    d_state: Int,
):
    head = Int(block_idx.x)
    j = Int(thread_idx.x)
    if head >= n_v_heads or j >= d_state:
        return

    sk = stack_allocation[1024, Float32, address_space = AddressSpace.SHARED]()
    sq = stack_allocation[1024, Float32, address_space = AddressSpace.SHARED]()
    state_elements = n_v_heads * d_state * d_state
    head_state = head * d_state * d_state
    head_vector = head * d_state
    inv_sqrt = rsqrt(Float32(d_state))

    comptime for token in range(steps):
        vector_base = token * n_v_heads * d_state + head_vector
        gate_base = token * n_v_heads + head
        sk[j] = Float32(k_in[vector_base + j])
        sq[j] = Float32(q_in[vector_base + j])
        barrier()

        checkpoint_base = token * state_elements + head_state
        previous_base = head_state if token == 0 else checkpoint_base - state_elements
        decay = exp(g_in[gate_base])
        beta = beta_in[gate_base]

        var kv: Float32 = 0.0
        for i in range(d_state):
            offset = i * d_state + j
            s = (state_in[previous_base + offset] if token == 0 else checkpoints[previous_base + offset]) * decay
            checkpoints[checkpoint_base + offset] = s
            kv += sk[i] * s
        dj = beta * (Float32(v_in[vector_base + j]) - kv)

        var output: Float32 = 0.0
        for i in range(d_state):
            offset = i * d_state + j
            s = checkpoints[checkpoint_base + offset] + sk[i] * dj
            checkpoints[checkpoint_base + offset] = s
            output += sq[i] * s
        out_ptr[vector_base + j] = Float16(output * inv_sqrt)
        barrier()


def deltanet_gated_scan_t2_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    checkpoints: UnsafePointer[Float32, MutAnyOrigin],
    state_in: UnsafePointer[Float32, MutAnyOrigin],
    q_in: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    g_in: UnsafePointer[Float32, MutAnyOrigin],
    beta_in: UnsafePointer[Float32, MutAnyOrigin],
    n_v_heads: Int,
    d_state: Int,
):
    _deltanet_gated_scan_f16[2](out_ptr, checkpoints, state_in, q_in, k_in, v_in, g_in, beta_in, n_v_heads, d_state)


def deltanet_gated_scan_t3_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    checkpoints: UnsafePointer[Float32, MutAnyOrigin],
    state_in: UnsafePointer[Float32, MutAnyOrigin],
    q_in: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    g_in: UnsafePointer[Float32, MutAnyOrigin],
    beta_in: UnsafePointer[Float32, MutAnyOrigin],
    n_v_heads: Int,
    d_state: Int,
):
    _deltanet_gated_scan_f16[3](out_ptr, checkpoints, state_in, q_in, k_in, v_in, g_in, beta_in, n_v_heads, d_state)


def deltanet_gated_scan_t4_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    checkpoints: UnsafePointer[Float32, MutAnyOrigin],
    state_in: UnsafePointer[Float32, MutAnyOrigin],
    q_in: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    g_in: UnsafePointer[Float32, MutAnyOrigin],
    beta_in: UnsafePointer[Float32, MutAnyOrigin],
    n_v_heads: Int,
    d_state: Int,
):
    _deltanet_gated_scan_f16[4](out_ptr, checkpoints, state_in, q_in, k_in, v_in, g_in, beta_in, n_v_heads, d_state)


def deltanet_commit_checkpoint_f32(
    state_out: UnsafePointer[Float32, MutAnyOrigin],
    checkpoints: UnsafePointer[Float32, MutAnyOrigin],
    accepted_ptr: UnsafePointer[Int32, MutAnyOrigin],
    state_elements: Int,
    max_steps: Int,
):
    accepted = Int(accepted_ptr[0])
    if accepted <= 0 or accepted > max_steps:
        return
    source_base = (accepted - 1) * state_elements
    index = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    stride = Int(grid_dim.x) * Int(block_dim.x)
    while index < state_elements:
        state_out[index] = checkpoints[source_base + index]
        index += stride
