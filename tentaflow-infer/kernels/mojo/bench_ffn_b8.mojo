# ===== File: bench_ffn_b8.mojo — dense FFN sweep at decode batch widths =====
# The hybrid's feed-forward block is the largest item of a batched decode step,
# and the kernel that computes it sweeps one weight row per warp for every token
# in the tile. This times that sweep on the model's own shapes and reports the
# weight bandwidth it achieves, which is the figure that says whether the sweep
# is short of memory or short of work.
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.decode_dp4a_batch import gemv_q4_k_dp4a_batch_b8
from src.decode_dp4a_batch import gemv_q4_k_dp4a_batch_r2_b8
from src.gemm import quantize_act_q8_1


def _run(ctx: DeviceContext, rows: Int, cols: Int) raises:
    comptime T = 8
    blocks_per_row = cols // 256
    wbytes = rows * blocks_per_row * 144
    var w = ctx.enqueue_create_buffer[DType.uint8](wbytes)
    var y = ctx.enqueue_create_buffer[DType.float16](T * rows)
    var x = ctx.enqueue_create_buffer[DType.float16](T * cols)
    var xq = ctx.enqueue_create_buffer[DType.int8](T * cols)
    var xd = ctx.enqueue_create_buffer[DType.float32](T * (cols // 32))
    var xs = ctx.enqueue_create_buffer[DType.float32](T * (cols // 32))
    with w.map_to_host() as h:
        for i in range(wbytes):
            h[i] = UInt8(i % 251)
    with x.map_to_host() as h:
        for i in range(T * cols):
            h[i] = Float16(Float64((i % 13) - 6) * 0.05)

    nbq = (T * (cols // 32) + 255) // 256
    ctx.enqueue_function[quantize_act_q8_1](
        xq.unsafe_ptr(), xd.unsafe_ptr(), xs.unsafe_ptr(), x.unsafe_ptr(),
        cols, T, grid_dim=nbq, block_dim=256,
    )
    ctx.synchronize()

    comptime ITERS = 100
    grid = (rows + 3) // 4
    for _ in range(20):
        ctx.enqueue_function[gemv_q4_k_dp4a_batch_b8](
            y.unsafe_ptr(), w.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
            xs.unsafe_ptr(), cols, rows, T, grid_dim=grid, block_dim=128,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q4_k_dp4a_batch_b8](
            y.unsafe_ptr(), w.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
            xs.unsafe_ptr(), cols, rows, T, grid_dim=grid, block_dim=128,
        )
    ctx.synchronize()
    us = Float64(perf_counter_ns() - t0) / Float64(ITERS) / 1000.0

    grid2 = (rows + 7) // 8
    for _ in range(20):
        ctx.enqueue_function[gemv_q4_k_dp4a_batch_r2_b8](
            y.unsafe_ptr(), w.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
            xs.unsafe_ptr(), cols, rows, T, grid_dim=grid2, block_dim=128,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q4_k_dp4a_batch_r2_b8](
            y.unsafe_ptr(), w.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
            xs.unsafe_ptr(), cols, rows, T, grid_dim=grid2, block_dim=128,
        )
    ctx.synchronize()
    us2 = Float64(perf_counter_ns() - t1) / Float64(ITERS) / 1000.0
    print(
        "rows", rows, "cols", cols,
        "| us", Int(us),
        "| w GB/s", Int(Float64(wbytes) / us / 1e3),
        "| akt GB/s", Int(Float64(grid) * Float64(T * cols) / us / 1e3),
        "|| r2 us", Int(us2),
        "w GB/s", Int(Float64(wbytes) / us2 / 1e3),
    )


def main() raises:
    with DeviceContext() as ctx:
        _run(ctx, 17408, 5120)
        _run(ctx, 5120, 17408)
