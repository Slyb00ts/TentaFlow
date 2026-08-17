# =============================================================================
# Plik: test_deltanet_scan_persistent.mojo
# Opis: Porównuje rejestrowy pełny skan DeltaNet z istniejącym skanem shared
#       wykonywanym w segmentach 128 tokenów i kontroluje granice buforów.
# Przykład: pixi run mojo test_deltanet_scan_persistent.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.math import abs
from std.time import perf_counter_ns
from src.deltanet_scan_persistent import deltanet_gated_scan_persistent_d128_f16
from src.deltanet_verify import deltanet_gated_scan_inplace_shared_d128_f16

comptime N_V = 2
comptime REAL_N_V = 48
comptime D_STATE = 128
comptime VECTOR_ELEMENTS = N_V * D_STATE
comptime STATE_ELEMENTS = N_V * D_STATE * D_STATE
comptime GUARD = 37
comptime GUARD_F16 = Float16(19.25)
comptime GUARD_F32 = Float32(71.5)
comptime OUTPUT_LIMIT = Float32(0.02)
comptime STATE_LIMIT = Float32(0.002)


def _fill_inputs[
    steps: Int
](
    q: DeviceBuffer[DType.float16],
    k: DeviceBuffer[DType.float16],
    v: DeviceBuffer[DType.float16],
    g: DeviceBuffer[DType.float32],
    beta: DeviceBuffer[DType.float32],
) raises:
    with q.map_to_host() as qh, k.map_to_host() as kh, v.map_to_host() as vh:
        for i in range(len(qh)):
            qh[i] = Float16(Float32((i * 7 + 3) % 19 - 9) * 0.003)
            kh[i] = Float16(Float32((i * 11 + 5) % 23 - 11) * 0.003)
            vh[i] = Float16(Float32((i * 17 + 7) % 29 - 14) * 0.002)
    with g.map_to_host() as gh, beta.map_to_host() as bh:
        for i in range(len(gh)):
            gh[i] = -0.01 - Float32((i * 5 + 1) % 7) * 0.002
            bh[i] = 0.1 + Float32((i * 3 + 2) % 5) * 0.03


def _case[steps: Int](ctx: DeviceContext) raises:
    var reference_state = ctx.enqueue_create_buffer[DType.float32](STATE_ELEMENTS + 2 * GUARD)
    var persistent_state = ctx.enqueue_create_buffer[DType.float32](STATE_ELEMENTS + 2 * GUARD)
    var q = ctx.enqueue_create_buffer[DType.float16](steps * VECTOR_ELEMENTS)
    var k = ctx.enqueue_create_buffer[DType.float16](steps * VECTOR_ELEMENTS)
    var v = ctx.enqueue_create_buffer[DType.float16](steps * VECTOR_ELEMENTS)
    var g = ctx.enqueue_create_buffer[DType.float32](steps * N_V)
    var beta = ctx.enqueue_create_buffer[DType.float32](steps * N_V)
    var reference_out = ctx.enqueue_create_buffer[DType.float16](steps * VECTOR_ELEMENTS + 2 * GUARD)
    var persistent_out = ctx.enqueue_create_buffer[DType.float16](steps * VECTOR_ELEMENTS + 2 * GUARD)

    with reference_state.map_to_host() as reference, persistent_state.map_to_host() as persistent:
        for i in range(len(reference)):
            value = GUARD_F32
            if i >= GUARD and i < GUARD + STATE_ELEMENTS:
                value = Float32(((i - GUARD) * 13 + 17) % 31 - 15) * 0.0001
            reference[i] = value
            persistent[i] = value
    with reference_out.map_to_host() as reference, persistent_out.map_to_host() as persistent:
        for i in range(len(reference)):
            reference[i] = GUARD_F16
            persistent[i] = GUARD_F16
    _fill_inputs[steps](q, k, v, g, beta)

    var offset = 0
    while offset < steps:
        count = min(128, steps - offset)
        ctx.enqueue_function[deltanet_gated_scan_inplace_shared_d128_f16](
            reference_out.unsafe_ptr() + GUARD + offset * VECTOR_ELEMENTS,
            reference_state.unsafe_ptr() + GUARD,
            q.unsafe_ptr() + offset * VECTOR_ELEMENTS,
            k.unsafe_ptr() + offset * VECTOR_ELEMENTS,
            v.unsafe_ptr() + offset * VECTOR_ELEMENTS,
            g.unsafe_ptr() + offset * N_V,
            beta.unsafe_ptr() + offset * N_V,
            count, N_V, D_STATE,
            grid_dim=N_V * 2, block_dim=64,
        )
        offset += count
    ctx.enqueue_function[deltanet_gated_scan_persistent_d128_f16](
        persistent_out.unsafe_ptr() + GUARD,
        persistent_state.unsafe_ptr() + GUARD,
        q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(),
        steps, N_V, D_STATE,
        grid_dim=N_V * 32, block_dim=64,
    )
    ctx.synchronize()

    var output_max: Float32 = 0.0
    var output_bitwise = 0
    with reference_out.map_to_host() as reference, persistent_out.map_to_host() as persistent:
        for i in range(GUARD):
            if reference[i].to_bits() != GUARD_F16.to_bits() or persistent[i].to_bits() != GUARD_F16.to_bits():
                raise Error("naruszony początkowy guard wyjścia T=" + String(steps))
        for i in range(steps * VECTOR_ELEMENTS):
            a = reference[GUARD + i]
            b = persistent[GUARD + i]
            if a.to_bits() == b.to_bits():
                output_bitwise += 1
            output_max = max(output_max, abs(Float32(a) - Float32(b)))
        for i in range(GUARD + steps * VECTOR_ELEMENTS, len(reference)):
            if reference[i].to_bits() != GUARD_F16.to_bits() or persistent[i].to_bits() != GUARD_F16.to_bits():
                raise Error("naruszony końcowy guard wyjścia T=" + String(steps))

    var state_max: Float32 = 0.0
    var state_bitwise = 0
    with reference_state.map_to_host() as reference, persistent_state.map_to_host() as persistent:
        for i in range(GUARD):
            if reference[i] != GUARD_F32 or persistent[i] != GUARD_F32:
                raise Error("naruszony początkowy guard stanu T=" + String(steps))
        for i in range(STATE_ELEMENTS):
            state_a = reference[GUARD + i]
            state_b = persistent[GUARD + i]
            if state_a.to_bits() == state_b.to_bits():
                state_bitwise += 1
            state_max = max(state_max, abs(state_a - state_b))
        for i in range(GUARD + STATE_ELEMENTS, len(reference)):
            if reference[i] != GUARD_F32 or persistent[i] != GUARD_F32:
                raise Error("naruszony końcowy guard stanu T=" + String(steps))
    if output_max > OUTPUT_LIMIT or state_max > STATE_LIMIT:
        raise Error("przekroczona tolerancja persistent scan T=" + String(steps))
    print(
        "PASS T=", steps,
        " output_max=", output_max,
        " output_bitwise=", output_bitwise, "/", steps * VECTOR_ELEMENTS,
        " state_max=", state_max,
        " state_bitwise=", state_bitwise, "/", STATE_ELEMENTS,
    )


def _benchmark(ctx: DeviceContext) raises:
    comptime steps = 2048
    comptime vectors = REAL_N_V * D_STATE
    comptime states = REAL_N_V * D_STATE * D_STATE
    var state = ctx.enqueue_create_buffer[DType.float32](states)
    var reference_state = ctx.enqueue_create_buffer[DType.float32](states)
    var q = ctx.enqueue_create_buffer[DType.float16](steps * vectors)
    var k = ctx.enqueue_create_buffer[DType.float16](steps * vectors)
    var v = ctx.enqueue_create_buffer[DType.float16](steps * vectors)
    var g = ctx.enqueue_create_buffer[DType.float32](steps * REAL_N_V)
    var beta = ctx.enqueue_create_buffer[DType.float32](steps * REAL_N_V)
    var out = ctx.enqueue_create_buffer[DType.float16](steps * vectors)
    var reference_out = ctx.enqueue_create_buffer[DType.float16](steps * vectors)
    with state.map_to_host() as host, reference_state.map_to_host() as reference:
        for i in range(len(host)):
            value = Float32((i * 13 + 17) % 31 - 15) * 0.0001
            host[i] = value
            reference[i] = value
    with q.map_to_host() as qh, k.map_to_host() as kh, v.map_to_host() as vh:
        for i in range(len(qh)):
            qh[i] = Float16(Float32((i * 7 + 3) % 19 - 9) * 0.003)
            kh[i] = Float16(Float32((i * 11 + 5) % 23 - 11) * 0.003)
            vh[i] = Float16(Float32((i * 17 + 7) % 29 - 14) * 0.002)
    with g.map_to_host() as gh, beta.map_to_host() as bh:
        for i in range(len(gh)):
            gh[i] = -0.01 - Float32((i * 5 + 1) % 7) * 0.002
            bh[i] = 0.1 + Float32((i * 3 + 2) % 5) * 0.03
    ctx.enqueue_function[deltanet_gated_scan_persistent_d128_f16](
        out.unsafe_ptr(), state.unsafe_ptr(), q.unsafe_ptr(), k.unsafe_ptr(),
        v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(), steps, REAL_N_V, D_STATE,
        grid_dim=REAL_N_V * 32, block_dim=64,
    )
    ctx.synchronize()
    reference_started = perf_counter_ns()
    for offset in range(0, steps, 128):
        ctx.enqueue_function[deltanet_gated_scan_inplace_shared_d128_f16](
            reference_out.unsafe_ptr() + offset * vectors,
            reference_state.unsafe_ptr(),
            q.unsafe_ptr() + offset * vectors,
            k.unsafe_ptr() + offset * vectors,
            v.unsafe_ptr() + offset * vectors,
            g.unsafe_ptr() + offset * REAL_N_V,
            beta.unsafe_ptr() + offset * REAL_N_V,
            128, REAL_N_V, D_STATE,
            grid_dim=REAL_N_V * 2, block_dim=64,
        )
    ctx.synchronize()
    reference_elapsed = Float64(perf_counter_ns() - reference_started) / 1e6
    started = perf_counter_ns()
    ctx.enqueue_function[deltanet_gated_scan_persistent_d128_f16](
        out.unsafe_ptr(), state.unsafe_ptr(), q.unsafe_ptr(), k.unsafe_ptr(),
        v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(), steps, REAL_N_V, D_STATE,
        grid_dim=REAL_N_V * 32, block_dim=64,
    )
    ctx.synchronize()
    persistent_elapsed = Float64(perf_counter_ns() - started) / 1e6
    print(
        "BENCH T=2048 H=48 shared16=", reference_elapsed,
        " ms persistent=", persistent_elapsed,
        " ms speedup=", reference_elapsed / persistent_elapsed,
    )


def main() raises:
    var ctx = DeviceContext()
    _case[1](ctx)
    _case[17](ctx)
    _case[128](ctx)
    _case[129](ctx)
    _case[1024](ctx)
    _case[2048](ctx)
    _benchmark(ctx)
