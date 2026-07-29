# Flash attention prefillu na WMMA wobec obecnego kernela `attn_prefill`.
# Ksztalty jak w prefillu Bielika 7B: head_dim 128, 32 glowice Q, 8 glowic KV.
from std.gpu.host import DeviceContext
from std.math import sqrt
from std.time import perf_counter_ns
from src.prefill import attn_prefill_f16_hd128
from src.prefill_wmma import attn_prefill_wmma_hd128

comptime HD: Int = 128
comptime WARMUP: Int = 3
comptime ITERS: Int = 10


def run(ctx: DeviceContext, n_tokens: Int, n_q: Int, n_kv: Int) raises:
    page_size = 256
    n_pages = (n_tokens + page_size - 1) // page_size
    kv_elems = n_pages * n_kv * page_size * HD

    var ph_ = ctx.enqueue_create_host_buffer[DType.int32](n_pages)
    ctx.synchronize()
    for i in range(n_pages):
        ph_[i] = Int32(i)

    var qd = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_q * HD)
    var kd = ctx.enqueue_create_buffer[DType.float16](kv_elems)
    var vd = ctx.enqueue_create_buffer[DType.float16](kv_elems)
    var od = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_q * HD)
    var pd = ctx.enqueue_create_buffer[DType.int32](n_pages)
    ctx.enqueue_copy(pd, ph_)
    ctx.synchronize()

    scale = 1.0 / sqrt(Float32(HD))

    var t0: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t0 = perf_counter_ns()
        ctx.enqueue_function[attn_prefill_f16_hd128](
            od.unsafe_ptr(), qd.unsafe_ptr(), kd.unsafe_ptr(), vd.unsafe_ptr(),
            pd.unsafe_ptr(), 0, n_q, n_kv, page_size, scale, n_tokens, 0,
            grid_dim=((n_tokens + 15) // 16, n_q), block_dim=256,
        )
    ctx.synchronize()
    old_us = Float64(perf_counter_ns() - t0) / 1e3 / Float64(ITERS)

    var t1: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t1 = perf_counter_ns()
        ctx.enqueue_function[attn_prefill_wmma_hd128](
            od.unsafe_ptr(), qd.unsafe_ptr(), kd.unsafe_ptr(), vd.unsafe_ptr(),
            pd.unsafe_ptr(), 0, n_q, n_kv, page_size, scale, n_tokens,
            grid_dim=((n_tokens + 63) // 64, n_q), block_dim=128,
        )
    ctx.synchronize()
    new_us = Float64(perf_counter_ns() - t1) / 1e3 / Float64(ITERS)

    print(
        "T=", n_tokens, "Hq=", n_q, "Hkv=", n_kv,
        "| obecny", Int(old_us), "us | wmma", Int(new_us),
        "us | przyspieszenie", Int(100.0 * old_us / new_us), "%",
    )


def main() raises:
    var ctx = DeviceContext()
    run(ctx, 512, 32, 8)
    run(ctx, 1024, 32, 8)
    run(ctx, 2048, 32, 8)
