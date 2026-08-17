# =============================================================================
# Plik: test_deltanet_scan_cycles.mojo
# Opis: Porównuje stary i kafelkowany skan DeltaNet przez wiele cykli commit.
# Przykład: pixi run mojo test_deltanet_scan_cycles.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from src.deltanet_verify import (
    deltanet_gated_scan_t3_f16,
    deltanet_gated_scan_t4_f16,
    deltanet_gated_scan_t3_d128_f16,
    deltanet_gated_scan_t4_d128_f16,
    deltanet_commit_checkpoint_f32,
)

comptime N_V = 32
comptime D_STATE = 128
comptime CYCLES = 100
comptime VECTOR_ELEMENTS = N_V * D_STATE
comptime STATE_ELEMENTS = N_V * D_STATE * D_STATE
comptime PREFIX_ELEMENTS = STATE_ELEMENTS + 37


def _case[steps: Int](ctx: DeviceContext) raises:
    var old_state = ctx.enqueue_create_buffer[DType.float32](STATE_ELEMENTS)
    var new_state = ctx.enqueue_create_buffer[DType.float32](STATE_ELEMENTS)
    var q = ctx.enqueue_create_buffer[DType.float16](CYCLES * steps * VECTOR_ELEMENTS)
    var k = ctx.enqueue_create_buffer[DType.float16](CYCLES * steps * VECTOR_ELEMENTS)
    var v = ctx.enqueue_create_buffer[DType.float16](CYCLES * steps * VECTOR_ELEMENTS)
    var g = ctx.enqueue_create_buffer[DType.float32](CYCLES * steps * N_V)
    var beta = ctx.enqueue_create_buffer[DType.float32](CYCLES * steps * N_V)
    var old_out = ctx.enqueue_create_buffer[DType.float16](steps * VECTOR_ELEMENTS)
    var new_out = ctx.enqueue_create_buffer[DType.float16](steps * VECTOR_ELEMENTS)
    var old_checkpoints = ctx.enqueue_create_buffer[DType.float32](PREFIX_ELEMENTS + steps * STATE_ELEMENTS)
    var new_checkpoints = ctx.enqueue_create_buffer[DType.float32](PREFIX_ELEMENTS + steps * STATE_ELEMENTS)
    var accepted_index = ctx.enqueue_create_buffer[DType.int32](1)

    with old_state.map_to_host() as old_h, new_state.map_to_host() as new_h:
        for i in range(STATE_ELEMENTS):
            value = Float32((i * 13 + 17) % 31 - 15) * 0.0001
            old_h[i] = value
            new_h[i] = value
    with q.map_to_host() as qh, k.map_to_host() as kh, v.map_to_host() as vh:
        for i in range(len(qh)):
            qh[i] = Float16(Float32((i * 7 + 3) % 19 - 9) * 0.003)
            kh[i] = Float16(Float32((i * 11 + 5) % 23 - 11) * 0.003)
            vh[i] = Float16(Float32((i * 17 + 7) % 29 - 14) * 0.002)
    with g.map_to_host() as gh, beta.map_to_host() as bh:
        for i in range(len(gh)):
            gh[i] = -0.05 - Float32((i * 5 + 1) % 7) * 0.01
            bh[i] = 0.2 + Float32((i * 3 + 2) % 5) * 0.05
    with old_checkpoints.map_to_host() as old_h, new_checkpoints.map_to_host() as new_h:
        for i in range(PREFIX_ELEMENTS):
            value = Float32((i * 19 + 11) % 37 - 18) * 0.125
            old_h[i] = value
            new_h[i] = value

    for cycle in range(CYCLES):
        vector_offset = cycle * steps * VECTOR_ELEMENTS
        gate_offset = cycle * steps * N_V
        accepted = cycle % steps + 1
        with accepted_index.map_to_host() as accepted_h:
            accepted_h[0] = Int32(accepted)

        comptime if steps == 3:
            ctx.enqueue_function[deltanet_gated_scan_t3_f16](
                old_out.unsafe_ptr(), old_checkpoints.unsafe_ptr() + PREFIX_ELEMENTS,
                old_state.unsafe_ptr(), q.unsafe_ptr() + vector_offset,
                k.unsafe_ptr() + vector_offset, v.unsafe_ptr() + vector_offset,
                g.unsafe_ptr() + gate_offset, beta.unsafe_ptr() + gate_offset,
                N_V, D_STATE, grid_dim=N_V, block_dim=D_STATE,
            )
            ctx.enqueue_function[deltanet_gated_scan_t3_d128_f16](
                new_out.unsafe_ptr(), new_checkpoints.unsafe_ptr() + PREFIX_ELEMENTS,
                new_state.unsafe_ptr(), q.unsafe_ptr() + vector_offset,
                k.unsafe_ptr() + vector_offset, v.unsafe_ptr() + vector_offset,
                g.unsafe_ptr() + gate_offset, beta.unsafe_ptr() + gate_offset,
                N_V, D_STATE, grid_dim=N_V * 4, block_dim=32,
            )
        else:
            ctx.enqueue_function[deltanet_gated_scan_t4_f16](
                old_out.unsafe_ptr(), old_checkpoints.unsafe_ptr() + PREFIX_ELEMENTS,
                old_state.unsafe_ptr(), q.unsafe_ptr() + vector_offset,
                k.unsafe_ptr() + vector_offset, v.unsafe_ptr() + vector_offset,
                g.unsafe_ptr() + gate_offset, beta.unsafe_ptr() + gate_offset,
                N_V, D_STATE, grid_dim=N_V, block_dim=D_STATE,
            )
            ctx.enqueue_function[deltanet_gated_scan_t4_d128_f16](
                new_out.unsafe_ptr(), new_checkpoints.unsafe_ptr() + PREFIX_ELEMENTS,
                new_state.unsafe_ptr(), q.unsafe_ptr() + vector_offset,
                k.unsafe_ptr() + vector_offset, v.unsafe_ptr() + vector_offset,
                g.unsafe_ptr() + gate_offset, beta.unsafe_ptr() + gate_offset,
                N_V, D_STATE, grid_dim=N_V * 4, block_dim=32,
            )

        ctx.enqueue_function[deltanet_commit_checkpoint_f32](
            old_state.unsafe_ptr(), old_checkpoints.unsafe_ptr() + PREFIX_ELEMENTS,
            accepted_index.unsafe_ptr(), STATE_ELEMENTS, steps,
            grid_dim=(STATE_ELEMENTS + 255) // 256, block_dim=256,
        )
        ctx.enqueue_function[deltanet_commit_checkpoint_f32](
            new_state.unsafe_ptr(), new_checkpoints.unsafe_ptr() + PREFIX_ELEMENTS,
            accepted_index.unsafe_ptr(), STATE_ELEMENTS, steps,
            grid_dim=(STATE_ELEMENTS + 255) // 256, block_dim=256,
        )
        ctx.synchronize()

        with old_out.map_to_host() as old_h, new_out.map_to_host() as new_h:
            for i in range(len(old_h)):
                if old_h[i] != new_h[i]:
                    raise Error("różnica wyjścia T=" + String(steps) + ", cykl=" + String(cycle))
        with old_checkpoints.map_to_host() as old_h, new_checkpoints.map_to_host() as new_h:
            for i in range(PREFIX_ELEMENTS + steps * STATE_ELEMENTS):
                if old_h[i] != new_h[i]:
                    raise Error("różnica checkpointu T=" + String(steps) + ", cykl=" + String(cycle))
        with old_state.map_to_host() as old_h, new_state.map_to_host() as new_h:
            for i in range(STATE_ELEMENTS):
                if old_h[i] != new_h[i]:
                    raise Error("różnica stanu T=" + String(steps) + ", cykl=" + String(cycle))

    print("PASS T=", steps, "cycles=", CYCLES)


def main() raises:
    var ctx = DeviceContext()
    _case[3](ctx)
    _case[4](ctx)
