# =============================================================================
# Plik: deltanet_prepare_tiled.mojo
# Opis: Przygotowuje pełny prefill Gated-DeltaNet d128/dconv4 w niezależnych
#       kaflach 32 tokenów, łącząc splot, SiLU, normalizację i bramki.
# Przykład: deltanet_prepare_tiled_d128_c4_f16 obsługuje prefill T=2048.
# =============================================================================

from std.gpu import block_idx, thread_idx
from std.gpu.memory import AddressSpace
from std.gpu.sync import barrier
from std.math import exp, log, rsqrt
from std.memory import stack_allocation
from src.reduce import block_reduce_sum

comptime D_STATE = 128
comptime D_CONV = 4
comptime WINDOW = D_CONV - 1
comptime TOKEN_TILE = 32
comptime SOURCE_ROWS = TOKEN_TILE + WINDOW
comptime SHARED_PLANE = SOURCE_ROWS * D_STATE


@always_inline
def _source_f16(
    conv_initial: UnsafePointer[Float16, MutAnyOrigin],
    qkv_mixed: UnsafePointer[Float16, MutAnyOrigin],
    channel: Int,
    position: Int,
    conv_dim: Int,
) -> Float16:
    if position < WINDOW:
        return conv_initial[channel * WINDOW + position]
    return qkv_mixed[(position - WINDOW) * conv_dim + channel]


@always_inline
def _silu_f16(value: Float32) -> Float32:
    return Float32(Float16(value / (1.0 + exp(-value))))


def deltanet_prepare_tiled_d128_c4_f16(
    q_out: UnsafePointer[Float16, MutAnyOrigin],
    k_out: UnsafePointer[Float16, MutAnyOrigin],
    v_out: UnsafePointer[Float16, MutAnyOrigin],
    g_out: UnsafePointer[Float32, MutAnyOrigin],
    beta_out: UnsafePointer[Float32, MutAnyOrigin],
    conv_final: UnsafePointer[Float16, MutAnyOrigin],
    conv_initial: UnsafePointer[Float16, MutAnyOrigin],
    qkv_mixed: UnsafePointer[Float16, MutAnyOrigin],
    conv_weight: UnsafePointer[Float16, MutAnyOrigin],
    alpha_raw: UnsafePointer[Float16, MutAnyOrigin],
    beta_raw: UnsafePointer[Float16, MutAnyOrigin],
    dt_bias: UnsafePointer[Float16, MutAnyOrigin],
    a_scale: UnsafePointer[Float16, MutAnyOrigin],
    n_steps: Int,
    n_k_heads: Int,
    n_v_heads: Int,
    d_state: Int,
    d_conv: Int,
    eps: Float32,
):
    """Przetwarza jedną głowę Q/K albo V i jeden kafel czasu."""
    if d_state != D_STATE or d_conv != D_CONV:
        return
    head_block = Int(block_idx.x)
    time_start = Int(block_idx.y) * TOKEN_TILE
    thread = Int(thread_idx.x)
    if thread >= D_STATE or time_start >= n_steps:
        return

    conv_dim = (2 * n_k_heads + n_v_heads) * D_STATE
    valid_tokens = min(TOKEN_TILE, n_steps - time_start)
    shared = stack_allocation[
        2 * SHARED_PLANE, Float16, address_space=AddressSpace.SHARED
    ]()
    is_qk = head_block < n_k_heads
    value_head = head_block - n_k_heads
    if not is_qk and value_head >= n_v_heads:
        return
    first_channel = (
        head_block * D_STATE + thread
        if is_qk
        else 2 * n_k_heads * D_STATE + value_head * D_STATE + thread
    )
    second_channel = n_k_heads * D_STATE + head_block * D_STATE + thread

    comptime for row in range(SOURCE_ROWS):
        position = time_start + row
        if row < valid_tokens + WINDOW:
            shared[row * D_STATE + thread] = _source_f16(
                conv_initial, qkv_mixed, first_channel, position, conv_dim
            )
            if is_qk:
                shared[SHARED_PLANE + row * D_STATE + thread] = _source_f16(
                    conv_initial, qkv_mixed, second_channel, position, conv_dim
                )
        else:
            shared[row * D_STATE + thread] = Float16(0.0)
            if is_qk:
                shared[SHARED_PLANE + row * D_STATE + thread] = Float16(0.0)

    if not is_qk and thread < TOKEN_TILE:
        token = time_start + thread
        if token < n_steps:
            gate_index = token * n_v_heads + value_head
            alpha = Float32(alpha_raw[gate_index]) + Float32(dt_bias[value_head])
            softplus = alpha if alpha > 20.0 else log(1.0 + exp(alpha))
            g_out[gate_index] = softplus * Float32(a_scale[value_head])
            beta_out[gate_index] = 1.0 / (1.0 + exp(-Float32(beta_raw[gate_index])))

    if time_start + TOKEN_TILE >= n_steps:
        comptime for slot in range(WINDOW):
            conv_final[first_channel * WINDOW + slot] = _source_f16(
                conv_initial, qkv_mixed, first_channel, n_steps + slot, conv_dim
            )
            if is_qk:
                conv_final[second_channel * WINDOW + slot] = _source_f16(
                    conv_initial, qkv_mixed, second_channel, n_steps + slot, conv_dim
                )
    barrier()

    var first_weight = InlineArray[Float32, D_CONV](fill=0.0)
    var second_weight = InlineArray[Float32, D_CONV](fill=0.0)
    comptime for tap in range(D_CONV):
        first_weight[tap] = Float32(conv_weight[first_channel * D_CONV + tap])
        if is_qk:
            second_weight[tap] = Float32(conv_weight[second_channel * D_CONV + tap])

    for local_token in range(TOKEN_TILE):
        token = time_start + local_token
        if token < n_steps:
            var first_acc: Float32 = 0.0
            var second_acc: Float32 = 0.0
            comptime for tap in range(D_CONV):
                first_acc += first_weight[tap] * Float32(
                    shared[(local_token + tap) * D_STATE + thread]
                )
                if is_qk:
                    second_acc += second_weight[tap] * Float32(
                        shared[SHARED_PLANE + (local_token + tap) * D_STATE + thread]
                    )
            first_value = _silu_f16(first_acc)
            if is_qk:
                second_value = _silu_f16(second_acc)
                first_inv = rsqrt(block_reduce_sum(first_value * first_value) + eps)
                second_inv = rsqrt(block_reduce_sum(second_value * second_value) + eps)
                repeats = n_v_heads // n_k_heads
                for repeat in range(repeats):
                    out_head = repeat * n_k_heads + head_block
                    out_index = (token * n_v_heads + out_head) * D_STATE + thread
                    q_out[out_index] = Float16(first_value * first_inv)
                    k_out[out_index] = Float16(second_value * second_inv)
            else:
                out_index = (token * n_v_heads + value_head) * D_STATE + thread
                v_out[out_index] = Float16(first_value)
