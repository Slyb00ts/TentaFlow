# =============================================================================
# Plik: bench_nvfp4_decode.mojo
# Opis: Mierzy ścieżkę decode NVFP4 na realnych kształtach warstw Bielika i
#       porównuje wariant z LUT w LDS z arytmetycznym rozpakowaniem e2m1.
# Przykład: pixi run mojo run -I . bench-amd/bench_nvfp4_decode.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.gemv2 import gemv_nvfp4_f16_v2

comptime ITERS = 50


def bench(ctx: DeviceContext, label: String, rows: Int, cols: Int) raises:
    groups = cols // 16
    var packed = ctx.enqueue_create_buffer[DType.uint8](rows * (cols // 2))
    var scales = ctx.enqueue_create_buffer[DType.uint8](rows * groups)
    var x = ctx.enqueue_create_buffer[DType.float16](cols)
    var y = ctx.enqueue_create_buffer[DType.float16](rows)
    ctx.synchronize()
    grid = (rows + 7) // 8
    for _ in range(5):
        ctx.enqueue_function[gemv_nvfp4_f16_v2](
            y.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(),
            x.unsafe_ptr(), cols, rows, Float32(1.0),
            grid_dim=(grid,), block_dim=256,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_nvfp4_f16_v2](
            y.unsafe_ptr(), packed.unsafe_ptr(), scales.unsafe_ptr(),
            x.unsafe_ptr(), cols, rows, Float32(1.0),
            grid_dim=(grid,), block_dim=256,
        )
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    # Wagi to pol bajtu na wartosc plus jeden bajt skali na 16 wartosci.
    bytes_read = Float64(rows) * (Float64(cols) * 0.5 + Float64(groups))
    print(
        label, "rows=", rows, "cols=", cols,
        Int(bytes_read / dt / 1e9), "GB/s", Int(dt * 1e6), "us",
    )


def main() raises:
    var ctx = DeviceContext()
    print("arch:", ctx.arch_name())
    # Warstwy Bielika-7B: qkv, o, gate+up, down.
    bench(ctx, "qkv    ", 5120, 4096)
    bench(ctx, "o      ", 4096, 4096)
    bench(ctx, "gate+up", 22528, 4096)
    bench(ctx, "down   ", 4096, 11264)
    # Kształt POZA Infinity Cache — dopiero to jest realny decode, bo w modelu
    # 4 GB wag przelatuje przez pamięć raz na token i nic się nie cache'uje.
    print("--- poza Infinity Cache (realny streaming) ---")
    bench(ctx, "stream ", 180224, 4096)
