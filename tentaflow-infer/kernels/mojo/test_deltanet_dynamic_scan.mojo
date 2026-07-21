# =============================================================================
# Plik: test_deltanet_dynamic_scan.mojo
# Opis: Porównuje referencyjny i kafelkowany dynamiczny skan DeltaNet dla
#       długości granicznych używanych przez batched prefill.
# Przykład: pixi run mojo test_deltanet_dynamic_scan.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.deltanet_verify import (
    deltanet_gated_scan_dynamic_f16,
    deltanet_gated_scan_dynamic_d128_f16,
    deltanet_gated_scan_inplace_dynamic_d128_f16,
)

comptime N_V = 32
comptime D_STATE = 128
comptime VECTOR_ELEMENTS = N_V * D_STATE
comptime STATE_ELEMENTS = N_V * D_STATE * D_STATE
comptime REAL_N_V = 48
comptime REAL_VECTOR_ELEMENTS = REAL_N_V * D_STATE
comptime REAL_STATE_ELEMENTS = REAL_N_V * D_STATE * D_STATE
comptime BENCH_ITERS = 20


def _case[steps: Int](ctx: DeviceContext) raises:
    var state = ctx.enqueue_create_buffer[DType.float32](STATE_ELEMENTS)
    var inplace_state = ctx.enqueue_create_buffer[DType.float32](STATE_ELEMENTS)
    var q = ctx.enqueue_create_buffer[DType.float16](steps * VECTOR_ELEMENTS)
    var k = ctx.enqueue_create_buffer[DType.float16](steps * VECTOR_ELEMENTS)
    var v = ctx.enqueue_create_buffer[DType.float16](steps * VECTOR_ELEMENTS)
    var g = ctx.enqueue_create_buffer[DType.float32](steps * N_V)
    var beta = ctx.enqueue_create_buffer[DType.float32](steps * N_V)
    var reference_out = ctx.enqueue_create_buffer[DType.float16](steps * VECTOR_ELEMENTS)
    var tiled_out = ctx.enqueue_create_buffer[DType.float16](steps * VECTOR_ELEMENTS)
    var inplace_out = ctx.enqueue_create_buffer[DType.float16](steps * VECTOR_ELEMENTS)
    var reference_state = ctx.enqueue_create_buffer[DType.float32](steps * STATE_ELEMENTS)
    var tiled_state = ctx.enqueue_create_buffer[DType.float32](steps * STATE_ELEMENTS)

    with state.map_to_host() as host, inplace_state.map_to_host() as inplace:
        for i in range(len(host)):
            value = Float32((i * 13 + 17) % 31 - 15) * 0.0001
            host[i] = value
            inplace[i] = value
    with q.map_to_host() as qh, k.map_to_host() as kh, v.map_to_host() as vh:
        for i in range(len(qh)):
            qh[i] = Float16(Float32((i * 7 + 3) % 19 - 9) * 0.003)
            kh[i] = Float16(Float32((i * 11 + 5) % 23 - 11) * 0.003)
            vh[i] = Float16(Float32((i * 17 + 7) % 29 - 14) * 0.002)
    with g.map_to_host() as gh, beta.map_to_host() as bh:
        for i in range(len(gh)):
            gh[i] = -0.05 - Float32((i * 5 + 1) % 7) * 0.01
            bh[i] = 0.2 + Float32((i * 3 + 2) % 5) * 0.05

    ctx.enqueue_function[deltanet_gated_scan_dynamic_f16](
        reference_out.unsafe_ptr(), reference_state.unsafe_ptr(), state.unsafe_ptr(),
        q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(),
        steps, N_V, D_STATE, grid_dim=N_V, block_dim=D_STATE,
    )
    ctx.enqueue_function[deltanet_gated_scan_dynamic_d128_f16](
        tiled_out.unsafe_ptr(), tiled_state.unsafe_ptr(), state.unsafe_ptr(),
        q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(),
        steps, N_V, D_STATE, grid_dim=N_V * 4, block_dim=32,
    )
    ctx.enqueue_function[deltanet_gated_scan_inplace_dynamic_d128_f16](
        inplace_out.unsafe_ptr(), inplace_state.unsafe_ptr(),
        q.unsafe_ptr(), k.unsafe_ptr(), v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(),
        steps, N_V, D_STATE, grid_dim=N_V * 4, block_dim=32,
    )
    ctx.synchronize()

    with reference_out.map_to_host() as reference, tiled_out.map_to_host() as tiled:
        for i in range(len(reference)):
            if reference[i] != tiled[i]:
                raise Error("różnica wyjścia dynamicznego skanu T=" + String(steps))
    with reference_out.map_to_host() as reference, inplace_out.map_to_host() as inplace:
        for i in range(len(reference)):
            if reference[i] != inplace[i]:
                raise Error("różnica wyjścia in-place T=" + String(steps))
    with reference_state.map_to_host() as reference, tiled_state.map_to_host() as tiled:
        for i in range(len(reference)):
            if reference[i] != tiled[i]:
                raise Error("różnica checkpointu dynamicznego skanu T=" + String(steps))
    with reference_state.map_to_host() as reference, inplace_state.map_to_host() as inplace:
        final_offset = (steps - 1) * STATE_ELEMENTS
        for i in range(len(inplace)):
            if reference[final_offset + i] != inplace[i]:
                raise Error("różnica stanu in-place T=" + String(steps))
    print("PASS dynamic scan T=", steps)


def _tile_width_case(ctx: DeviceContext) raises:
    comptime steps = 128
    var state32 = ctx.enqueue_create_buffer[DType.float32](REAL_STATE_ELEMENTS)
    var state64 = ctx.enqueue_create_buffer[DType.float32](REAL_STATE_ELEMENTS)
    var q = ctx.enqueue_create_buffer[DType.float16](steps * REAL_VECTOR_ELEMENTS)
    var k = ctx.enqueue_create_buffer[DType.float16](steps * REAL_VECTOR_ELEMENTS)
    var v = ctx.enqueue_create_buffer[DType.float16](steps * REAL_VECTOR_ELEMENTS)
    var g = ctx.enqueue_create_buffer[DType.float32](steps * REAL_N_V)
    var beta = ctx.enqueue_create_buffer[DType.float32](steps * REAL_N_V)
    var out32 = ctx.enqueue_create_buffer[DType.float16](steps * REAL_VECTOR_ELEMENTS)
    var out64 = ctx.enqueue_create_buffer[DType.float16](steps * REAL_VECTOR_ELEMENTS)

    with state32.map_to_host() as host32, state64.map_to_host() as host64:
        for i in range(len(host32)):
            value = Float32((i * 13 + 17) % 31 - 15) * 0.0001
            host32[i] = value
            host64[i] = value
    with q.map_to_host() as qh, k.map_to_host() as kh, v.map_to_host() as vh:
        for i in range(len(qh)):
            qh[i] = Float16(Float32((i * 7 + 3) % 19 - 9) * 0.003)
            kh[i] = Float16(Float32((i * 11 + 5) % 23 - 11) * 0.003)
            vh[i] = Float16(Float32((i * 17 + 7) % 29 - 14) * 0.002)
    with g.map_to_host() as gh, beta.map_to_host() as bh:
        for i in range(len(gh)):
            gh[i] = -0.05 - Float32((i * 5 + 1) % 7) * 0.01
            bh[i] = 0.2 + Float32((i * 3 + 2) % 5) * 0.05

    ctx.enqueue_function[deltanet_gated_scan_inplace_dynamic_d128_f16](
        out32.unsafe_ptr(), state32.unsafe_ptr(), q.unsafe_ptr(), k.unsafe_ptr(),
        v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(), steps, REAL_N_V,
        D_STATE, grid_dim=REAL_N_V * 4, block_dim=32,
    )
    ctx.enqueue_function[deltanet_gated_scan_inplace_dynamic_d128_f16](
        out64.unsafe_ptr(), state64.unsafe_ptr(), q.unsafe_ptr(), k.unsafe_ptr(),
        v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(), steps, REAL_N_V,
        D_STATE, grid_dim=REAL_N_V * 2, block_dim=64,
    )
    ctx.synchronize()

    with out32.map_to_host() as reference, out64.map_to_host() as result:
        for i in range(len(reference)):
            if reference[i].to_bits() != result[i].to_bits():
                raise Error("block64 zmienia wyjście skanu DeltaNet")
    with state32.map_to_host() as reference, state64.map_to_host() as result:
        for i in range(len(reference)):
            if reference[i].to_bits() != result[i].to_bits():
                raise Error("block64 zmienia stan skanu DeltaNet")

    var started = perf_counter_ns()
    for _ in range(BENCH_ITERS):
        ctx.enqueue_function[deltanet_gated_scan_inplace_dynamic_d128_f16](
            out32.unsafe_ptr(), state32.unsafe_ptr(), q.unsafe_ptr(), k.unsafe_ptr(),
            v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(), steps, REAL_N_V,
            D_STATE, grid_dim=REAL_N_V * 4, block_dim=32,
        )
    ctx.synchronize()
    var elapsed32 = Float64(perf_counter_ns() - started) / 1e3 / BENCH_ITERS

    started = perf_counter_ns()
    for _ in range(BENCH_ITERS):
        ctx.enqueue_function[deltanet_gated_scan_inplace_dynamic_d128_f16](
            out64.unsafe_ptr(), state64.unsafe_ptr(), q.unsafe_ptr(), k.unsafe_ptr(),
            v.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(), steps, REAL_N_V,
            D_STATE, grid_dim=REAL_N_V * 2, block_dim=64,
        )
    ctx.synchronize()
    var elapsed64 = Float64(perf_counter_ns() - started) / 1e3 / BENCH_ITERS
    print("T=128 block32:", elapsed32, "us; block64:", elapsed64, "us")

    with out32.map_to_host() as reference, out64.map_to_host() as result:
        for i in range(len(reference)):
            if reference[i].to_bits() != result[i].to_bits():
                raise Error("block64 traci zgodność po benchmarku")
    print("PASS dynamic scan block32=block64 T=128")


def main() raises:
    var ctx = DeviceContext()
    _case[1](ctx)
    _case[5](ctx)
    _case[31](ctx)
    _case[32](ctx)
    _tile_width_case(ctx)
