# =============================================================================
# Plik: test_gemm_nvfp4_wmma.mojo
# Opis: Test złoty GEMM-u NVFP4 na WMMA — porównuje z tą samą matematyką liczoną
#       na hoście w f32, na kształtach z ogonami po tokenach i po wierszach.
# Przykład: pixi run mojo run -I . test_gemm_nvfp4_wmma.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.random import random_si64, seed

from src.nvfp4_gguf_wmma import (
    gemm_nvfp4_gguf_wmma_f16_bm32,
    gemm_nvfp4_gguf_wmma_f16_bm256,
)

comptime BLOCK_VALUES = 64
comptime BLOCK_BYTES = 36


def _e2m1(code: Int) -> Float64:
    magnitude = code & 0x07
    var value: Float64 = 0.0
    if magnitude < 2:
        value = Float64(magnitude) * 0.5
    else:
        value = Float64((2 + (magnitude & 1)) << (magnitude >> 1)) * 0.25
    if (code >> 3) & 1 == 1:
        return -value
    return value


def _ue4m3(code: Int) -> Float64:
    if code == 0 or code == 0x7F:
        return 0.0
    exponent = (code >> 3) & 0x0F
    mantissa = code & 0x07
    if exponent == 0:
        return Float64(mantissa) / 512.0
    var scaled: Float64 = 1.0
    for _ in range(abs(exponent - 7)):
        if exponent >= 7:
            scaled *= 2.0
        else:
            scaled *= 0.5
    return (1.0 + Float64(mantissa) / 8.0) * scaled


def check(ctx: DeviceContext, n_tokens: Int, n_rows: Int, n_cols: Int, tile: Int) raises:
    blocks = n_cols // BLOCK_VALUES
    var wb = ctx.enqueue_create_host_buffer[DType.uint8](n_rows * blocks * BLOCK_BYTES)
    var xb = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_cols)
    ctx.synchronize()
    for r in range(n_rows):
        for b in range(blocks):
            base = (r * blocks + b) * BLOCK_BYTES
            for s in range(4):
                # Skale w okolicy 1.0, żeby wynik nie wychodził poza f16.
                wb[base + s] = UInt8(0x38 + ((r + b + s) % 5))
            for i in range(32):
                wb[base + 4 + i] = UInt8(Int(random_si64(0, 255)))
    for t in range(n_tokens):
        for k in range(n_cols):
            xb[t * n_cols + k] = Float16(Float64(Int(random_si64(-8, 8))) * 0.125)

    var wd = ctx.enqueue_create_buffer[DType.uint8](n_rows * blocks * BLOCK_BYTES)
    var xd = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_cols)
    var yd = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(wd, wb)
    ctx.enqueue_copy(xd, xb)
    ctx.synchronize()

    output_scale = Float32(0.75)
    var bm = 32
    var bn = 64
    if tile == 1:
        bm = 256
    grid_x = (n_rows + bn - 1) // bn
    grid_y = (n_tokens + bm - 1) // bm
    if tile == 0:
        ctx.enqueue_function[gemm_nvfp4_gguf_wmma_f16_bm32](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xd.unsafe_ptr(),
            n_cols, n_rows, n_tokens, output_scale,
            grid_dim=(grid_x, grid_y), block_dim=128,
        )
    else:
        ctx.enqueue_function[gemm_nvfp4_gguf_wmma_f16_bm256](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xd.unsafe_ptr(),
            n_cols, n_rows, n_tokens, output_scale,
            grid_dim=(grid_x, grid_y), block_dim=256,
        )
    ctx.synchronize()

    var yh = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(yh, yd)
    ctx.synchronize()

    var worst: Float64 = 0.0
    for t in range(n_tokens):
        for r in range(n_rows):
            var want: Float64 = 0.0
            for b in range(blocks):
                base = (r * blocks + b) * BLOCK_BYTES
                for s in range(4):
                    scale = _ue4m3(Int(wb[base + s]))
                    column = b * BLOCK_VALUES + s * 16
                    for i in range(8):
                        byte = Int(wb[base + 4 + s * 8 + i])
                        want += (
                            _e2m1(byte & 0x0F)
                            * scale
                            * Float64(xb[t * n_cols + column + i])
                        )
                        want += (
                            _e2m1(byte >> 4)
                            * scale
                            * Float64(xb[t * n_cols + column + 8 + i])
                        )
            want *= Float64(output_scale)
            got = Float64(yh[t * n_rows + r])
            denom = abs(want)
            if denom < 1.0:
                denom = 1.0
            rel = abs(got - want) / denom
            if rel > worst:
                worst = rel
    print("kafel", tile, "T=", n_tokens, "rows=", n_rows, "cols=", n_cols, "blad:", worst)
    # Wyjście jest f16, więc próg mieści jego zaokrąglenie; pomylony układ
    # fragmentu albo zła skala dałyby rzędy wielkości.
    if worst > 3e-3:
        raise Error("GEMM NVFP4 WMMA rozjezdza sie z referencja")


def main() raises:
    seed(20260727)
    var ctx = DeviceContext()
    for tile in range(2):
        check(ctx, 32, 64, 128, tile)
        check(ctx, 17, 70, 192, tile)
    print("GEMM NVFP4 WMMA: wszystkie kafle zgodne z referencja")
