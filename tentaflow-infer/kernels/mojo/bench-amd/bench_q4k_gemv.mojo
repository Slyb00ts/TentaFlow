# =============================================================================
# Plik: bench_q4k_gemv.mojo
# Opis: Porownuje warianty GEMV Q4_K na ksztaltach dekodowania Mistrala 7B —
#       profiler pokazal tam 185 GB/s, gdy Q4_0 wyciaga 466 GB/s.
# Przyklad: pixi run mojo run -I . bench-amd/bench_q4k_gemv.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.gemv2 import gemv_q4_k_f16_v2
from src.decode_dp4a import gemv_q4_k_dp4a_f16

comptime ITERS = 50


def bench_v2(label: String, ctx: DeviceContext, rows: Int, cols: Int) raises:
    nsuper = cols // 256
    var y = ctx.enqueue_create_buffer[DType.float16](rows)
    var x = ctx.enqueue_create_buffer[DType.float16](cols)
    var w = ctx.enqueue_create_buffer[DType.uint8](rows * nsuper * 144)
    ctx.synchronize()
    grid = (rows + 7) // 8
    for _ in range(10):
        ctx.enqueue_function[gemv_q4_k_f16_v2](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q4_k_f16_v2](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256,
        )
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    b = Float64(rows) * Float64(nsuper) * 144.0
    print(label, "N=", rows, "K=", cols, "->", Int(dt * 1e6), "us", Int(b / dt / 1e9), "GB/s")


def bench_dp4a(label: String, ctx: DeviceContext, rows: Int, cols: Int) raises:
    nsuper = cols // 256
    var y = ctx.enqueue_create_buffer[DType.float16](rows)
    var x = ctx.enqueue_create_buffer[DType.float16](cols)
    var w = ctx.enqueue_create_buffer[DType.uint8](rows * nsuper * 144)
    ctx.synchronize()
    grid = (rows + 7) // 8
    for _ in range(10):
        ctx.enqueue_function[gemv_q4_k_dp4a_f16](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q4_k_dp4a_f16](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256,
        )
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    b = Float64(rows) * Float64(nsuper) * 144.0
    print(label, "N=", rows, "K=", cols, "->", Int(dt * 1e6), "us", Int(b / dt / 1e9), "GB/s")


def main() raises:
    var ctx = DeviceContext()
    print("arch:", ctx.arch_name())
    bench_v2("v2   ffn  ", ctx, 14336, 4096)
    bench_dp4a("dp4a ffn  ", ctx, 14336, 4096)
    bench_v2("v2   attn ", ctx, 4096, 4096)
    bench_dp4a("dp4a attn ", ctx, 4096, 4096)
    bench_v2("v2   down ", ctx, 4096, 14336)
    bench_dp4a("dp4a down ", ctx, 4096, 14336)
