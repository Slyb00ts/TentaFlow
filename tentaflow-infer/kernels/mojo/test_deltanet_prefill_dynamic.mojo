# =============================================================================
# Plik: test_deltanet_prefill_dynamic.mojo
# Opis: Porownuje dynamiczne prepare i scan DeltaNet z sekwencyjna referencja CPU
#       oraz sprawdza niezmiennosc wyniku po podziale 32 tokenow na dwa chunki.
# Przyklad: pixi run mojo test_deltanet_prefill_dynamic.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.math import exp, log, sqrt
from src.deltanet_verify import (
    deltanet_prepare_dynamic_f16,
    deltanet_gated_scan_dynamic_f16,
)


comptime N_K = 2
comptime N_V = 4
comptime D_STATE = 8
comptime D_CONV = 4
comptime WINDOW = D_CONV - 1
comptime CONV_DIM = (2 * N_K + N_V) * D_STATE
comptime VECTOR_ELEMENTS = N_V * D_STATE
comptime STATE_ELEMENTS = N_V * D_STATE * D_STATE
comptime EPS: Float32 = 0.00001


def _conv_source(
    initial: UnsafePointer[Float16, MutUntrackedOrigin],
    mixed: UnsafePointer[Float16, MutUntrackedOrigin],
    channel: Int,
    position: Int,
) -> Float32:
    if position < WINDOW:
        return Float32(initial[channel * WINDOW + position])
    return Float32(mixed[(position - WINDOW) * CONV_DIM + channel])


def _fill_inputs(
    initial: UnsafePointer[Float16, MutUntrackedOrigin],
    mixed: UnsafePointer[Float16, MutUntrackedOrigin],
    weight: UnsafePointer[Float16, MutUntrackedOrigin],
    alpha: UnsafePointer[Float16, MutUntrackedOrigin],
    beta_raw: UnsafePointer[Float16, MutUntrackedOrigin],
    dt_bias: UnsafePointer[Float16, MutUntrackedOrigin],
    a_scale: UnsafePointer[Float16, MutUntrackedOrigin],
    state: UnsafePointer[Float32, MutUntrackedOrigin],
    steps: Int,
):
    for i in range(CONV_DIM * WINDOW):
        initial[i] = Float16(Float32((i * 7 + 3) % 29 - 14) * 0.019)
    for i in range(steps * CONV_DIM):
        mixed[i] = Float16(Float32((i * 11 + 5) % 31 - 15) * 0.017)
    for i in range(CONV_DIM * D_CONV):
        weight[i] = Float16(Float32((i * 13 + 7) % 23 - 11) * 0.021)
    for i in range(steps * N_V):
        alpha[i] = Float16(Float32((i * 5 + 1) % 17 - 8) * 0.09)
        beta_raw[i] = Float16(Float32((i * 3 + 2) % 19 - 9) * 0.08)
    for i in range(N_V):
        dt_bias[i] = Float16(Float32(i - 2) * 0.07)
        a_scale[i] = Float16(-0.18 - Float32(i) * 0.025)
    for i in range(STATE_ELEMENTS):
        state[i] = Float32((i * 17 + 9) % 37 - 18) * 0.0007


def _case(ctx: DeviceContext, steps: Int) raises:
    var q = ctx.enqueue_create_buffer[DType.float16](steps * VECTOR_ELEMENTS)
    var k = ctx.enqueue_create_buffer[DType.float16](steps * VECTOR_ELEMENTS)
    var v = ctx.enqueue_create_buffer[DType.float16](steps * VECTOR_ELEMENTS)
    var g = ctx.enqueue_create_buffer[DType.float32](steps * N_V)
    var beta = ctx.enqueue_create_buffer[DType.float32](steps * N_V)
    var conv_checkpoints = ctx.enqueue_create_buffer[DType.float16](steps * CONV_DIM * WINDOW)
    var initial = ctx.enqueue_create_buffer[DType.float16](CONV_DIM * WINDOW)
    var mixed = ctx.enqueue_create_buffer[DType.float16](steps * CONV_DIM)
    var weight = ctx.enqueue_create_buffer[DType.float16](CONV_DIM * D_CONV)
    var alpha = ctx.enqueue_create_buffer[DType.float16](steps * N_V)
    var beta_raw = ctx.enqueue_create_buffer[DType.float16](steps * N_V)
    var dt_bias = ctx.enqueue_create_buffer[DType.float16](N_V)
    var a_scale = ctx.enqueue_create_buffer[DType.float16](N_V)
    var state = ctx.enqueue_create_buffer[DType.float32](STATE_ELEMENTS)
    var output = ctx.enqueue_create_buffer[DType.float16](steps * VECTOR_ELEMENTS)
    var state_checkpoints = ctx.enqueue_create_buffer[DType.float32](steps * STATE_ELEMENTS)

    with initial.map_to_host() as initial_h, mixed.map_to_host() as mixed_h, weight.map_to_host() as weight_h, alpha.map_to_host() as alpha_h, beta_raw.map_to_host() as beta_raw_h, dt_bias.map_to_host() as dt_h, a_scale.map_to_host() as scale_h, state.map_to_host() as state_h:
        _fill_inputs(initial_h.unsafe_ptr(), mixed_h.unsafe_ptr(), weight_h.unsafe_ptr(), alpha_h.unsafe_ptr(), beta_raw_h.unsafe_ptr(), dt_h.unsafe_ptr(), scale_h.unsafe_ptr(), state_h.unsafe_ptr(), steps)

    ctx.enqueue_function[deltanet_prepare_dynamic_f16](
        q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(),
        conv_checkpoints.unsafe_ptr(), initial.unsafe_ptr(), mixed.unsafe_ptr(), weight.unsafe_ptr(),
        alpha.unsafe_ptr(), beta_raw.unsafe_ptr(), dt_bias.unsafe_ptr(), a_scale.unsafe_ptr(),
        steps, N_K, N_V, D_STATE, D_CONV, EPS,
        grid_dim=N_K + N_V, block_dim=32,
    )
    ctx.enqueue_function[deltanet_gated_scan_dynamic_f16](
        output.unsafe_ptr(), state_checkpoints.unsafe_ptr(), state.unsafe_ptr(),
        q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(),
        steps, N_V, D_STATE, grid_dim=N_V, block_dim=D_STATE,
    )
    ctx.synchronize()

    var max_prepare_error: Float32 = 0.0
    var max_state_error: Float32 = 0.0
    var max_output_error: Float32 = 0.0
    with initial.map_to_host() as initial_h, mixed.map_to_host() as mixed_h, weight.map_to_host() as weight_h, alpha.map_to_host() as alpha_h, beta_raw.map_to_host() as beta_raw_h, dt_bias.map_to_host() as dt_h, a_scale.map_to_host() as scale_h, state.map_to_host() as state_h, q.map_to_host() as q_h, k.map_to_host() as k_h, v.map_to_host() as v_h, g.map_to_host() as g_h, beta.map_to_host() as beta_h, conv_checkpoints.map_to_host() as conv_h, output.map_to_host() as output_h, state_checkpoints.map_to_host() as checkpoints_h:
        var conv = List[Float32]()
        for _ in range(steps * CONV_DIM):
            conv.append(0.0)
        for token in range(steps):
            for channel in range(CONV_DIM):
                var acc: Float32 = 0.0
                for tap in range(D_CONV):
                    acc += Float32(weight_h[channel * D_CONV + tap]) * _conv_source(
                        initial_h.unsafe_ptr(), mixed_h.unsafe_ptr(), channel, token + tap
                    )
                conv[token * CONV_DIM + channel] = Float32(Float16(acc / (1.0 + exp(-acc))))
                for slot in range(WINDOW):
                    expected = Float16(_conv_source(
                        initial_h.unsafe_ptr(), mixed_h.unsafe_ptr(), channel, token + slot + 1
                    ))
                    if conv_h[(token * CONV_DIM + channel) * WINDOW + slot] != expected:
                        raise Error("dynamic prepare zapisuje niepoprawny checkpoint conv dla T=" + String(steps))

        for token in range(steps):
            for head in range(N_K):
                var q_sum: Float32 = 0.0
                var k_sum: Float32 = 0.0
                for lane in range(D_STATE):
                    q_value = conv[token * CONV_DIM + head * D_STATE + lane]
                    k_value = conv[token * CONV_DIM + N_K * D_STATE + head * D_STATE + lane]
                    q_sum += q_value * q_value
                    k_sum += k_value * k_value
                q_inv = 1.0 / sqrt(q_sum + EPS)
                k_inv = 1.0 / sqrt(k_sum + EPS)
                for repeat in range(N_V // N_K):
                    out_head = repeat * N_K + head
                    for lane in range(D_STATE):
                        index = (token * N_V + out_head) * D_STATE + lane
                        q_expected = Float16(conv[token * CONV_DIM + head * D_STATE + lane] * q_inv)
                        k_expected = Float16(conv[token * CONV_DIM + N_K * D_STATE + head * D_STATE + lane] * k_inv)
                        q_error = abs(Float32(q_h[index]) - Float32(q_expected))
                        k_error = abs(Float32(k_h[index]) - Float32(k_expected))
                        max_prepare_error = max(max_prepare_error, max(q_error, k_error))
            for head in range(N_V):
                gate = token * N_V + head
                x = Float32(alpha_h[gate]) + Float32(dt_h[head])
                softplus = x if x > 20.0 else log(1.0 + exp(x))
                g_expected = softplus * Float32(scale_h[head])
                beta_expected = 1.0 / (1.0 + exp(-Float32(beta_raw_h[gate])))
                max_prepare_error = max(max_prepare_error, abs(g_h[gate] - g_expected))
                max_prepare_error = max(max_prepare_error, abs(beta_h[gate] - beta_expected))
                for lane in range(D_STATE):
                    index = (token * N_V + head) * D_STATE + lane
                    channel = 2 * N_K * D_STATE + head * D_STATE + lane
                    expected = Float16(conv[token * CONV_DIM + channel])
                    max_prepare_error = max(max_prepare_error, abs(Float32(v_h[index]) - Float32(expected)))

        var current = List[Float32]()
        for i in range(STATE_ELEMENTS):
            current.append(state_h[i])
        inv_sqrt = 1.0 / sqrt(Float32(D_STATE))
        for token in range(steps):
            for head in range(N_V):
                gate = token * N_V + head
                decay = exp(g_h[gate])
                head_state = head * D_STATE * D_STATE
                vector = (token * N_V + head) * D_STATE
                for i in range(D_STATE):
                    for j in range(D_STATE):
                        index = head_state + i * D_STATE + j
                        current[index] *= decay
                for j in range(D_STATE):
                    var kv: Float32 = 0.0
                    for i in range(D_STATE):
                        kv += Float32(k_h[vector + i]) * current[head_state + i * D_STATE + j]
                    delta = beta_h[gate] * (Float32(v_h[vector + j]) - kv)
                    for i in range(D_STATE):
                        current[head_state + i * D_STATE + j] += Float32(k_h[vector + i]) * delta
                    var expected_output: Float32 = 0.0
                    for i in range(D_STATE):
                        expected_output += Float32(q_h[vector + i]) * current[head_state + i * D_STATE + j]
                    expected_output *= inv_sqrt
                    max_output_error = max(max_output_error, abs(Float32(output_h[vector + j]) - Float32(Float16(expected_output))))
            checkpoint_base = token * STATE_ELEMENTS
            for i in range(STATE_ELEMENTS):
                max_state_error = max(max_state_error, abs(checkpoints_h[checkpoint_base + i] - current[i]))

    if max_prepare_error > 0.00001:
        raise Error("dynamic prepare przekracza tolerancje dla T=" + String(steps))
    if max_state_error > 0.00001:
        raise Error("dynamic scan przekracza tolerancje stanu dla T=" + String(steps))
    if max_output_error > 0.0005:
        raise Error("dynamic scan przekracza tolerancje wyjscia dla T=" + String(steps))
    print("PASS T=", steps, " prepare=", max_prepare_error, " state=", max_state_error, " output=", max_output_error, sep="")


def _chunk_invariance(ctx: DeviceContext) raises:
    comptime STEPS = 32
    comptime HALF = 16
    var initial = ctx.enqueue_create_buffer[DType.float16](CONV_DIM * WINDOW)
    var mixed = ctx.enqueue_create_buffer[DType.float16](STEPS * CONV_DIM)
    var weight = ctx.enqueue_create_buffer[DType.float16](CONV_DIM * D_CONV)
    var alpha = ctx.enqueue_create_buffer[DType.float16](STEPS * N_V)
    var beta_raw = ctx.enqueue_create_buffer[DType.float16](STEPS * N_V)
    var dt_bias = ctx.enqueue_create_buffer[DType.float16](N_V)
    var a_scale = ctx.enqueue_create_buffer[DType.float16](N_V)
    var state = ctx.enqueue_create_buffer[DType.float32](STATE_ELEMENTS)
    var full_q = ctx.enqueue_create_buffer[DType.float16](STEPS * VECTOR_ELEMENTS)
    var full_k = ctx.enqueue_create_buffer[DType.float16](STEPS * VECTOR_ELEMENTS)
    var full_v = ctx.enqueue_create_buffer[DType.float16](STEPS * VECTOR_ELEMENTS)
    var full_g = ctx.enqueue_create_buffer[DType.float32](STEPS * N_V)
    var full_beta = ctx.enqueue_create_buffer[DType.float32](STEPS * N_V)
    var full_conv = ctx.enqueue_create_buffer[DType.float16](STEPS * CONV_DIM * WINDOW)
    var full_output = ctx.enqueue_create_buffer[DType.float16](STEPS * VECTOR_ELEMENTS)
    var full_states = ctx.enqueue_create_buffer[DType.float32](STEPS * STATE_ELEMENTS)
    var chunk_q = ctx.enqueue_create_buffer[DType.float16](STEPS * VECTOR_ELEMENTS)
    var chunk_k = ctx.enqueue_create_buffer[DType.float16](STEPS * VECTOR_ELEMENTS)
    var chunk_v = ctx.enqueue_create_buffer[DType.float16](STEPS * VECTOR_ELEMENTS)
    var chunk_g = ctx.enqueue_create_buffer[DType.float32](STEPS * N_V)
    var chunk_beta = ctx.enqueue_create_buffer[DType.float32](STEPS * N_V)
    var chunk_conv = ctx.enqueue_create_buffer[DType.float16](STEPS * CONV_DIM * WINDOW)
    var chunk_output = ctx.enqueue_create_buffer[DType.float16](STEPS * VECTOR_ELEMENTS)
    var chunk_states = ctx.enqueue_create_buffer[DType.float32](STEPS * STATE_ELEMENTS)
    var conv_carry = ctx.enqueue_create_buffer[DType.float16](CONV_DIM * WINDOW)
    var state_carry = ctx.enqueue_create_buffer[DType.float32](STATE_ELEMENTS)

    with initial.map_to_host() as initial_h, mixed.map_to_host() as mixed_h, weight.map_to_host() as weight_h, alpha.map_to_host() as alpha_h, beta_raw.map_to_host() as beta_raw_h, dt_bias.map_to_host() as dt_h, a_scale.map_to_host() as scale_h, state.map_to_host() as state_h:
        _fill_inputs(initial_h.unsafe_ptr(), mixed_h.unsafe_ptr(), weight_h.unsafe_ptr(), alpha_h.unsafe_ptr(), beta_raw_h.unsafe_ptr(), dt_h.unsafe_ptr(), scale_h.unsafe_ptr(), state_h.unsafe_ptr(), STEPS)

    ctx.enqueue_function[deltanet_prepare_dynamic_f16](
        full_q.unsafe_ptr(), full_k.unsafe_ptr(), full_v.unsafe_ptr(), full_g.unsafe_ptr(), full_beta.unsafe_ptr(),
        full_conv.unsafe_ptr(), initial.unsafe_ptr(), mixed.unsafe_ptr(), weight.unsafe_ptr(),
        alpha.unsafe_ptr(), beta_raw.unsafe_ptr(), dt_bias.unsafe_ptr(), a_scale.unsafe_ptr(),
        STEPS, N_K, N_V, D_STATE, D_CONV, EPS, grid_dim=N_K + N_V, block_dim=32,
    )
    ctx.enqueue_function[deltanet_gated_scan_dynamic_f16](
        full_output.unsafe_ptr(), full_states.unsafe_ptr(), state.unsafe_ptr(),
        full_q.unsafe_ptr(), full_k.unsafe_ptr(), full_v.unsafe_ptr(), full_g.unsafe_ptr(), full_beta.unsafe_ptr(),
        STEPS, N_V, D_STATE, grid_dim=N_V, block_dim=D_STATE,
    )
    ctx.enqueue_function[deltanet_prepare_dynamic_f16](
        chunk_q.unsafe_ptr(), chunk_k.unsafe_ptr(), chunk_v.unsafe_ptr(), chunk_g.unsafe_ptr(), chunk_beta.unsafe_ptr(),
        chunk_conv.unsafe_ptr(), initial.unsafe_ptr(), mixed.unsafe_ptr(), weight.unsafe_ptr(),
        alpha.unsafe_ptr(), beta_raw.unsafe_ptr(), dt_bias.unsafe_ptr(), a_scale.unsafe_ptr(),
        HALF, N_K, N_V, D_STATE, D_CONV, EPS, grid_dim=N_K + N_V, block_dim=32,
    )
    ctx.enqueue_function[deltanet_gated_scan_dynamic_f16](
        chunk_output.unsafe_ptr(), chunk_states.unsafe_ptr(), state.unsafe_ptr(),
        chunk_q.unsafe_ptr(), chunk_k.unsafe_ptr(), chunk_v.unsafe_ptr(), chunk_g.unsafe_ptr(), chunk_beta.unsafe_ptr(),
        HALF, N_V, D_STATE, grid_dim=N_V, block_dim=D_STATE,
    )
    ctx.synchronize()
    with chunk_conv.map_to_host() as conv_h, conv_carry.map_to_host() as carry_h:
        for i in range(CONV_DIM * WINDOW):
            carry_h[i] = conv_h[(HALF - 1) * CONV_DIM * WINDOW + i]
    with chunk_states.map_to_host() as states_h, state_carry.map_to_host() as carry_h:
        for i in range(STATE_ELEMENTS):
            carry_h[i] = states_h[(HALF - 1) * STATE_ELEMENTS + i]
    ctx.enqueue_function[deltanet_prepare_dynamic_f16](
        chunk_q.unsafe_ptr() + HALF * VECTOR_ELEMENTS,
        chunk_k.unsafe_ptr() + HALF * VECTOR_ELEMENTS,
        chunk_v.unsafe_ptr() + HALF * VECTOR_ELEMENTS,
        chunk_g.unsafe_ptr() + HALF * N_V,
        chunk_beta.unsafe_ptr() + HALF * N_V,
        chunk_conv.unsafe_ptr() + HALF * CONV_DIM * WINDOW,
        conv_carry.unsafe_ptr(),
        mixed.unsafe_ptr() + HALF * CONV_DIM, weight.unsafe_ptr(),
        alpha.unsafe_ptr() + HALF * N_V, beta_raw.unsafe_ptr() + HALF * N_V,
        dt_bias.unsafe_ptr(), a_scale.unsafe_ptr(), HALF, N_K, N_V, D_STATE, D_CONV, EPS,
        grid_dim=N_K + N_V, block_dim=32,
    )
    ctx.enqueue_function[deltanet_gated_scan_dynamic_f16](
        chunk_output.unsafe_ptr() + HALF * VECTOR_ELEMENTS,
        chunk_states.unsafe_ptr() + HALF * STATE_ELEMENTS,
        state_carry.unsafe_ptr(),
        chunk_q.unsafe_ptr() + HALF * VECTOR_ELEMENTS,
        chunk_k.unsafe_ptr() + HALF * VECTOR_ELEMENTS,
        chunk_v.unsafe_ptr() + HALF * VECTOR_ELEMENTS,
        chunk_g.unsafe_ptr() + HALF * N_V,
        chunk_beta.unsafe_ptr() + HALF * N_V,
        HALF, N_V, D_STATE, grid_dim=N_V, block_dim=D_STATE,
    )
    ctx.synchronize()

    with full_q.map_to_host() as fq, chunk_q.map_to_host() as cq, full_k.map_to_host() as fk, chunk_k.map_to_host() as ck, full_v.map_to_host() as fv, chunk_v.map_to_host() as cv, full_g.map_to_host() as fg, chunk_g.map_to_host() as cg, full_beta.map_to_host() as fb, chunk_beta.map_to_host() as cb, full_conv.map_to_host() as fc, chunk_conv.map_to_host() as cc, full_output.map_to_host() as fo, chunk_output.map_to_host() as co, full_states.map_to_host() as fs, chunk_states.map_to_host() as cs:
        for i in range(len(fq)):
            if fq[i] != cq[i] or fk[i] != ck[i] or fv[i] != cv[i] or fo[i] != co[i]:
                raise Error("podzial 32 na 2x16 zmienia wektory DeltaNet")
        for i in range(len(fg)):
            if fg[i] != cg[i] or fb[i] != cb[i]:
                raise Error("podzial 32 na 2x16 zmienia bramki DeltaNet")
        for i in range(len(fc)):
            if fc[i] != cc[i]:
                raise Error("podzial 32 na 2x16 zmienia checkpointy conv")
        for i in range(len(fs)):
            if fs[i] != cs[i]:
                raise Error("podzial 32 na 2x16 zmienia checkpointy stanu")
    print("PASS chunk invariance 32=2x16: bit parity")


def main() raises:
    var ctx = DeviceContext()
    for steps in [1, 2, 3, 4, 5, 31, 32, 64, 128]:
        _case(ctx, steps)
    _chunk_invariance(ctx)
