# =============================================================================
# Plik: test_q8_0_f16_exact_batch.mojo
# Opis: Porównuje dokładne projekcje Q8_0×F16 T3/T4 z seryjnym GEMV F32.
# Przykład: pixi run mojo test_q8_0_f16_exact_batch.mojo
# =============================================================================

from std.gpu import WARP_SIZE
from std.gpu.host import DeviceContext
from src.gemv2 import gemv_q8_0_out_f32_v2
from src.q8_0_batch import (
    gemm_q8_0_f16_exact_out_f32_b3,
    gemm_q8_0_f16_exact_out_f32_b4,
)

comptime ROWS = 257
comptime COLS = 5120
comptime WEIGHT_BYTES = ROWS * (COLS // 32) * 34


def _case[steps: Int](ctx: DeviceContext) raises:
    var weights = ctx.enqueue_create_buffer[DType.uint8](WEIGHT_BYTES)
    var x = ctx.enqueue_create_buffer[DType.float16](steps * COLS)
    var reference = ctx.enqueue_create_buffer[DType.float32](steps * ROWS)
    var generic = ctx.enqueue_create_buffer[DType.float32](steps * ROWS)

    with weights.map_to_host() as values:
        for block in range(ROWS * (COLS // 32)):
            offset = block * 34
            scale = Float16(0.0005 + Float32((block * 13 + 7) % 97) * 0.00003125)
            bits = scale.to_bits()
            values[offset] = UInt8(bits & 0xFF)
            values[offset + 1] = UInt8(bits >> 8)
            for element in range(32):
                values[offset + 2 + element] = UInt8(Int8((block * 17 + element * 29 + 11) % 255 - 127))
    with x.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32((i * 37 + 19) % 257 - 128) * (1.0 / 91.0))

    for token in range(steps):
        ctx.enqueue_function[gemv_q8_0_out_f32_v2](reference.unsafe_ptr() + token * ROWS, weights.unsafe_ptr(), x.unsafe_ptr() + token * COLS, COLS, ROWS, grid_dim=(ROWS + 7) // 8, block_dim=256)
    comptime if steps == 3:
        ctx.enqueue_function[gemm_q8_0_f16_exact_out_f32_b3](generic.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS, steps, grid_dim=(ROWS + 7) // 8, block_dim=8 * WARP_SIZE)
    else:
        ctx.enqueue_function[gemm_q8_0_f16_exact_out_f32_b4](generic.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS, steps, grid_dim=(ROWS + 7) // 8, block_dim=8 * WARP_SIZE)
    ctx.synchronize()

    with reference.map_to_host() as expected, generic.map_to_host() as actual:
        for i in range(len(expected)):
            if expected[i].to_bits() != actual[i].to_bits():
                raise Error("niezgodny wynik generic T=" + String(steps) + ", indeks=" + String(i))
        for token in range(steps):
            var expected_argmax = 0
            var actual_argmax = 0
            for row in range(1, ROWS):
                if expected[token * ROWS + row] > expected[token * ROWS + expected_argmax]:
                    expected_argmax = row
                if actual[token * ROWS + row] > actual[token * ROWS + actual_argmax]:
                    actual_argmax = row
            if expected_argmax != actual_argmax:
                raise Error("niezgodny argmax T=" + String(steps))
    print("PASS T=", steps, "rows=", ROWS)


def main() raises:
    var ctx = DeviceContext()
    _case[3](ctx)
    _case[4](ctx)
