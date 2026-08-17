# =============================================================================
# Plik: bench_q4k_wmma_tiles.mojo
# Opis: Sweep kafli prefillowego GEMM-u Q4_K na WMMA, na ksztaltach projekcji
#       Bielika 7B. Dobor kafla ma wynikac z pomiaru, a nie z rachunku.
# Przyklad: pixi run mojo run -I . bench-amd/bench_q4k_wmma_tiles.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.random import random_si64, seed
from std.time import perf_counter_ns

from src.gemm_q4_k_wmma import gemm_q4_k_wmma_impl

comptime WARMUP = 2
comptime ITERS = 6
comptime SB_BYTES = 144


def one[WM: Int, WN: Int, MT: Int, NT: Int, PAD: Int = 0](
    ctx: DeviceContext, label: String, y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin], x: UnsafePointer[Float16, MutAnyOrigin],
    n_rows: Int, n_cols: Int, n_tokens: Int, flops: Float64,
) raises:
    comptime BM = WM * MT * 16
    comptime BN = WN * NT * 16
    comptime THREADS = WM * WN * 32
    var t0: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t0 = perf_counter_ns()
        ctx.enqueue_function[gemm_q4_k_wmma_impl[WM, WN, MT, NT, PAD]](
            y, w, x, n_cols, n_rows, n_tokens,
            grid_dim=((n_rows + BN - 1) // BN, (n_tokens + BM - 1) // BM),
            block_dim=THREADS,
        )
    ctx.synchronize()
    s = Float64(perf_counter_ns() - t0) / 1e9 / Float64(ITERS)
    print("   ", label, "BM", BM, "BN", BN, "|", Int(s * 1e6), "us =",
          Int(flops / s / 1e12), "TFLOPS")


def shape(ctx: DeviceContext, n_rows: Int, n_cols: Int, n_tokens: Int) raises:
    sbs = n_cols // 256
    var wb = ctx.enqueue_create_host_buffer[DType.uint8](n_rows * sbs * SB_BYTES)
    var xb = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_cols)
    ctx.synchronize()
    for i in range(n_rows * sbs * SB_BYTES):
        wb[i] = UInt8(Int(random_si64(0, 255)))
    for i in range(n_tokens * n_cols):
        xb[i] = Float16(Float64(Int(random_si64(-4, 4))) * 0.25)
    var w = ctx.enqueue_create_buffer[DType.uint8](n_rows * sbs * SB_BYTES)
    var x = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_cols)
    var y = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(w, wb)
    ctx.enqueue_copy(x, xb)
    ctx.synchronize()
    flops = 2.0 * Float64(n_tokens) * Float64(n_rows) * Float64(n_cols)
    yp = rebind[UnsafePointer[Float16, MutAnyOrigin]](y.unsafe_ptr())
    wp = rebind[UnsafePointer[UInt8, MutAnyOrigin]](w.unsafe_ptr())
    xp = rebind[UnsafePointer[Float16, MutAnyOrigin]](x.unsafe_ptr())
    print("rows=", n_rows, "cols=", n_cols, "T=", n_tokens)
    one[4, 2, 4, 2, 16](ctx, String("BM256/BN64   4x2 "), yp, wp, xp, n_rows, n_cols, n_tokens, flops)
    one[4, 2, 4, 4, 16](ctx, String("BM256/BN128  4x2 "), yp, wp, xp, n_rows, n_cols, n_tokens, flops)
    one[4, 2, 4, 4, 8](ctx, String("BM256/BN128  pad8"), yp, wp, xp, n_rows, n_cols, n_tokens, flops)
    one[4, 2, 4, 4, 0](ctx, String("BM256/BN128  pad0"), yp, wp, xp, n_rows, n_cols, n_tokens, flops)
    one[8, 2, 4, 4, 16](ctx, String("BM512/BN128  8x2 "), yp, wp, xp, n_rows, n_cols, n_tokens, flops)
    one[4, 2, 2, 4, 16](ctx, String("BM128/BN128  4x2 "), yp, wp, xp, n_rows, n_cols, n_tokens, flops)
    one[4, 2, 8, 4, 16](ctx, String("BM512/BN128  4x2 "), yp, wp, xp, n_rows, n_cols, n_tokens, flops)


def main() raises:
    seed(20260729)
    var ctx = DeviceContext()
    # ksztalty Q4_K ThinkingCap-Qwen3.6-27B
    shape(ctx, 17408, 5120, 1024)
    shape(ctx, 6144, 5120, 1024)
    shape(ctx, 5120, 6144, 1024)
