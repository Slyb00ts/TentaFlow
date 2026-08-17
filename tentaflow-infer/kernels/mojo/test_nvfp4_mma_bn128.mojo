# =============================================================================
# Plik: test_nvfp4_mma_bn128.mojo
# Opis: Sprawdza bitowa zgodnosc i canary wariantow jednobarierowych wzgledem BN64.
# Przyklad: pixi run mojo test_nvfp4_mma_bn128.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from src.nvfp4_gguf_mma import gemm_nvfp4_gguf_mma_f16_bm128_prefetch
from src.nvfp4_gguf_mma_bn128 import (
    gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1,
    gemm_nvfp4_gguf_mma_f16_bm128_bn128,
)

comptime TOKENS = 129
comptime ROWS = 193
comptime COLS = 256
comptime GUARD = 64


def main() raises:
    var ctx = DeviceContext()
    output_elements = TOKENS * ROWS
    var weights = ctx.enqueue_create_buffer[DType.uint8](
        ROWS * (COLS // 64) * 36
    )
    var x = ctx.enqueue_create_buffer[DType.float16](TOKENS * COLS)
    var reference = ctx.enqueue_create_buffer[DType.float16](
        output_elements + 2 * GUARD
    )
    var result = ctx.enqueue_create_buffer[DType.float16](
        output_elements + 2 * GUARD
    )
    var result64 = ctx.enqueue_create_buffer[DType.float16](
        output_elements + 2 * GUARD
    )

    with weights.map_to_host() as values:
        for i in range(len(values)):
            values[i] = UInt8((i * 29 + 11) & 0xFF)
        for i in range(0, len(values), 36):
            values[i] = UInt8(0x38)
            values[i + 1] = UInt8(0x30)
            values[i + 2] = UInt8(0x40)
            values[i + 3] = UInt8(0x28)
    with x.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32((i * 17 % 31) - 15) * 0.03125)
    with reference.map_to_host() as left, result.map_to_host() as right, result64.map_to_host() as right64:
        for i in range(len(left)):
            left[i] = Float16(19.0)
            right[i] = Float16(19.0)
            right64[i] = Float16(19.0)

    ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128_prefetch](
        reference.unsafe_ptr() + GUARD, weights.unsafe_ptr(), x.unsafe_ptr(),
        COLS, ROWS, TOKENS, Float32(0.625),
        grid_dim=((ROWS + 63) // 64, (TOKENS + 127) // 128), block_dim=256,
    )
    ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128_bn128](
        result.unsafe_ptr() + GUARD, weights.unsafe_ptr(), x.unsafe_ptr(),
        COLS, ROWS, TOKENS, Float32(0.625),
        grid_dim=((ROWS + 127) // 128, (TOKENS + 127) // 128), block_dim=256,
    )
    ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1](
        result64.unsafe_ptr() + GUARD, weights.unsafe_ptr(), x.unsafe_ptr(),
        COLS, ROWS, TOKENS, Float32(0.625),
        grid_dim=((ROWS + 63) // 64, (TOKENS + 127) // 128), block_dim=256,
    )
    ctx.synchronize()

    with reference.map_to_host() as expected, result.map_to_host() as actual:
        for i in range(len(expected)):
            if expected[i].to_bits() != actual[i].to_bits():
                raise Error("BN128 zmienia wynik lub narusza canary na pozycji " + String(i))
    with reference.map_to_host() as expected, result64.map_to_host() as actual:
        for i in range(len(expected)):
            if expected[i].to_bits() != actual[i].to_bits():
                raise Error("BN64 sync1 zmienia wynik lub narusza canary na pozycji " + String(i))
    print("raw NVFP4 BN64 sync1 i BN128 bit-exact z canary: PASS")
