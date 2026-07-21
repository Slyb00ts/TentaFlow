# =============================================================================
# Plik: test_decode_residual_rows.mojo
# Opis: Sprawdza pełne pokrycie wierszy NVFP4 residual dla projekcji O i down.
# Przykład: pixi run mojo run test_decode_residual_rows.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from src.decode_fused import gemv_residual_nvfp4_f16

comptime ROWS = 4096
comptime MAX_COLS = 11264
comptime SENTINEL: Float32 = 12345.0


def main() raises:
    var ctx = DeviceContext()
    var h = ctx.enqueue_create_buffer[DType.float16](ROWS)
    var h32 = ctx.enqueue_create_buffer[DType.float32](ROWS)
    var packed = ctx.enqueue_create_buffer[DType.uint8](ROWS * MAX_COLS // 2)
    var scales = ctx.enqueue_create_buffer[DType.uint8](ROWS * MAX_COLS // 16)
    var x = ctx.enqueue_create_buffer[DType.float16](MAX_COLS)

    with h.map_to_host() as hh:
        for i in range(ROWS):
            hh[i] = Float16(Float32(i % 31 - 15) * 0.03125)
    with packed.map_to_host() as hp:
        for i in range(ROWS * MAX_COLS // 2):
            hp[i] = 0
    with scales.map_to_host() as hs:
        for i in range(ROWS * MAX_COLS // 16):
            hs[i] = 0
    with x.map_to_host() as hx:
        for i in range(MAX_COLS):
            hx[i] = Float16(0.0)

    for shape in range(2):
        cols = 4096 if shape == 0 else MAX_COLS
        with h32.map_to_host() as ho:
            for i in range(ROWS):
                ho[i] = SENTINEL
        ctx.enqueue_function[gemv_residual_nvfp4_f16](
            h.unsafe_ptr(), h32.unsafe_ptr(), packed.unsafe_ptr(),
            scales.unsafe_ptr(), x.unsafe_ptr(), cols, ROWS, Float32(1.0),
            grid_dim=(ROWS + 7) // 8, block_dim=256,
        )
        ctx.synchronize()

        var mismatches = 0
        with h.map_to_host() as hh, h32.map_to_host() as ho:
            for i in range(ROWS):
                if ho[i] != Float32(hh[i]):
                    mismatches += 1
        print("cols/mismatches:", cols, mismatches)
        if mismatches != 0:
            raise Error("NVFP4 residual nie pokrywa wszystkich wierszy")
    print("PASS")
