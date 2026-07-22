# =============================================================================
# Plik: test_nvfp4_gguf_batch.mojo
# Opis: Golden szybkich kerneli F16 dla surowego GGUF NVFP4 T=2/3/4.
# Przyklad: pixi run mojo test_nvfp4_gguf_batch.mojo
# =============================================================================

from std.gpu import WARP_SIZE
from std.gpu.host import DeviceContext
from std.memory import bitcast
from src.nvfp4_gguf_batch import (
    gemm_nvfp4_gguf_f16_b2,
    gemm_nvfp4_gguf_f16_b3,
    gemm_nvfp4_gguf_f16_b4,
    gemm_nvfp4_gguf_f16_b3_nvidia,
    gemm_nvfp4_gguf_f16_b4_nvidia,
)


def _e2m1_value(code: UInt8) -> Float32:
    comptime values = SIMD[DType.float32, 16](
        0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0,
        -0.0, -0.5, -1.0, -1.5, -2.0, -3.0, -4.0, -6.0,
    )
    return values[Int(code & 0x0F)]


def _ue4m3_value(code: UInt8) -> Float32:
    if code == 0 or code == 0x7F:
        return 0.0
    exponent = Int((code >> 3) & 0x0F)
    mantissa = Int(code & 0x07)
    if exponent == 0:
        return Float32(mantissa) * (1.0 / 512.0)
    bits = UInt32((exponent + 120) << 23 | mantissa << 20)
    return bitcast[DType.float32, 1](SIMD[DType.uint32, 1](bits))[0]


def _golden(ctx: DeviceContext) raises:
    comptime TOKENS = 4
    comptime ROWS = 11
    comptime COLS = 192
    comptime OUTPUT_SCALE = 0.625
    var weights = ctx.enqueue_create_buffer[DType.uint8](ROWS * (COLS // 64) * 36)
    var x = ctx.enqueue_create_buffer[DType.float16](TOKENS * COLS)
    var y2 = ctx.enqueue_create_buffer[DType.float16](2 * ROWS)
    var y3 = ctx.enqueue_create_buffer[DType.float16](3 * ROWS)
    var y4 = ctx.enqueue_create_buffer[DType.float16](4 * ROWS)
    var y3_nvidia = ctx.enqueue_create_buffer[DType.float16](3 * ROWS)
    var y4_nvidia = ctx.enqueue_create_buffer[DType.float16](4 * ROWS)
    with weights.map_to_host() as values:
        for i in range(len(values)):
            values[i] = UInt8((i * 29 + 11) & 0xFF)
        for i in range(0, len(values), 36):
            values[i] = 0x38
            values[i + 1] = 0x30
            values[i + 2] = 0x40
            values[i + 3] = 0x28
        values[0] = 0x7F
    with x.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32((i * 17 % 31) - 15) * 0.03125)

    ctx.enqueue_function[gemm_nvfp4_gguf_f16_b2](
        y2.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS,
        2, Float32(OUTPUT_SCALE), grid_dim=ROWS, block_dim=WARP_SIZE,
    )
    ctx.enqueue_function[gemm_nvfp4_gguf_f16_b3](
        y3.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS,
        3, Float32(OUTPUT_SCALE), grid_dim=ROWS, block_dim=WARP_SIZE,
    )
    ctx.enqueue_function[gemm_nvfp4_gguf_f16_b4](
        y4.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS,
        4, Float32(OUTPUT_SCALE), grid_dim=ROWS, block_dim=WARP_SIZE,
    )
    ctx.enqueue_function[gemm_nvfp4_gguf_f16_b3_nvidia](
        y3_nvidia.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS,
        3, Float32(OUTPUT_SCALE), grid_dim=(ROWS + 1) // 2,
        block_dim=2 * WARP_SIZE,
    )
    ctx.enqueue_function[gemm_nvfp4_gguf_f16_b4_nvidia](
        y4_nvidia.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS,
        4, Float32(OUTPUT_SCALE), grid_dim=(ROWS + 1) // 2,
        block_dim=2 * WARP_SIZE,
    )
    ctx.synchronize()

    with weights.map_to_host() as w, x.map_to_host() as xv, y2.map_to_host() as result2, y3.map_to_host() as result3, y4.map_to_host() as result4, y3_nvidia.map_to_host() as result3_nvidia, y4_nvidia.map_to_host() as result4_nvidia:
        for token in range(TOKENS):
            for row in range(ROWS):
                var expected: Float32 = 0.0
                for block in range(COLS // 64):
                    base = (row * (COLS // 64) + block) * 36
                    for group in range(4):
                        scale = _ue4m3_value(w[base + group])
                        column = token * COLS + block * 64 + group * 16
                        for element in range(8):
                            code = w[base + 4 + group * 8 + element]
                            expected += scale * _e2m1_value(code & 0x0F) * Float32(
                                xv[column + element]
                            )
                            expected += scale * _e2m1_value(code >> 4) * Float32(
                                xv[column + element + 8]
                            )
                expected *= OUTPUT_SCALE
                tolerance = 0.02 * (abs(expected) + 1.0)
                if token < 2 and abs(Float32(result2[token * ROWS + row]) - expected) > tolerance:
                    raise Error("niezgodny wynik F16 B2 GGUF NVFP4")
                if token < 3 and abs(Float32(result3[token * ROWS + row]) - expected) > tolerance:
                    raise Error("niezgodny wynik F16 B3 GGUF NVFP4")
                if abs(Float32(result4[token * ROWS + row]) - expected) > tolerance:
                    raise Error("niezgodny wynik F16 B4 GGUF NVFP4")
                if token < 3 and result3_nvidia[token * ROWS + row].to_bits() != result3[token * ROWS + row].to_bits():
                    raise Error("szybki NVIDIA B3 nie jest bit-exact")
                if result4_nvidia[token * ROWS + row].to_bits() != result4[token * ROWS + row].to_bits():
                    raise Error("szybki NVIDIA B4 nie jest bit-exact")
    print("golden F16 NVFP4 T=2/3/4 0x7f/output_scale: PASS")


def main() raises:
    var ctx = DeviceContext()
    _golden(ctx)
