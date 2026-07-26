# =============================================================================
# Plik: bench_q6k_gemv.mojo
# Opis: Pasmo GEMV Q6_K na ksztaltach z Mistrala Q4_K_M (tam czesc tensorow jest
#       w Q6_K) — profiler pokazal 66,67 us na wywolanie, trzeba wiedziec ile to
#       GB/s.
# Przyklad: pixi run mojo run -I . bench-amd/bench_q6k_gemv.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.gemv2 import gemv_q6_k_f16_v2

comptime ITERS = 50


def bench(label: String, ctx: DeviceContext, rows: Int, cols: Int) raises:
    nsuper = cols // 256
    var y = ctx.enqueue_create_buffer[DType.float16](rows)
    var x = ctx.enqueue_create_buffer[DType.float16](cols)
    var w = ctx.enqueue_create_buffer[DType.uint8](rows * nsuper * 210)
    ctx.synchronize()
    grid = (rows + 7) // 8
    for _ in range(10):
        ctx.enqueue_function[gemv_q6_k_f16_v2](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q6_k_f16_v2](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256)
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    b = Float64(rows) * Float64(nsuper) * 210.0
    print(label, "N=", rows, "K=", cols, "->", Int(dt * 1e6), "us",
          Int(b / 1024.0 / 1024.0), "MB", Int(b / dt / 1e9), "GB/s")


def main() raises:
    var ctx = DeviceContext()
    print("arch:", ctx.arch_name())
    bench("attn_v ", ctx, 1024, 4096)
    bench("ffn_down", ctx, 4096, 14336)
