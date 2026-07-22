# =============================================================================
# Plik: test_q8_0_dp4a.mojo
# Opis: Sprawdza bitowa zgodnosc NVIDIA DP4A B3/B4 na ksztaltach Qwen.
# Przyklad: pixi run mojo test_q8_0_dp4a.mojo
# =============================================================================

from std.gpu import WARP_SIZE
from std.gpu.host import DeviceContext
from src.gemm import quantize_act_q8_1
from src.q8_0_batch import (
    gemm_q8_0_i8mma_b3,
    gemm_q8_0_i8mma_b4,
    gemm_q8_0_i8mma_out_f32_b3,
    gemm_q8_0_i8mma_out_f32_b4,
    gemm_q8_0_dp4a_b3_nvidia,
    gemm_q8_0_dp4a_b4_nvidia,
    gemm_q8_0_dp4a_out_f32_b3_nvidia,
    gemm_q8_0_dp4a_out_f32_b4_nvidia,
)


def _case[rows: Int, tokens: Int](ctx: DeviceContext) raises:
    comptime COLS = 5120
    var weights = ctx.enqueue_create_buffer[DType.uint8](rows * (COLS // 32) * 34)
    var x = ctx.enqueue_create_buffer[DType.float16](tokens * COLS)
    var xq = ctx.enqueue_create_buffer[DType.int8](tokens * COLS)
    var xd = ctx.enqueue_create_buffer[DType.float32](tokens * (COLS // 32))
    var xsm = ctx.enqueue_create_buffer[DType.float32](tokens * (COLS // 32))
    var reference = ctx.enqueue_create_buffer[DType.float16](tokens * rows)
    var actual = ctx.enqueue_create_buffer[DType.float16](tokens * rows)
    var reference_f32 = ctx.enqueue_create_buffer[DType.float32](tokens * rows)
    var actual_f32 = ctx.enqueue_create_buffer[DType.float32](tokens * rows)
    with weights.map_to_host() as values:
        for i in range(len(values)):
            values[i] = UInt8((i * 29 + 11) & 0xFF)
        for block in range(rows * (COLS // 32)):
            offset = block * 34
            bits = Float16(0.015625 + Float32(block % 11) * 0.0078125).to_bits()
            values[offset] = UInt8(bits & 0xFF)
            values[offset + 1] = UInt8(bits >> 8)
    with x.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32((i * 17 + 3) % 31 - 15) * 0.03125)

    ctx.enqueue_function[quantize_act_q8_1](xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), x.unsafe_ptr(), COLS, tokens, grid_dim=(tokens * (COLS // 32) + 255) // 256, block_dim=256)
    comptime if tokens == 3:
        ctx.enqueue_function[gemm_q8_0_i8mma_b3](reference.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(), COLS, rows, tokens, grid_dim=(rows + 7) // 8, block_dim=8 * WARP_SIZE)
        ctx.enqueue_function[gemm_q8_0_dp4a_b3_nvidia](actual.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(), COLS, rows, tokens, grid_dim=(rows + 3) // 4, block_dim=4 * WARP_SIZE)
        ctx.enqueue_function[gemm_q8_0_i8mma_out_f32_b3](reference_f32.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(), COLS, rows, tokens, grid_dim=(rows + 7) // 8, block_dim=8 * WARP_SIZE)
        ctx.enqueue_function[gemm_q8_0_dp4a_out_f32_b3_nvidia](actual_f32.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(), COLS, rows, tokens, grid_dim=(rows + 3) // 4, block_dim=4 * WARP_SIZE)
    else:
        ctx.enqueue_function[gemm_q8_0_i8mma_b4](reference.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(), COLS, rows, tokens, grid_dim=(rows + 7) // 8, block_dim=8 * WARP_SIZE)
        ctx.enqueue_function[gemm_q8_0_dp4a_b4_nvidia](actual.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(), COLS, rows, tokens, grid_dim=(rows + 3) // 4, block_dim=4 * WARP_SIZE)
        ctx.enqueue_function[gemm_q8_0_i8mma_out_f32_b4](reference_f32.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(), COLS, rows, tokens, grid_dim=(rows + 7) // 8, block_dim=8 * WARP_SIZE)
        ctx.enqueue_function[gemm_q8_0_dp4a_out_f32_b4_nvidia](actual_f32.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(), COLS, rows, tokens, grid_dim=(rows + 3) // 4, block_dim=4 * WARP_SIZE)
    ctx.synchronize()

    with reference.map_to_host() as expected, actual.map_to_host() as got:
        for i in range(len(expected)):
            if expected[i].to_bits() != got[i].to_bits():
                raise Error("niezgodny wynik F16 DP4A")
    with reference_f32.map_to_host() as expected, actual_f32.map_to_host() as got:
        for i in range(len(expected)):
            if expected[i] != got[i]:
                raise Error("niezgodny wynik F32 DP4A")
    print("PASS T=", tokens, " rows=", rows)


def main() raises:
    var ctx = DeviceContext()
    _case[48, 3](ctx)
    _case[48, 4](ctx)
    _case[5120, 3](ctx)
    _case[5120, 4](ctx)
    _case[6144, 3](ctx)
    _case[6144, 4](ctx)
