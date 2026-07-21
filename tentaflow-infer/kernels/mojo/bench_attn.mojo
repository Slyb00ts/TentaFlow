# ===== File: bench_attn.mojo — attn_prefill micro-bench at Qwen3-0.6B shapes =====

from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.prefill import attn_prefill_f16_hd128, QT


def main() raises:
    var ctx = DeviceContext()
    comptime NQH = 16
    comptime NKVH = 8
    comptime HD = 128
    comptime PAGE = 32
    comptime T = 256
    comptime BASE = 768
    comptime NPAGES = (BASE + T) // PAGE
    comptime SCALE: Float32 = 0.0883883

    var kc = ctx.enqueue_create_buffer[DType.float16](NPAGES * NKVH * PAGE * HD)
    var vc = ctx.enqueue_create_buffer[DType.float16](NPAGES * NKVH * PAGE * HD)
    var pt = ctx.enqueue_create_buffer[DType.int32](NPAGES)
    var qb = ctx.enqueue_create_buffer[DType.float16](T * NQH * HD)
    var ob = ctx.enqueue_create_buffer[DType.float16](T * NQH * HD)
    with pt.map_to_host() as h:
        for i in range(NPAGES):
            h[i] = Int32(i)
    with qb.map_to_host() as h:
        for i in range(T * NQH * HD):
            h[i] = Float16(Float32((i * 37 % 19) - 9) * 0.01)
    with kc.map_to_host() as kh, vc.map_to_host() as vh:
        for i in range(NPAGES * NKVH * PAGE * HD):
            kh[i] = Float16(Float32((i * 13 % 23) - 11) * 0.01)
            vh[i] = Float16(Float32((i * 7 % 17) - 8) * 0.01)

    for _ in range(300):
        ctx.enqueue_function[attn_prefill_f16_hd128](
            ob.unsafe_ptr(), qb.unsafe_ptr(), kc.unsafe_ptr(), vc.unsafe_ptr(),
            pt.unsafe_ptr(), BASE, NQH, NKVH, PAGE, SCALE, T,
            grid_dim=((T + QT - 1) // QT, NQH), block_dim=256,
        )
    ctx.synchronize()

    comptime ITERS = 50
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[attn_prefill_f16_hd128](
            ob.unsafe_ptr(), qb.unsafe_ptr(), kc.unsafe_ptr(), vc.unsafe_ptr(),
            pt.unsafe_ptr(), BASE, NQH, NKVH, PAGE, SCALE, T,
            grid_dim=((T + QT - 1) // QT, NQH), block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    # (q, pos) pairs across all heads for causal T=256 @ base 768
    pairs = Float64(NQH) * Float64(T) * (Float64(BASE) + Float64(T + 1) / 2.0)
    print("attn_prefill hd128 T=256 base=768:", ms, "ms  ",
          pairs / (ms / 1e3) / 1e9, "Gpairs/s")
