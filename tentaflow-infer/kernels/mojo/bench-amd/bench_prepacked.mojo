# Porownuje prefill Q4_K liczony W LOCIE (kafel f16 WMMA) z wariantem, ktory
# czyta wagi PRZEPAKOWANE przy ladowaniu. Rozstrzyga, czy przepakowanie sie oplaca.
from std.gpu.host import DeviceContext
from std.random import random_si64, seed
from std.time import perf_counter_ns

from src.gemm_q4_k_wmma import gemm_q4_k_wmma_impl
from src.gemm_prepacked_i8wmma import gemm_prepacked_i8wmma_f16_bm256

comptime WARMUP = 2
comptime ITERS = 6


def shape(ctx: DeviceContext, n_rows: Int, n_cols: Int, n_tokens: Int) raises:
    sbs = n_cols // 256
    nb = n_cols // 32
    var w = ctx.enqueue_create_buffer[DType.uint8](n_rows * sbs * 144)
    var x = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_cols)
    var y = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_rows)
    var wi8 = ctx.enqueue_create_buffer[DType.int8](n_rows * n_cols)
    var wds = ctx.enqueue_create_buffer[DType.float32](n_rows * nb)
    var wdm = ctx.enqueue_create_buffer[DType.float32](n_rows * nb)
    var xq = ctx.enqueue_create_buffer[DType.int8](n_tokens * n_cols)
    var xd = ctx.enqueue_create_buffer[DType.float32](nb * n_tokens)
    var xs = ctx.enqueue_create_buffer[DType.float32](nb * n_tokens)
    ctx.synchronize()
    flops = 2.0 * Float64(n_tokens) * Float64(n_rows) * Float64(n_cols)
    print("rows=", n_rows, "cols=", n_cols, "T=", n_tokens)

    comptime BM = 256
    comptime BN = 64
    var t0: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t0 = perf_counter_ns()
        ctx.enqueue_function[gemm_q4_k_wmma_impl[4, 2, 4, 2]](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(), n_cols, n_rows, n_tokens,
            grid_dim=((n_rows + BN - 1) // BN, (n_tokens + BM - 1) // BM), block_dim=256,
        )
    ctx.synchronize()
    s_live = Float64(perf_counter_ns() - t0) / 1e9 / Float64(ITERS)
    print("    w locie (f16 WMMA)   ", Int(s_live * 1e6), "us =", Int(flops / s_live / 1e12), "TFLOPS")

    var t1: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t1 = perf_counter_ns()
        ctx.enqueue_function[gemm_prepacked_i8wmma_f16_bm256](
            y.unsafe_ptr(), wi8.unsafe_ptr(), wds.unsafe_ptr(), wdm.unsafe_ptr(),
            xq.unsafe_ptr(), xd.unsafe_ptr(), xs.unsafe_ptr(),
            n_cols, n_rows, n_tokens,
            grid_dim=((n_rows + BN - 1) // BN, (n_tokens + BM - 1) // BM), block_dim=256,
        )
    ctx.synchronize()
    s_pre = Float64(perf_counter_ns() - t1) / 1e9 / Float64(ITERS)
    print("    przepakowane (int8)  ", Int(s_pre * 1e6), "us =", Int(flops / s_pre / 1e12), "TFLOPS",
          "| przyspieszenie", s_live / s_pre)


def main() raises:
    seed(20260729)
    var ctx = DeviceContext()
    shape(ctx, 11264, 4096, 512)
    shape(ctx, 4096, 11264, 512)
    shape(ctx, 4096, 4096, 512)
