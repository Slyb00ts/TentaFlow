# =============================================================================
# Plik: test_nvfp4_gguf_b1.mojo
# Opis: Sprawdza B3/B4 względem osobnych projekcji NVIDIA B1 na realnych kształtach.
# Przykład: /home/critix/.pixi/bin/pixi run mojo test_nvfp4_gguf_b1.mojo
# =============================================================================

from std.gpu import WARP_SIZE
from std.gpu.host import DeviceContext
from src.nvfp4_gguf_batch import (
    gemm_nvfp4_gguf_f16_b1_nvidia,
    gemm_nvfp4_gguf_f16_b3_nvidia,
    gemm_nvfp4_gguf_f16_b4_nvidia,
)


def _case[rows: Int, cols: Int, tokens: Int](ctx: DeviceContext) raises:
    blocks = rows * (cols // 64)
    var weights = ctx.enqueue_create_buffer[DType.uint8](blocks * 36)
    var x = ctx.enqueue_create_buffer[DType.float16](tokens * cols)
    var reference = ctx.enqueue_create_buffer[DType.float16](tokens * rows)
    var actual = ctx.enqueue_create_buffer[DType.float16](tokens * rows)
    with weights.map_to_host() as values:
        for i in range(len(values)):
            values[i] = UInt8((i * 29 + 11) & 0xFF)
        for block in range(blocks):
            offset = block * 36
            values[offset] = UInt8(0x18 + block % 5)
            values[offset + 1] = UInt8(0x20 + block % 7)
            values[offset + 2] = UInt8(0x28 + block % 3)
            values[offset + 3] = UInt8(0x30 + block % 9)
    with x.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(Float32((i * 23 + 7) % 127 - 63) * 0.015625)

    for token in range(tokens):
        ctx.enqueue_function[gemm_nvfp4_gguf_f16_b1_nvidia](
            reference.unsafe_ptr() + token * rows, weights.unsafe_ptr(),
            x.unsafe_ptr() + token * cols, cols, rows, 1, Float32(0.625),
            grid_dim=(rows + 1) // 2, block_dim=2 * WARP_SIZE,
        )
    comptime if tokens == 3:
        ctx.enqueue_function[gemm_nvfp4_gguf_f16_b3_nvidia](
            actual.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), cols,
            rows, tokens, Float32(0.625), grid_dim=(rows + 1) // 2,
            block_dim=2 * WARP_SIZE,
        )
    else:
        ctx.enqueue_function[gemm_nvfp4_gguf_f16_b4_nvidia](
            actual.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), cols,
            rows, tokens, Float32(0.625), grid_dim=(rows + 1) // 2,
            block_dim=2 * WARP_SIZE,
        )
    ctx.synchronize()
    with reference.map_to_host() as expected, actual.map_to_host() as got:
        for i in range(len(expected)):
            if expected[i].to_bits() != got[i].to_bits():
                raise Error("NVIDIA B1 nie jest bitowo zgodny z B3/B4")
    print("PASS T=", tokens, " rows=", rows, " cols=", cols)


def main() raises:
    var ctx = DeviceContext()
    _case[12288, 5120, 3](ctx)
    _case[12288, 5120, 4](ctx)
    _case[1024, 5120, 3](ctx)
    _case[1024, 5120, 4](ctx)
    _case[5120, 6144, 3](ctx)
    _case[5120, 6144, 4](ctx)
    _case[17408, 5120, 3](ctx)
    _case[17408, 5120, 4](ctx)
    _case[5120, 17408, 3](ctx)
    _case[5120, 17408, 4](ctx)
