# =============================================================================
# Plik: test_nvfp4_gguf_mma.mojo
# Opis: Sprawdza bitową zgodność kafli BM32 i BM128 dla surowego GGUF NVFP4.
# Przykład: pixi run mojo test_nvfp4_gguf_mma.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from src.nvfp4_gguf_mma import (
    gemm_nvfp4_gguf_mma_f16_bm32,
    gemm_nvfp4_gguf_mma_f16_bm128,
    gemm_nvfp4_gguf_mma_f16_bm128_bn32,
)

comptime TOKENS = 129
comptime ROWS = 67
comptime COLS = 192
comptime OUTPUT_SCALE = 0.625
comptime REPEATS = 10


def _repeated[rows: Int, cols: Int, block_m: Int, block_n: Int](ctx: DeviceContext) raises:
    comptime tokens = block_m
    comptime output_elements = tokens * rows
    var weights = ctx.enqueue_create_buffer[DType.uint8](
        rows * (cols // 64) * 36
    )
    var x = ctx.enqueue_create_buffer[DType.float16](tokens * cols)
    var outputs = ctx.enqueue_create_buffer[DType.float16](
        REPEATS * output_elements
    )
    with weights.map_to_host() as values:
        for i in range(len(values)):
            values[i] = UInt8((i * 29 + 11) & 0xFF)
        for i in range(0, len(values), 36):
            values[i] = 0x38
            values[i + 1] = 0x30
            values[i + 2] = 0x40
            values[i + 3] = 0x28
    with x.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32((i * 17 % 31) - 15) * 0.03125)
    with outputs.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32((i // output_elements) + 1))

    for repeat in range(REPEATS):
        comptime if block_m == 32:
            ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm32](
                outputs.unsafe_ptr() + repeat * output_elements,
                weights.unsafe_ptr(), x.unsafe_ptr(), cols, rows, tokens,
                Float32(OUTPUT_SCALE), grid_dim=((rows + 63) // 64, 1),
                block_dim=64,
            )
        elif block_n == 64:
            ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128](
                outputs.unsafe_ptr() + repeat * output_elements,
                weights.unsafe_ptr(), x.unsafe_ptr(), cols, rows, tokens,
                Float32(OUTPUT_SCALE), grid_dim=((rows + 63) // 64, 1),
                block_dim=256,
            )
        else:
            ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128_bn32](
                outputs.unsafe_ptr() + repeat * output_elements,
                weights.unsafe_ptr(), x.unsafe_ptr(), cols, rows, tokens,
                Float32(OUTPUT_SCALE), grid_dim=((rows + 31) // 32, 1),
                block_dim=128,
            )
    ctx.synchronize()

    with outputs.map_to_host() as result:
        for repeat in range(1, REPEATS):
            for i in range(output_elements):
                if result[i].to_bits() != result[repeat * output_elements + i].to_bits():
                    raise Error(
                        "GGUF NVFP4 nie jest deterministyczny dla BM"
                        + String(block_m) + "x" + String(block_n) + " "
                        + String(rows) + "x" + String(cols)
                    )
    print("repeated BM/BN", block_m, "/", block_n, " GGUF NVFP4", rows, "x", cols, ": PASS")


def main() raises:
    var ctx = DeviceContext()
    var weights = ctx.enqueue_create_buffer[DType.uint8](
        ROWS * (COLS // 64) * 36
    )
    var x = ctx.enqueue_create_buffer[DType.float16](TOKENS * COLS)
    var y32 = ctx.enqueue_create_buffer[DType.float16](TOKENS * ROWS)
    var y128 = ctx.enqueue_create_buffer[DType.float16](TOKENS * ROWS)
    var y128bn32 = ctx.enqueue_create_buffer[DType.float16](TOKENS * ROWS)
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

    ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm32](
        y32.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS,
        TOKENS, Float32(OUTPUT_SCALE), grid_dim=((ROWS + 63) // 64, 5),
        block_dim=64,
    )
    ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128](
        y128.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS,
        TOKENS, Float32(OUTPUT_SCALE), grid_dim=((ROWS + 63) // 64, 2),
        block_dim=256,
    )
    ctx.enqueue_function[gemm_nvfp4_gguf_mma_f16_bm128_bn32](
        y128bn32.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS,
        TOKENS, Float32(OUTPUT_SCALE), grid_dim=((ROWS + 31) // 32, 2),
        block_dim=128,
    )
    ctx.synchronize()

    with y32.map_to_host() as reference, y128.map_to_host() as result:
        for i in range(len(reference)):
            if result[i].to_bits() != reference[i].to_bits():
                raise Error("BM128 GGUF NVFP4 nie jest bit-exact względem BM32")
    with y128.map_to_host() as reference, y128bn32.map_to_host() as result:
        for i in range(len(reference)):
            if result[i].to_bits() != reference[i].to_bits():
                raise Error("BN32 GGUF NVFP4 nie jest bit-exact względem BN64")
    print("golden MMA GGUF NVFP4 BM32=BM128: PASS")
    print("golden MMA GGUF NVFP4 BN64=BN32: PASS")
    _repeated[17408, 5120, 32, 64](ctx)
    _repeated[5120, 17408, 32, 64](ctx)
    _repeated[17408, 5120, 128, 64](ctx)
    _repeated[5120, 17408, 128, 64](ctx)
    _repeated[17408, 5120, 128, 32](ctx)
    _repeated[5120, 17408, 128, 32](ctx)
