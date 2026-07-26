# =============================================================================
# Plik: bench_gemma_gemv.mojo
# Opis: Mierzy pasmo GEMV Q4_0 na kształtach dekodowania Gemmy 4 12B — decode
#       jest ograniczony czytaniem wag, więc to jego jedyny licznik.
# Przykład: pixi run mojo run -I . bench-amd/bench_gemma_gemv.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.gemv2 import gemv_q4_0_f16_v2

comptime ITERS = 60


def bench(label: String, ctx: DeviceContext, rows: Int, cols: Int) raises:
    nb = cols // 32
    var y = ctx.enqueue_create_buffer[DType.float16](rows)
    var x = ctx.enqueue_create_buffer[DType.float16](cols)
    var w = ctx.enqueue_create_buffer[DType.uint8](rows * nb * 18)
    ctx.synchronize()
    grid = (rows + 7) // 8
    for _ in range(10):
        ctx.enqueue_function[gemv_q4_0_f16_v2](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q4_0_f16_v2](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256,
        )
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    bytes = Float64(rows) * Float64(nb) * 18.0
    print(label, "N=", rows, "K=", cols, "->", Int(dt * 1e6), "us",
          Int(bytes / dt / 1e9), "GB/s")


def main() raises:
    var ctx = DeviceContext()
    print("arch:", ctx.arch_name())
    bench("ffn gate/up ", ctx, 15360, 3840)
    bench("ffn down    ", ctx, 3840, 15360)
    bench("attn q swa  ", ctx, 4096, 3840)
    bench("attn q glob ", ctx, 8192, 3840)
    bench("attn kv swa ", ctx, 2048, 3840)
    bench("attn o swa  ", ctx, 3840, 4096)
