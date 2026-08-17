# =============================================================================
# Plik: bench_gemma_attn.mojo
# Opis: Mierzy uwage prefillu na kształtach Gemmy 4 (hd256 okienna, hd512
#       globalna) — to 14% czasu prefillu na RDNA2.
# Przykład: pixi run mojo run -I . bench-amd/bench_gemma_attn.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.prefill import attn_prefill

comptime ITERS = 30
comptime WARP_SIZE = 32


def bench[head_dim: Int, PT: Int](
    label: String, ctx: DeviceContext, tokens: Int, n_q: Int, n_kv: Int, window: Int
) raises:
    comptime page_size = 32
    pages = (tokens + page_size - 1) // page_size
    ctxlen = pages * page_size
    var qd = ctx.enqueue_create_buffer[DType.float16](tokens * n_q * head_dim)
    var kd = ctx.enqueue_create_buffer[DType.float16](ctxlen * n_kv * head_dim)
    var vd = ctx.enqueue_create_buffer[DType.float16](ctxlen * n_kv * head_dim)
    var od = ctx.enqueue_create_buffer[DType.float16](tokens * n_q * head_dim)
    var pt = ctx.enqueue_create_buffer[DType.int32](pages)
    var pth = ctx.enqueue_create_host_buffer[DType.int32](pages)
    for i in range(pages):
        pth[i] = Int32(i)
    ctx.enqueue_copy(pt, pth)
    ctx.synchronize()
    grid = ((tokens + 15) // 16, n_q)
    for _ in range(3):
        ctx.enqueue_function[attn_prefill[head_dim, DType.float16, PT]](
            od.unsafe_ptr(), qd.unsafe_ptr(), kd.unsafe_ptr(), vd.unsafe_ptr(),
            pt.unsafe_ptr(), 0, n_q, n_kv, page_size, Float32(1.0), tokens, window,
            grid_dim=grid, block_dim=256,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[attn_prefill[head_dim, DType.float16, PT]](
            od.unsafe_ptr(), qd.unsafe_ptr(), kd.unsafe_ptr(), vd.unsafe_ptr(),
            pt.unsafe_ptr(), 0, n_q, n_kv, page_size, Float32(1.0), tokens, window,
            grid_dim=grid, block_dim=256,
        )
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    # przyczynowe: ~T^2/2 par, po 2 iloczyny (QK i PV) na wymiar glowicy
    flops = 2.0 * 2.0 * Float64(tokens) * Float64(tokens) / 2.0 * Float64(head_dim) * Float64(n_q)
    print(label, "T=", tokens, "->", Int(dt * 1e6), "us", Int(flops / dt / 1e11), "/10 TFLOPS")


def main() raises:
    var ctx = DeviceContext()
    print("arch:", ctx.arch_name())
    bench[256, WARP_SIZE]("hd256 swa   ", ctx, 1024, 16, 8, 1024)
    bench[512, WARP_SIZE // 2]("hd512 global", ctx, 1024, 16, 1, 0)
