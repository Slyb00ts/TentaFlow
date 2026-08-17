# =============================================================================
# Plik: bench_persist_grid.mojo
# Opis: Porownuje GEMV dekodowania z siatka jeden-kafel-na-blok i z siatka
#       TRWALA (grid-stride) na ZIMNYM DRAM. Sweep rozmiaru siatki pokazuje,
#       ile kosztuje niepelna ostatnia fala grup roboczych.
# Przyklad: pixi run mojo run -I . bench-amd/bench_persist_grid.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.decode_dp4a import gemv_q4_k_dp4a_f16, gemv_q4_k_dp4a_persist_f16

comptime POOL = 4 << 30
comptime ITERS = 40
comptime WARMUP = 4


def run(ctx: DeviceContext, rows: Int, cols: Int, grid_cap: Int) raises:
    nsuper = cols // 256
    span = rows * nsuper * 144
    slices = POOL // span
    if slices < 2:
        slices = 2
    var y = ctx.enqueue_create_buffer[DType.float16](rows)
    var x = ctx.enqueue_create_buffer[DType.float16](cols)
    var w = ctx.enqueue_create_buffer[DType.uint8](span * slices)
    ctx.synchronize()
    full = (rows + 7) // 8
    grid = full
    if grid_cap > 0 and grid_cap < full:
        grid = grid_cap

    if grid_cap == 0:
        for i in range(WARMUP):
            ctx.enqueue_function[gemv_q4_k_dp4a_f16](
                y.unsafe_ptr(), w.unsafe_ptr() + (i % slices) * span,
                x.unsafe_ptr(), cols, rows, grid_dim=grid, block_dim=256)
        ctx.synchronize()
        t0 = perf_counter_ns()
        for i in range(ITERS):
            ctx.enqueue_function[gemv_q4_k_dp4a_f16](
                y.unsafe_ptr(), w.unsafe_ptr() + (i % slices) * span,
                x.unsafe_ptr(), cols, rows, grid_dim=grid, block_dim=256)
        ctx.synchronize()
    else:
        for i in range(WARMUP):
            ctx.enqueue_function[gemv_q4_k_dp4a_persist_f16](
                y.unsafe_ptr(), w.unsafe_ptr() + (i % slices) * span,
                x.unsafe_ptr(), cols, rows, grid_dim=grid, block_dim=256)
        ctx.synchronize()
        t0 = perf_counter_ns()
        for i in range(ITERS):
            ctx.enqueue_function[gemv_q4_k_dp4a_persist_f16](
                y.unsafe_ptr(), w.unsafe_ptr() + (i % slices) * span,
                x.unsafe_ptr(), cols, rows, grid_dim=grid, block_dim=256)
        ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    b = Float64(span)
    label = "jeden kafel " if grid_cap == 0 else "trwala      "
    print(label, "N=", rows, "K=", cols, "grid=", grid, "->",
          Int(dt * 1e6), "us", Int(b / dt / 1e9), "GB/s")


def sweep(ctx: DeviceContext, rows: Int, cols: Int) raises:
    print("--- N=", rows, "K=", cols, "pelna siatka=", (rows + 7) // 8, "---")
    run(ctx, rows, cols, 0)
    run(ctx, rows, cols, 64)
    run(ctx, rows, cols, 128)
    run(ctx, rows, cols, 192)
    run(ctx, rows, cols, 256)
    run(ctx, rows, cols, 384)
    run(ctx, rows, cols, 512)
    run(ctx, rows, cols, 1024)


def main() raises:
    var ctx = DeviceContext()
    print("arch:", ctx.arch_name())
    sweep(ctx, 5120, 6144)
    sweep(ctx, 5120, 5120)
    sweep(ctx, 10240, 5120)
    sweep(ctx, 34816, 5120)
