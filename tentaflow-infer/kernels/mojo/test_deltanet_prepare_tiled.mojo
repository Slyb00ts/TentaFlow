# =============================================================================
# Plik: test_deltanet_prepare_tiled.mojo
# Opis: Porównuje kafelkowane przygotowanie DeltaNet z istniejącym kernelem
#       segmentowanym, łącznie ze stanem conv i granicami buforów.
# Przykład: pixi run mojo test_deltanet_prepare_tiled.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.time import perf_counter_ns
from src.deltanet_prepare_tiled import deltanet_prepare_tiled_d128_c4_f16
from src.deltanet_verify import deltanet_prepare_segmented_final_f16

comptime N_K = 2
comptime N_V = 6
comptime D_STATE = 128
comptime D_CONV = 4
comptime CONV_DIM = (2 * N_K + N_V) * D_STATE
comptime VALUE_DIM = N_V * D_STATE
comptime CONV_ELEMENTS = CONV_DIM * (D_CONV - 1)
comptime GUARD = 19
comptime GUARD_F16 = Float16(23.5)
comptime GUARD_F32 = Float32(91.25)
comptime REAL_N_K = 16
comptime REAL_N_V = 48
comptime REAL_CONV_DIM = (2 * REAL_N_K + REAL_N_V) * D_STATE
comptime REAL_VALUE_DIM = REAL_N_V * D_STATE
comptime REAL_CONV_ELEMENTS = REAL_CONV_DIM * (D_CONV - 1)
comptime BENCH_ITERS = 5


def _fill_inputs[
    steps: Int
](
    initial_a: DeviceBuffer[DType.float16],
    initial_b: DeviceBuffer[DType.float16],
    mixed: DeviceBuffer[DType.float16],
    weight: DeviceBuffer[DType.float16],
    alpha: DeviceBuffer[DType.float16],
    beta: DeviceBuffer[DType.float16],
    dt: DeviceBuffer[DType.float16],
    scale: DeviceBuffer[DType.float16],
) raises:
    with initial_a.map_to_host() as ah, initial_b.map_to_host() as bh:
        for i in range(len(ah)):
            value = GUARD_F16
            if i >= GUARD and i < GUARD + CONV_ELEMENTS:
                value = Float16(Float32(((i - GUARD) * 13 + 5) % 29 - 14) * 0.002)
            ah[i] = value
            bh[i] = value
    with mixed.map_to_host() as host:
        for i in range(len(host)):
            host[i] = Float16(Float32((i * 7 + 3) % 31 - 15) * 0.006)
    with weight.map_to_host() as host:
        for i in range(len(host)):
            host[i] = Float16(Float32((i * 11 + 1) % 17 - 8) * 0.025)
    with alpha.map_to_host() as ah, beta.map_to_host() as bh:
        for i in range(len(ah)):
            ah[i] = Float16(Float32((i * 5 + 2) % 13 - 6) * 0.04)
            bh[i] = Float16(Float32((i * 3 + 4) % 11 - 5) * 0.07)
    with dt.map_to_host() as dh, scale.map_to_host() as sh:
        for i in range(len(dh)):
            dh[i] = Float16(Float32(i - N_V // 2) * 0.03)
            sh[i] = Float16(-0.08 - Float32(i % 3) * 0.01)


def _guard_f16(buffer: DeviceBuffer[DType.float16], elements: Int, name: String) raises:
    with buffer.map_to_host() as host:
        for i in range(GUARD):
            if host[i].to_bits() != GUARD_F16.to_bits():
                raise Error("naruszony początkowy guard " + name)
        for i in range(GUARD + elements, len(host)):
            if host[i].to_bits() != GUARD_F16.to_bits():
                raise Error("naruszony końcowy guard " + name)


def _guard_f32(buffer: DeviceBuffer[DType.float32], elements: Int, name: String) raises:
    with buffer.map_to_host() as host:
        for i in range(GUARD):
            if host[i] != GUARD_F32:
                raise Error("naruszony początkowy guard " + name)
        for i in range(GUARD + elements, len(host)):
            if host[i] != GUARD_F32:
                raise Error("naruszony końcowy guard " + name)


def _compare_f16(
    reference: DeviceBuffer[DType.float16],
    tiled: DeviceBuffer[DType.float16],
    elements: Int,
    name: String,
) raises:
    with reference.map_to_host() as a, tiled.map_to_host() as b:
        for i in range(elements):
            if a[GUARD + i].to_bits() != b[GUARD + i].to_bits():
                raise Error("różnica bitowa " + name + " przy elemencie " + String(i))
    _guard_f16(reference, elements, name)
    _guard_f16(tiled, elements, name)


def _compare_f32(
    reference: DeviceBuffer[DType.float32],
    tiled: DeviceBuffer[DType.float32],
    elements: Int,
    name: String,
) raises:
    with reference.map_to_host() as a, tiled.map_to_host() as b:
        for i in range(elements):
            if a[GUARD + i].to_bits() != b[GUARD + i].to_bits():
                raise Error("różnica bitowa " + name + " przy elemencie " + String(i))
    _guard_f32(reference, elements, name)
    _guard_f32(tiled, elements, name)


def _case[steps: Int](ctx: DeviceContext) raises:
    comptime vectors = steps * VALUE_DIM
    comptime gates = steps * N_V
    var initial_reference = ctx.enqueue_create_buffer[DType.float16](CONV_ELEMENTS + 2 * GUARD)
    var initial_tiled = ctx.enqueue_create_buffer[DType.float16](CONV_ELEMENTS + 2 * GUARD)
    var mixed = ctx.enqueue_create_buffer[DType.float16](steps * CONV_DIM)
    var weight = ctx.enqueue_create_buffer[DType.float16](CONV_DIM * D_CONV)
    var alpha = ctx.enqueue_create_buffer[DType.float16](gates)
    var beta_raw = ctx.enqueue_create_buffer[DType.float16](gates)
    var dt = ctx.enqueue_create_buffer[DType.float16](N_V)
    var scale = ctx.enqueue_create_buffer[DType.float16](N_V)
    var q_reference = ctx.enqueue_create_buffer[DType.float16](vectors + 2 * GUARD)
    var q_tiled = ctx.enqueue_create_buffer[DType.float16](vectors + 2 * GUARD)
    var k_reference = ctx.enqueue_create_buffer[DType.float16](vectors + 2 * GUARD)
    var k_tiled = ctx.enqueue_create_buffer[DType.float16](vectors + 2 * GUARD)
    var v_reference = ctx.enqueue_create_buffer[DType.float16](vectors + 2 * GUARD)
    var v_tiled = ctx.enqueue_create_buffer[DType.float16](vectors + 2 * GUARD)
    var g_reference = ctx.enqueue_create_buffer[DType.float32](gates + 2 * GUARD)
    var g_tiled = ctx.enqueue_create_buffer[DType.float32](gates + 2 * GUARD)
    var beta_reference = ctx.enqueue_create_buffer[DType.float32](gates + 2 * GUARD)
    var beta_tiled = ctx.enqueue_create_buffer[DType.float32](gates + 2 * GUARD)
    var final_reference = ctx.enqueue_create_buffer[DType.float16](CONV_ELEMENTS + 2 * GUARD)
    var final_tiled = ctx.enqueue_create_buffer[DType.float16](CONV_ELEMENTS + 2 * GUARD)

    _fill_inputs[steps](initial_reference, initial_tiled, mixed, weight, alpha, beta_raw, dt, scale)
    with q_reference.map_to_host() as qr, q_tiled.map_to_host() as qt, k_reference.map_to_host() as kr, k_tiled.map_to_host() as kt, v_reference.map_to_host() as vr, v_tiled.map_to_host() as vt, final_reference.map_to_host() as fr, final_tiled.map_to_host() as ft:
        for i in range(len(qr)):
            qr[i] = GUARD_F16
            qt[i] = GUARD_F16
            kr[i] = GUARD_F16
            kt[i] = GUARD_F16
            vr[i] = GUARD_F16
            vt[i] = GUARD_F16
        for i in range(len(fr)):
            fr[i] = GUARD_F16
            ft[i] = GUARD_F16
    with g_reference.map_to_host() as gr, g_tiled.map_to_host() as gt, beta_reference.map_to_host() as br, beta_tiled.map_to_host() as bt:
        for i in range(len(gr)):
            gr[i] = GUARD_F32
            gt[i] = GUARD_F32
            br[i] = GUARD_F32
            bt[i] = GUARD_F32

    ctx.enqueue_function[deltanet_prepare_segmented_final_f16](
        q_reference.unsafe_ptr() + GUARD, k_reference.unsafe_ptr() + GUARD,
        v_reference.unsafe_ptr() + GUARD, g_reference.unsafe_ptr() + GUARD,
        beta_reference.unsafe_ptr() + GUARD, final_reference.unsafe_ptr() + GUARD,
        initial_reference.unsafe_ptr() + GUARD, mixed.unsafe_ptr(), weight.unsafe_ptr(),
        alpha.unsafe_ptr(), beta_raw.unsafe_ptr(), dt.unsafe_ptr(), scale.unsafe_ptr(),
        steps, N_K, N_V, D_STATE, D_CONV, Float32(1e-6),
        grid_dim=N_K + N_V, block_dim=D_STATE,
    )
    ctx.enqueue_function[deltanet_prepare_tiled_d128_c4_f16](
        q_tiled.unsafe_ptr() + GUARD, k_tiled.unsafe_ptr() + GUARD,
        v_tiled.unsafe_ptr() + GUARD, g_tiled.unsafe_ptr() + GUARD,
        beta_tiled.unsafe_ptr() + GUARD, final_tiled.unsafe_ptr() + GUARD,
        initial_tiled.unsafe_ptr() + GUARD, mixed.unsafe_ptr(), weight.unsafe_ptr(),
        alpha.unsafe_ptr(), beta_raw.unsafe_ptr(), dt.unsafe_ptr(), scale.unsafe_ptr(),
        steps, N_K, N_V, D_STATE, D_CONV, Float32(1e-6),
        grid_dim=(N_K + N_V, (steps + 31) // 32), block_dim=D_STATE,
    )
    ctx.synchronize()

    _compare_f16(q_reference, q_tiled, vectors, "Q T=" + String(steps))
    _compare_f16(k_reference, k_tiled, vectors, "K T=" + String(steps))
    _compare_f16(v_reference, v_tiled, vectors, "V T=" + String(steps))
    _compare_f32(g_reference, g_tiled, gates, "G T=" + String(steps))
    _compare_f32(beta_reference, beta_tiled, gates, "beta T=" + String(steps))
    _compare_f16(final_reference, final_tiled, CONV_ELEMENTS, "conv final T=" + String(steps))
    _compare_f16(initial_reference, initial_tiled, CONV_ELEMENTS, "conv initial T=" + String(steps))
    print("PASS tiled Delta prepare T=", steps)


def _benchmark(ctx: DeviceContext) raises:
    comptime steps = 2048
    var initial = ctx.enqueue_create_buffer[DType.float16](REAL_CONV_ELEMENTS)
    var mixed = ctx.enqueue_create_buffer[DType.float16](steps * REAL_CONV_DIM)
    var weight = ctx.enqueue_create_buffer[DType.float16](REAL_CONV_DIM * D_CONV)
    var alpha = ctx.enqueue_create_buffer[DType.float16](steps * REAL_N_V)
    var beta_raw = ctx.enqueue_create_buffer[DType.float16](steps * REAL_N_V)
    var dt = ctx.enqueue_create_buffer[DType.float16](REAL_N_V)
    var scale = ctx.enqueue_create_buffer[DType.float16](REAL_N_V)
    var q = ctx.enqueue_create_buffer[DType.float16](steps * REAL_VALUE_DIM)
    var k = ctx.enqueue_create_buffer[DType.float16](steps * REAL_VALUE_DIM)
    var v = ctx.enqueue_create_buffer[DType.float16](steps * REAL_VALUE_DIM)
    var g = ctx.enqueue_create_buffer[DType.float32](steps * REAL_N_V)
    var beta = ctx.enqueue_create_buffer[DType.float32](steps * REAL_N_V)
    var final = ctx.enqueue_create_buffer[DType.float16](REAL_CONV_ELEMENTS)
    with initial.map_to_host() as ih, mixed.map_to_host() as mh, weight.map_to_host() as wh:
        for i in range(len(ih)):
            ih[i] = Float16(Float32((i * 13 + 5) % 29 - 14) * 0.002)
        for i in range(len(mh)):
            mh[i] = Float16(Float32((i * 7 + 3) % 31 - 15) * 0.006)
        for i in range(len(wh)):
            wh[i] = Float16(Float32((i * 11 + 1) % 17 - 8) * 0.025)
    with alpha.map_to_host() as ah, beta_raw.map_to_host() as bh:
        for i in range(len(ah)):
            ah[i] = Float16(Float32((i * 5 + 2) % 13 - 6) * 0.04)
            bh[i] = Float16(Float32((i * 3 + 4) % 11 - 5) * 0.07)
    with dt.map_to_host() as dh, scale.map_to_host() as sh:
        for i in range(len(dh)):
            dh[i] = Float16(Float32(i - REAL_N_V // 2) * 0.003)
            sh[i] = Float16(-0.08 - Float32(i % 3) * 0.01)

    ctx.enqueue_function[deltanet_prepare_tiled_d128_c4_f16](
        q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(),
        final.unsafe_ptr(), initial.unsafe_ptr(), mixed.unsafe_ptr(), weight.unsafe_ptr(),
        alpha.unsafe_ptr(), beta_raw.unsafe_ptr(), dt.unsafe_ptr(), scale.unsafe_ptr(),
        steps, REAL_N_K, REAL_N_V, D_STATE, D_CONV, Float32(1e-6),
        grid_dim=(REAL_N_K + REAL_N_V, (steps + 31) // 32), block_dim=D_STATE,
    )
    ctx.synchronize()
    reference_start = perf_counter_ns()
    for _ in range(BENCH_ITERS):
        ctx.enqueue_function[deltanet_prepare_segmented_final_f16](
            q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(),
            final.unsafe_ptr(), initial.unsafe_ptr(), mixed.unsafe_ptr(), weight.unsafe_ptr(),
            alpha.unsafe_ptr(), beta_raw.unsafe_ptr(), dt.unsafe_ptr(), scale.unsafe_ptr(),
            steps, REAL_N_K, REAL_N_V, D_STATE, D_CONV, Float32(1e-6),
            grid_dim=REAL_N_K + REAL_N_V, block_dim=D_STATE,
        )
    ctx.synchronize()
    reference_ms = Float64(perf_counter_ns() - reference_start) / 1e6 / BENCH_ITERS
    tiled_start = perf_counter_ns()
    for _ in range(BENCH_ITERS):
        ctx.enqueue_function[deltanet_prepare_tiled_d128_c4_f16](
            q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(),
            final.unsafe_ptr(), initial.unsafe_ptr(), mixed.unsafe_ptr(), weight.unsafe_ptr(),
            alpha.unsafe_ptr(), beta_raw.unsafe_ptr(), dt.unsafe_ptr(), scale.unsafe_ptr(),
            steps, REAL_N_K, REAL_N_V, D_STATE, D_CONV, Float32(1e-6),
            grid_dim=(REAL_N_K + REAL_N_V, (steps + 31) // 32), block_dim=D_STATE,
        )
    ctx.synchronize()
    tiled_ms = Float64(perf_counter_ns() - tiled_start) / 1e6 / BENCH_ITERS
    print("BENCH prepare T=2048 reference=", reference_ms, " ms tiled=", tiled_ms, " ms speedup=", reference_ms / tiled_ms)


def main() raises:
    var ctx = DeviceContext()
    _case[1](ctx)
    _case[17](ctx)
    _case[32](ctx)
    _case[33](ctx)
    _case[128](ctx)
    _case[129](ctx)
    _case[2048](ctx)
    _benchmark(ctx)
