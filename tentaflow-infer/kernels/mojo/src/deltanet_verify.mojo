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
from std.math import rsqrt, exp, log
from src.reduce import block_reduce_sum


@always_inline
def _deltanet_conv_source_f16(
    conv_initial: UnsafePointer[Float16, MutAnyOrigin],
    qkv_mixed: UnsafePointer[Float16, MutAnyOrigin],
    channel: Int,
    position: Int,
    conv_dim: Int,
    window: Int,
) -> Float32:
    if position < window:
        return Float32(conv_initial[channel * window + position])
    return Float32(qkv_mixed[(position - window) * conv_dim + channel])


@always_inline
def _deltanet_conv_value_f16(
    conv_initial: UnsafePointer[Float16, MutAnyOrigin],
    qkv_mixed: UnsafePointer[Float16, MutAnyOrigin],
    conv_weight: UnsafePointer[Float16, MutAnyOrigin],
    channel: Int,
    token: Int,
    conv_dim: Int,
    d_conv: Int,
) -> Float32:
    window = d_conv - 1
    var acc: Float32 = 0.0
    for tap in range(d_conv):
        acc += Float32(conv_weight[channel * d_conv + tap]) * _deltanet_conv_source_f16(
            conv_initial, qkv_mixed, channel, token + tap, conv_dim, window
        )
    return Float32(Float16(acc / (1.0 + exp(-acc))))


@always_inline
def _deltanet_store_conv_checkpoint_f16(
    checkpoints: UnsafePointer[Float16, MutAnyOrigin],
    conv_initial: UnsafePointer[Float16, MutAnyOrigin],
    qkv_mixed: UnsafePointer[Float16, MutAnyOrigin],
    channel: Int,
    token: Int,
    conv_dim: Int,
    d_conv: Int,
):
    window = d_conv - 1
    checkpoint_base = (token * conv_dim + channel) * window
    for slot in range(window):
        checkpoints[checkpoint_base + slot] = Float16(_deltanet_conv_source_f16(
            conv_initial, qkv_mixed, channel, token + slot + 1, conv_dim, window
        ))


def _deltanet_prepare_f16[steps: Int](
    q_out: UnsafePointer[Float16, MutAnyOrigin],
    k_out: UnsafePointer[Float16, MutAnyOrigin],
    v_out: UnsafePointer[Float16, MutAnyOrigin],
    g_out: UnsafePointer[Float32, MutAnyOrigin],
    beta_out: UnsafePointer[Float32, MutAnyOrigin],
    conv_checkpoints: UnsafePointer[Float16, MutAnyOrigin],
    conv_initial: UnsafePointer[Float16, MutAnyOrigin],
    qkv_mixed: UnsafePointer[Float16, MutAnyOrigin],
    conv_weight: UnsafePointer[Float16, MutAnyOrigin],
    alpha_raw: UnsafePointer[Float16, MutAnyOrigin],
    beta_raw: UnsafePointer[Float16, MutAnyOrigin],
    dt_bias: UnsafePointer[Float16, MutAnyOrigin],
    a_scale: UnsafePointer[Float16, MutAnyOrigin],
    n_k_heads: Int,
    n_v_heads: Int,
    d_state: Int,
    d_conv: Int,
    eps: Float32,
):
    """Scala splot, podział QKV, normalizację, repeat oraz bramki dla T=2-4."""
    block = Int(block_idx.x)
    lane = Int(thread_idx.x)
    active = lane < d_state
    conv_dim = (2 * n_k_heads + n_v_heads) * d_state

    if block < n_k_heads:
        q_channel = block * d_state + lane
        k_channel = n_k_heads * d_state + block * d_state + lane
        repeats = n_v_heads // n_k_heads
        comptime for token in range(steps):
            var q_value: Float32 = 0.0
            var k_value: Float32 = 0.0
            if active:
                q_value = _deltanet_conv_value_f16(
                    conv_initial, qkv_mixed, conv_weight, q_channel, token, conv_dim, d_conv
                )
                k_value = _deltanet_conv_value_f16(
                    conv_initial, qkv_mixed, conv_weight, k_channel, token, conv_dim, d_conv
                )
                _deltanet_store_conv_checkpoint_f16(
                    conv_checkpoints, conv_initial, qkv_mixed, q_channel, token, conv_dim, d_conv
                )
                _deltanet_store_conv_checkpoint_f16(
                    conv_checkpoints, conv_initial, qkv_mixed, k_channel, token, conv_dim, d_conv
                )
            q_inv = rsqrt(block_reduce_sum(q_value * q_value) + eps)
            k_inv = rsqrt(block_reduce_sum(k_value * k_value) + eps)
            if active:
                for repeat in range(repeats):
                    out_head = repeat * n_k_heads + block
                    out_index = (token * n_v_heads + out_head) * d_state + lane
                    q_out[out_index] = Float16(q_value * q_inv)
                    k_out[out_index] = Float16(k_value * k_inv)
    else:
        v_head = block - n_k_heads
        if v_head >= n_v_heads:
            return
        v_channel = 2 * n_k_heads * d_state + v_head * d_state + lane
        comptime for token in range(steps):
            if active:
                value = _deltanet_conv_value_f16(
                    conv_initial, qkv_mixed, conv_weight, v_channel, token, conv_dim, d_conv
                )
                out_index = (token * n_v_heads + v_head) * d_state + lane
                v_out[out_index] = Float16(value)
                _deltanet_store_conv_checkpoint_f16(
                    conv_checkpoints, conv_initial, qkv_mixed, v_channel, token, conv_dim, d_conv
                )
            if lane == 0:
                gate_index = token * n_v_heads + v_head
                alpha = Float32(alpha_raw[gate_index]) + Float32(dt_bias[v_head])
                softplus = alpha if alpha > 20.0 else log(1.0 + exp(alpha))
                g_out[gate_index] = softplus * Float32(a_scale[v_head])
                beta_out[gate_index] = 1.0 / (1.0 + exp(-Float32(beta_raw[gate_index])))


def deltanet_prepare_t2_f16(
    q_out: UnsafePointer[Float16, MutAnyOrigin], k_out: UnsafePointer[Float16, MutAnyOrigin],
    v_out: UnsafePointer[Float16, MutAnyOrigin], g_out: UnsafePointer[Float32, MutAnyOrigin],
    beta_out: UnsafePointer[Float32, MutAnyOrigin], conv_checkpoints: UnsafePointer[Float16, MutAnyOrigin],
    conv_initial: UnsafePointer[Float16, MutAnyOrigin], qkv_mixed: UnsafePointer[Float16, MutAnyOrigin],
    conv_weight: UnsafePointer[Float16, MutAnyOrigin], alpha_raw: UnsafePointer[Float16, MutAnyOrigin],
    beta_raw: UnsafePointer[Float16, MutAnyOrigin], dt_bias: UnsafePointer[Float16, MutAnyOrigin],
    a_scale: UnsafePointer[Float16, MutAnyOrigin], n_k_heads: Int, n_v_heads: Int,
    d_state: Int, d_conv: Int, eps: Float32,
):
    _deltanet_prepare_f16[2](q_out, k_out, v_out, g_out, beta_out, conv_checkpoints, conv_initial, qkv_mixed, conv_weight, alpha_raw, beta_raw, dt_bias, a_scale, n_k_heads, n_v_heads, d_state, d_conv, eps)


def deltanet_prepare_t3_f16(
    q_out: UnsafePointer[Float16, MutAnyOrigin], k_out: UnsafePointer[Float16, MutAnyOrigin],
    v_out: UnsafePointer[Float16, MutAnyOrigin], g_out: UnsafePointer[Float32, MutAnyOrigin],
    beta_out: UnsafePointer[Float32, MutAnyOrigin], conv_checkpoints: UnsafePointer[Float16, MutAnyOrigin],
    conv_initial: UnsafePointer[Float16, MutAnyOrigin], qkv_mixed: UnsafePointer[Float16, MutAnyOrigin],
    conv_weight: UnsafePointer[Float16, MutAnyOrigin], alpha_raw: UnsafePointer[Float16, MutAnyOrigin],
    beta_raw: UnsafePointer[Float16, MutAnyOrigin], dt_bias: UnsafePointer[Float16, MutAnyOrigin],
    a_scale: UnsafePointer[Float16, MutAnyOrigin], n_k_heads: Int, n_v_heads: Int,
    d_state: Int, d_conv: Int, eps: Float32,
):
    _deltanet_prepare_f16[3](q_out, k_out, v_out, g_out, beta_out, conv_checkpoints, conv_initial, qkv_mixed, conv_weight, alpha_raw, beta_raw, dt_bias, a_scale, n_k_heads, n_v_heads, d_state, d_conv, eps)


def deltanet_prepare_t4_f16(
    q_out: UnsafePointer[Float16, MutAnyOrigin], k_out: UnsafePointer[Float16, MutAnyOrigin],
    v_out: UnsafePointer[Float16, MutAnyOrigin], g_out: UnsafePointer[Float32, MutAnyOrigin],
    beta_out: UnsafePointer[Float32, MutAnyOrigin], conv_checkpoints: UnsafePointer[Float16, MutAnyOrigin],
    conv_initial: UnsafePointer[Float16, MutAnyOrigin], qkv_mixed: UnsafePointer[Float16, MutAnyOrigin],
    conv_weight: UnsafePointer[Float16, MutAnyOrigin], alpha_raw: UnsafePointer[Float16, MutAnyOrigin],
    beta_raw: UnsafePointer[Float16, MutAnyOrigin], dt_bias: UnsafePointer[Float16, MutAnyOrigin],
    a_scale: UnsafePointer[Float16, MutAnyOrigin], n_k_heads: Int, n_v_heads: Int,
    d_state: Int, d_conv: Int, eps: Float32,
):
    _deltanet_prepare_f16[4](q_out, k_out, v_out, g_out, beta_out, conv_checkpoints, conv_initial, qkv_mixed, conv_weight, alpha_raw, beta_raw, dt_bias, a_scale, n_k_heads, n_v_heads, d_state, d_conv, eps)


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


def _deltanet_gated_scan_d128_f16[steps: Int](
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
    """Dzieli kolumny stanu d_state<=128 na niezależne kafle wielkości bloku."""
    tid = Int(thread_idx.x)
    tile_width = Int(block_dim.x)
    tiles = (d_state + tile_width - 1) // tile_width
    block = Int(block_idx.x)
    head = block // tiles
    tile = block % tiles
    if head >= n_v_heads:
        return
    j = tile * tile_width + tid
    active = j < d_state

    sk = stack_allocation[128, Float32, address_space=AddressSpace.SHARED]()
    sq = stack_allocation[128, Float32, address_space=AddressSpace.SHARED]()
    state_elements = n_v_heads * d_state * d_state
    head_state = head * d_state * d_state
    head_vector = head * d_state
    inv_sqrt = rsqrt(Float32(d_state))

    comptime for token in range(steps):
        vector_base = token * n_v_heads * d_state + head_vector
        gate_base = token * n_v_heads + head
        var i = tid
        while i < d_state:
            sk[i] = Float32(k_in[vector_base + i])
            sq[i] = Float32(q_in[vector_base + i])
            i += tile_width
        barrier()

        if active:
            checkpoint_base = token * state_elements + head_state
            previous_base = head_state if token == 0 else checkpoint_base - state_elements
            decay = exp(g_in[gate_base])
            beta = beta_in[gate_base]
            var kv: Float32 = 0.0
            for key in range(d_state):
                offset = key * d_state + j
                s = (state_in[previous_base + offset] if token == 0 else checkpoints[previous_base + offset]) * decay
                checkpoints[checkpoint_base + offset] = s
                kv += sk[key] * s
            dj = beta * (Float32(v_in[vector_base + j]) - kv)

            var output: Float32 = 0.0
            for key in range(d_state):
                offset = key * d_state + j
                s = checkpoints[checkpoint_base + offset] + sk[key] * dj
                checkpoints[checkpoint_base + offset] = s
                output += sq[key] * s
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


def deltanet_gated_scan_t3_d128_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin], checkpoints: UnsafePointer[Float32, MutAnyOrigin],
    state_in: UnsafePointer[Float32, MutAnyOrigin], q_in: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin], v_in: UnsafePointer[Float16, MutAnyOrigin],
    g_in: UnsafePointer[Float32, MutAnyOrigin], beta_in: UnsafePointer[Float32, MutAnyOrigin],
    n_v_heads: Int, d_state: Int,
):
    _deltanet_gated_scan_d128_f16[3](out_ptr, checkpoints, state_in, q_in, k_in, v_in, g_in, beta_in, n_v_heads, d_state)


def deltanet_gated_scan_t4_d128_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin], checkpoints: UnsafePointer[Float32, MutAnyOrigin],
    state_in: UnsafePointer[Float32, MutAnyOrigin], q_in: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin], v_in: UnsafePointer[Float16, MutAnyOrigin],
    g_in: UnsafePointer[Float32, MutAnyOrigin], beta_in: UnsafePointer[Float32, MutAnyOrigin],
    n_v_heads: Int, d_state: Int,
):
    _deltanet_gated_scan_d128_f16[4](out_ptr, checkpoints, state_in, q_in, k_in, v_in, g_in, beta_in, n_v_heads, d_state)


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
