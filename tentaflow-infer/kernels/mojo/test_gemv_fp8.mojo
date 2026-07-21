# =============================================================================
# Plik: test_gemv_fp8.mojo
# Opis: Sprawdza GEMV E4M3 do FP32 względem referencji CPU i głowicy F16.
# Przykład: pixi run mojo run test_gemv_fp8.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.memory import bitcast
from src.gemv2 import gemv_f16_out_f32_v2, gemv_fp8_out_f32_v2

comptime ROWS = 19
comptime COLS = 256
comptime TOL: Float32 = 0.0005


def _fill(i: Int) -> Float32:
    seed = (UInt32(i) * 1664525 + 1013904223) & 0xFFFFFFFF
    return Float32(seed) * (2.0 / 4294967296.0) - 1.0


def main() raises:
    var ctx = DeviceContext()
    var yfp8 = ctx.enqueue_create_buffer[DType.float32](ROWS)
    var yf16 = ctx.enqueue_create_buffer[DType.float32](ROWS)
    var yref = ctx.enqueue_create_buffer[DType.float32](ROWS)
    var x = ctx.enqueue_create_buffer[DType.float16](COLS)
    var wf16 = ctx.enqueue_create_buffer[DType.float16](ROWS * COLS)
    var wfp8 = ctx.enqueue_create_buffer[DType.uint8](ROWS * COLS)
    var scales = ctx.enqueue_create_buffer[DType.float32](ROWS)

    with x.map_to_host() as hx:
        for col in range(COLS):
            hx[col] = Float16(_fill(col * 7 + 3))

    with wf16.map_to_host() as hf, wfp8.map_to_host() as hq, scales.map_to_host() as hs, x.map_to_host() as hx, yref.map_to_host() as hr:
        for row in range(ROWS):
            scale = Float32(0.25 if row % 3 == 0 else (0.5 if row % 3 == 1 else 1.0))
            hs[row] = scale
            var acc: Float32 = 0.0
            for col in range(COLS):
                encoded = Scalar[DType.float8_e4m3fn](_fill(row * COLS + col * 11 + 17) * 4.0)
                hq[row * COLS + col] = bitcast[DType.uint8, 1](encoded)[0]
                value = Float32(encoded) * scale
                hf[row * COLS + col] = Float16(value)
                acc += Float32(encoded) * Float32(hx[col])
            hr[row] = acc * scale

    ctx.enqueue_function[gemv_fp8_out_f32_v2](
        yfp8.unsafe_ptr(), wfp8.unsafe_ptr(), scales.unsafe_ptr(),
        x.unsafe_ptr(), COLS, ROWS,
        grid_dim=(ROWS + 7) // 8, block_dim=256,
    )
    ctx.enqueue_function[gemv_f16_out_f32_v2](
        yf16.unsafe_ptr(), wf16.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS,
        grid_dim=(ROWS + 7) // 8, block_dim=256,
    )
    ctx.synchronize()

    var max_error: Float32 = 0.0
    var argmax_fp8 = 0
    var argmax_f16 = 0
    with yfp8.map_to_host() as h8, yf16.map_to_host() as h16, yref.map_to_host() as hr:
        for row in range(ROWS):
            error = abs(h8[row] - hr[row])
            if error > max_error:
                max_error = error
            if h8[row] > h8[argmax_fp8]:
                argmax_fp8 = row
            if h16[row] > h16[argmax_f16]:
                argmax_f16 = row

    if max_error > TOL:
        raise Error("GEMV FP8 nie zgadza się z referencją CPU")
    if argmax_fp8 != argmax_f16:
        raise Error("GEMV FP8 zmienił argmax równoważnej głowicy F16")
    print("PASS max_error=", max_error, "argmax=", argmax_fp8)
