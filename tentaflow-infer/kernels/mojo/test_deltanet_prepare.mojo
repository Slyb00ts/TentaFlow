# =============================================================================
# Plik: test_deltanet_prepare.mojo
# Opis: Sprawdza fused prepare DeltaNet względem CPU oracle i mierzy koszt
#       pojedynczego kernela wobec rozłożonych operacji GPU.
# Przykład: pixi run mojo test_deltanet_prepare.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.math import exp, log, sqrt
from std.time import perf_counter_ns
from src.deltanet import (
    deltanet_conv_silu_f16,
    l2norm_heads_f16,
    deltanet_log_decay_f32,
    deltanet_beta_sigmoid_f32,
)
from src.deltanet_verify import (
    deltanet_prepare_t2_f16,
    deltanet_prepare_t3_f16,
    deltanet_prepare_t4_f16,
)


comptime N_K = 2
comptime N_V = 4
comptime D_STATE = 8
comptime D_CONV = 3
comptime CONV_DIM = (2 * N_K + N_V) * D_STATE
comptime EPS = 0.00001


def _source(
    initial: UnsafePointer[Float16, MutUntrackedOrigin],
    mixed: UnsafePointer[Float16, MutUntrackedOrigin],
    channel: Int,
    position: Int,
) -> Float32:
    comptime WINDOW = D_CONV - 1
    if position < WINDOW:
        return Float32(initial[channel * WINDOW + position])
    return Float32(mixed[(position - WINDOW) * CONV_DIM + channel])


def _golden[steps: Int](ctx: DeviceContext) raises:
    comptime WINDOW = D_CONV - 1
    var q = ctx.enqueue_create_buffer[DType.float16](steps * N_V * D_STATE)
    var k = ctx.enqueue_create_buffer[DType.float16](steps * N_V * D_STATE)
    var v = ctx.enqueue_create_buffer[DType.float16](steps * N_V * D_STATE)
    var g = ctx.enqueue_create_buffer[DType.float32](steps * N_V)
    var beta = ctx.enqueue_create_buffer[DType.float32](steps * N_V)
    var checkpoints = ctx.enqueue_create_buffer[DType.float16](steps * CONV_DIM * WINDOW)
    var initial = ctx.enqueue_create_buffer[DType.float16](CONV_DIM * WINDOW)
    var mixed = ctx.enqueue_create_buffer[DType.float16](steps * CONV_DIM)
    var weight = ctx.enqueue_create_buffer[DType.float16](CONV_DIM * D_CONV)
    var alpha = ctx.enqueue_create_buffer[DType.float16](steps * N_V)
    var beta_raw = ctx.enqueue_create_buffer[DType.float16](steps * N_V)
    var dt_bias = ctx.enqueue_create_buffer[DType.float16](N_V)
    var a_scale = ctx.enqueue_create_buffer[DType.float16](N_V)

    with initial.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32((i * 7) % 19 - 9) * 0.03125)
    with mixed.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32((i * 11) % 23 - 11) * 0.025)
    with weight.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32((i * 5) % 13 - 6) * 0.04)
    with alpha.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32(i - 3) * 0.2)
    with beta_raw.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32(4 - i) * 0.15)
    with dt_bias.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32(i - 2) * 0.1)
    with a_scale.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(-0.25 - Float32(i) * 0.05)

    comptime if steps == 2:
        ctx.enqueue_function[deltanet_prepare_t2_f16](
            q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(),
            checkpoints.unsafe_ptr(), initial.unsafe_ptr(), mixed.unsafe_ptr(), weight.unsafe_ptr(),
            alpha.unsafe_ptr(), beta_raw.unsafe_ptr(), dt_bias.unsafe_ptr(), a_scale.unsafe_ptr(),
            N_K, N_V, D_STATE, D_CONV, Float32(EPS), grid_dim=N_K + N_V, block_dim=32,
        )
    elif steps == 3:
        ctx.enqueue_function[deltanet_prepare_t3_f16](
            q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(),
            checkpoints.unsafe_ptr(), initial.unsafe_ptr(), mixed.unsafe_ptr(), weight.unsafe_ptr(),
            alpha.unsafe_ptr(), beta_raw.unsafe_ptr(), dt_bias.unsafe_ptr(), a_scale.unsafe_ptr(),
            N_K, N_V, D_STATE, D_CONV, Float32(EPS), grid_dim=N_K + N_V, block_dim=32,
        )
    else:
        ctx.enqueue_function[deltanet_prepare_t4_f16](
            q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(),
            checkpoints.unsafe_ptr(), initial.unsafe_ptr(), mixed.unsafe_ptr(), weight.unsafe_ptr(),
            alpha.unsafe_ptr(), beta_raw.unsafe_ptr(), dt_bias.unsafe_ptr(), a_scale.unsafe_ptr(),
            N_K, N_V, D_STATE, D_CONV, Float32(EPS), grid_dim=N_K + N_V, block_dim=32,
        )
    ctx.synchronize()

    with initial.map_to_host() as initial_h, mixed.map_to_host() as mixed_h, weight.map_to_host() as weight_h, alpha.map_to_host() as alpha_h, beta_raw.map_to_host() as beta_raw_h, dt_bias.map_to_host() as dt_h, a_scale.map_to_host() as a_h, q.map_to_host() as q_h, k.map_to_host() as k_h, v.map_to_host() as v_h, g.map_to_host() as g_h, beta.map_to_host() as beta_h, checkpoints.map_to_host() as checkpoint_h:
        var conv = InlineArray[Float32, steps * CONV_DIM](fill=0.0)
        for token in range(steps):
            for channel in range(CONV_DIM):
                var acc: Float32 = 0.0
                for tap in range(D_CONV):
                    acc += Float32(weight_h[channel * D_CONV + tap]) * _source(
                        initial_h.unsafe_ptr(), mixed_h.unsafe_ptr(), channel, token + tap
                    )
                conv[token * CONV_DIM + channel] = Float32(Float16(acc / (1.0 + exp(-acc))))
                for slot in range(WINDOW):
                    expected = _source(
                        initial_h.unsafe_ptr(), mixed_h.unsafe_ptr(), channel, token + slot + 1
                    )
                    actual = Float32(checkpoint_h[(token * CONV_DIM + channel) * WINDOW + slot])
                    if abs(actual - expected) > 0.0005:
                        raise Error("niezgodny checkpoint splotu DeltaNet")

        for token in range(steps):
            for head in range(N_K):
                var q_ss: Float32 = 0.0
                var k_ss: Float32 = 0.0
                for lane in range(D_STATE):
                    q_value = conv[token * CONV_DIM + head * D_STATE + lane]
                    k_value = conv[token * CONV_DIM + N_K * D_STATE + head * D_STATE + lane]
                    q_ss += q_value * q_value
                    k_ss += k_value * k_value
                q_inv = 1.0 / sqrt(q_ss + EPS)
                k_inv = 1.0 / sqrt(k_ss + EPS)
                for repeat in range(N_V // N_K):
                    out_head = repeat * N_K + head
                    for lane in range(D_STATE):
                        out_index = (token * N_V + out_head) * D_STATE + lane
                        q_expected = conv[token * CONV_DIM + head * D_STATE + lane] * q_inv
                        k_expected = conv[token * CONV_DIM + N_K * D_STATE + head * D_STATE + lane] * k_inv
                        if abs(Float32(q_h[out_index]) - q_expected) > 0.0015:
                            raise Error("niezgodny wynik Q fused DeltaNet")
                        if abs(Float32(k_h[out_index]) - k_expected) > 0.0015:
                            raise Error("niezgodny wynik K fused DeltaNet")
            for head in range(N_V):
                gate_index = token * N_V + head
                x = Float32(alpha_h[gate_index]) + Float32(dt_h[head])
                softplus = x if x > 20.0 else log(1.0 + exp(x))
                if abs(g_h[gate_index] - softplus * Float32(a_h[head])) > 0.00001:
                    raise Error("niezgodny log-decay fused DeltaNet")
                expected_beta = 1.0 / (1.0 + exp(-Float32(beta_raw_h[gate_index])))
                if abs(beta_h[gate_index] - expected_beta) > 0.00001:
                    raise Error("niezgodna bramka beta fused DeltaNet")
                for lane in range(D_STATE):
                    out_index = (token * N_V + head) * D_STATE + lane
                    channel = 2 * N_K * D_STATE + head * D_STATE + lane
                    if abs(Float32(v_h[out_index]) - conv[token * CONV_DIM + channel]) > 0.001:
                        raise Error("niezgodny wynik V fused DeltaNet")
    print("golden fused DeltaNet T=", steps, ": PASS", sep="")


def _bench(ctx: DeviceContext) raises:
    comptime STEPS = 4
    comptime BENCH_N_K = 16
    comptime BENCH_N_V = 32
    comptime BENCH_D_STATE = 128
    comptime BENCH_D_CONV = 4
    comptime BENCH_CONV_DIM = (2 * BENCH_N_K + BENCH_N_V) * BENCH_D_STATE
    comptime ITERS = 200
    comptime WARMUP = 40
    var q = ctx.enqueue_create_buffer[DType.float16](STEPS * BENCH_N_V * BENCH_D_STATE)
    var k = ctx.enqueue_create_buffer[DType.float16](STEPS * BENCH_N_V * BENCH_D_STATE)
    var v = ctx.enqueue_create_buffer[DType.float16](STEPS * BENCH_N_V * BENCH_D_STATE)
    var g = ctx.enqueue_create_buffer[DType.float32](STEPS * BENCH_N_V)
    var beta = ctx.enqueue_create_buffer[DType.float32](STEPS * BENCH_N_V)
    var checkpoints = ctx.enqueue_create_buffer[DType.float16](STEPS * BENCH_CONV_DIM * (BENCH_D_CONV - 1))
    var initial = ctx.enqueue_create_buffer[DType.float16](BENCH_CONV_DIM * (BENCH_D_CONV - 1))
    var mixed = ctx.enqueue_create_buffer[DType.float16](STEPS * BENCH_CONV_DIM)
    var conv_out = ctx.enqueue_create_buffer[DType.float16](STEPS * BENCH_CONV_DIM)
    var norm = ctx.enqueue_create_buffer[DType.float16](STEPS * BENCH_CONV_DIM)
    var weight = ctx.enqueue_create_buffer[DType.float16](BENCH_CONV_DIM * BENCH_D_CONV)
    var alpha = ctx.enqueue_create_buffer[DType.float16](STEPS * BENCH_N_V)
    var beta_raw = ctx.enqueue_create_buffer[DType.float16](STEPS * BENCH_N_V)
    var dt_bias = ctx.enqueue_create_buffer[DType.float16](BENCH_N_V)
    var a_scale = ctx.enqueue_create_buffer[DType.float16](BENCH_N_V)

    for _ in range(WARMUP):
        ctx.enqueue_function[deltanet_prepare_t4_f16](
            q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(),
            checkpoints.unsafe_ptr(), initial.unsafe_ptr(), mixed.unsafe_ptr(), weight.unsafe_ptr(),
            alpha.unsafe_ptr(), beta_raw.unsafe_ptr(), dt_bias.unsafe_ptr(), a_scale.unsafe_ptr(),
            BENCH_N_K, BENCH_N_V, BENCH_D_STATE, BENCH_D_CONV, Float32(EPS),
            grid_dim=BENCH_N_K + BENCH_N_V, block_dim=BENCH_D_STATE,
        )
    ctx.synchronize()
    start = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[deltanet_prepare_t4_f16](
            q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(),
            checkpoints.unsafe_ptr(), initial.unsafe_ptr(), mixed.unsafe_ptr(), weight.unsafe_ptr(),
            alpha.unsafe_ptr(), beta_raw.unsafe_ptr(), dt_bias.unsafe_ptr(), a_scale.unsafe_ptr(),
            BENCH_N_K, BENCH_N_V, BENCH_D_STATE, BENCH_D_CONV, Float32(EPS),
            grid_dim=BENCH_N_K + BENCH_N_V, block_dim=BENCH_D_STATE,
        )
    ctx.synchronize()
    fused_ms = Float64(perf_counter_ns() - start) / 1e6 / ITERS

    for _ in range(WARMUP):
        for token in range(STEPS):
            ctx.enqueue_function[deltanet_conv_silu_f16](
                conv_out.unsafe_ptr() + token * BENCH_CONV_DIM, initial.unsafe_ptr(),
                mixed.unsafe_ptr() + token * BENCH_CONV_DIM, weight.unsafe_ptr(),
                BENCH_CONV_DIM, BENCH_D_CONV, grid_dim=(BENCH_CONV_DIM + 255) // 256,
                block_dim=256,
            )
            ctx.enqueue_function[l2norm_heads_f16](
                norm.unsafe_ptr() + token * BENCH_CONV_DIM,
                conv_out.unsafe_ptr() + token * BENCH_CONV_DIM,
                BENCH_D_STATE, Float32(EPS), grid_dim=BENCH_N_K, block_dim=BENCH_D_STATE,
            )
            ctx.enqueue_function[l2norm_heads_f16](
                norm.unsafe_ptr() + token * BENCH_CONV_DIM + BENCH_N_K * BENCH_D_STATE,
                conv_out.unsafe_ptr() + token * BENCH_CONV_DIM + BENCH_N_K * BENCH_D_STATE,
                BENCH_D_STATE, Float32(EPS), grid_dim=BENCH_N_K, block_dim=BENCH_D_STATE,
            )
            ctx.enqueue_function[deltanet_log_decay_f32](
                g.unsafe_ptr() + token * BENCH_N_V, alpha.unsafe_ptr() + token * BENCH_N_V,
                dt_bias.unsafe_ptr(), a_scale.unsafe_ptr(), BENCH_N_V,
                grid_dim=1, block_dim=256,
            )
        ctx.enqueue_function[deltanet_beta_sigmoid_f32](
            beta.unsafe_ptr(), beta_raw.unsafe_ptr(), STEPS * BENCH_N_V,
            grid_dim=1, block_dim=256,
        )
    ctx.synchronize()
    start = perf_counter_ns()
    for _ in range(ITERS):
        for token in range(STEPS):
            ctx.enqueue_function[deltanet_conv_silu_f16](
                conv_out.unsafe_ptr() + token * BENCH_CONV_DIM, initial.unsafe_ptr(),
                mixed.unsafe_ptr() + token * BENCH_CONV_DIM, weight.unsafe_ptr(),
                BENCH_CONV_DIM, BENCH_D_CONV, grid_dim=(BENCH_CONV_DIM + 255) // 256,
                block_dim=256,
            )
            ctx.enqueue_function[l2norm_heads_f16](
                norm.unsafe_ptr() + token * BENCH_CONV_DIM,
                conv_out.unsafe_ptr() + token * BENCH_CONV_DIM,
                BENCH_D_STATE, Float32(EPS), grid_dim=BENCH_N_K, block_dim=BENCH_D_STATE,
            )
            ctx.enqueue_function[l2norm_heads_f16](
                norm.unsafe_ptr() + token * BENCH_CONV_DIM + BENCH_N_K * BENCH_D_STATE,
                conv_out.unsafe_ptr() + token * BENCH_CONV_DIM + BENCH_N_K * BENCH_D_STATE,
                BENCH_D_STATE, Float32(EPS), grid_dim=BENCH_N_K, block_dim=BENCH_D_STATE,
            )
            ctx.enqueue_function[deltanet_log_decay_f32](
                g.unsafe_ptr() + token * BENCH_N_V, alpha.unsafe_ptr() + token * BENCH_N_V,
                dt_bias.unsafe_ptr(), a_scale.unsafe_ptr(), BENCH_N_V,
                grid_dim=1, block_dim=256,
            )
        ctx.enqueue_function[deltanet_beta_sigmoid_f32](
            beta.unsafe_ptr(), beta_raw.unsafe_ptr(), STEPS * BENCH_N_V,
            grid_dim=1, block_dim=256,
        )
    ctx.synchronize()
    decomposed_ms = Float64(perf_counter_ns() - start) / 1e6 / ITERS
    print("fused Delta prepare T4:", fused_ms, "ms")
    print("rozłożone kernele bez kosztu kopii/repeat:", decomposed_ms, "ms")
    print("przyspieszenie konserwatywne:", decomposed_ms / fused_ms, "x")


def main() raises:
    var ctx = DeviceContext()
    _golden[2](ctx)
    _golden[3](ctx)
    _golden[4](ctx)
    _bench(ctx)
