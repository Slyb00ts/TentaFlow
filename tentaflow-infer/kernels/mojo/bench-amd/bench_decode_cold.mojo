# =============================================================================
# Plik: bench_decode_cold.mojo
# Opis: Pasmo GEMV dekodowania mierzone na ZIMNYM DRAM — kazda iteracja czyta
#       inny wycinek bufora 4 GiB, wiec Infinity Cache nie ma czego powtorzyc.
#       Sweep po liczbie wierszy przy stalej dlugosci wiersza rozstrzyga, czy
#       krotkie kernele traca pasmo przez staly narzut rozbiegu/ogona.
# Przyklad: pixi run mojo run -I . bench-amd/bench_decode_cold.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.decode_dp4a import gemv_q4_k_dp4a_f16, gemv_q6_k_dp4a_f16

comptime POOL = 4 << 30
comptime ITERS = 40
comptime WARMUP = 4


def sweep_q4k(ctx: DeviceContext, rows: Int, cols: Int) raises:
    """Q4_K GEMV, wagi brane z innego wycinka puli w kazdej iteracji."""
    nsuper = cols // 256
    span = rows * nsuper * 144
    slices = POOL // span
    if slices < 2:
        slices = 2
    var y = ctx.enqueue_create_buffer[DType.float16](rows)
    var x = ctx.enqueue_create_buffer[DType.float16](cols)
    var w = ctx.enqueue_create_buffer[DType.uint8](span * slices)
    ctx.synchronize()
    grid = (rows + 7) // 8
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
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    b = Float64(span)
    ideal = b / 629e9
    print("Q4_K N=", rows, "K=", cols, "blocks=", grid, "|",
          Int(b / 1e6), "MB", Int(dt * 1e6), "us", Int(b / dt / 1e9), "GB/s",
          "| narzut", Int((dt - ideal) * 1e6), "us", "| slices=", slices)


def sweep_q6k(ctx: DeviceContext, rows: Int, cols: Int) raises:
    nsuper = cols // 256
    span = rows * nsuper * 210
    slices = POOL // span
    if slices < 2:
        slices = 2
    var y = ctx.enqueue_create_buffer[DType.float16](rows)
    var x = ctx.enqueue_create_buffer[DType.float16](cols)
    var w = ctx.enqueue_create_buffer[DType.uint8](span * slices)
    ctx.synchronize()
    grid = (rows + 7) // 8
    for i in range(WARMUP):
        ctx.enqueue_function[gemv_q6_k_dp4a_f16](
            y.unsafe_ptr(), w.unsafe_ptr() + (i % slices) * span,
            x.unsafe_ptr(), cols, rows, grid_dim=grid, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for i in range(ITERS):
        ctx.enqueue_function[gemv_q6_k_dp4a_f16](
            y.unsafe_ptr(), w.unsafe_ptr() + (i % slices) * span,
            x.unsafe_ptr(), cols, rows, grid_dim=grid, block_dim=256)
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    b = Float64(span)
    ideal = b / 629e9
    print("Q6_K N=", rows, "K=", cols, "blocks=", grid, "|",
          Int(b / 1e6), "MB", Int(dt * 1e6), "us", Int(b / dt / 1e9), "GB/s",
          "| narzut", Int((dt - ideal) * 1e6), "us", "| slices=", slices)


def main() raises:
    var ctx = DeviceContext()
    print("arch:", ctx.arch_name())

    print("\n== zimny DRAM, K=5120, sweep po wierszach ==")
    sweep_q4k(ctx, 5120, 5120)
    sweep_q4k(ctx, 10240, 5120)
    sweep_q4k(ctx, 17408, 5120)
    sweep_q4k(ctx, 34816, 5120)
    sweep_q4k(ctx, 69632, 5120)
    sweep_q4k(ctx, 139264, 5120)
    sweep_q4k(ctx, 248320, 5120)

    print("\n== zimny DRAM, ksztalty modelu ==")
    sweep_q4k(ctx, 5120, 6144)
    sweep_q4k(ctx, 10240, 5120)
    sweep_q4k(ctx, 6144, 5120)
    sweep_q4k(ctx, 12288, 5120)
    sweep_q6k(ctx, 10240, 5120)
    sweep_q6k(ctx, 248320, 5120)
