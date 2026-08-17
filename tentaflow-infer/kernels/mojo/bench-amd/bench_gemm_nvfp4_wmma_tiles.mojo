# =============================================================================
# Plik: bench_gemm_nvfp4_wmma_tiles.mojo
# Opis: Sweep kafli prefillowego GEMM-u NVFP4 na WMMA — na kształtach projekcji
#       Qwen3.6-27B. Mierzy TFLOPS, żeby dobór kafla wynikał z pomiaru, a nie
#       z rachunku na kartce.
# Przykład: pixi run mojo run -I . bench-amd/bench_gemm_nvfp4_wmma_tiles.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.random import random_si64, seed
from std.time import perf_counter_ns

from src.nvfp4_gguf_wmma import gemm_nvfp4_gguf_wmma_impl

comptime WARMUP = 2
comptime ITERS = 8
comptime BLOCK_BYTES = 36


def shape(ctx: DeviceContext, n_rows: Int, n_cols: Int, n_tokens: Int) raises:
    blocks = n_cols // 64
    var wb = ctx.enqueue_create_host_buffer[DType.uint8](n_rows * blocks * BLOCK_BYTES)
    var xb = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_cols)
    ctx.synchronize()
    for i in range(n_rows * blocks * BLOCK_BYTES):
        wb[i] = UInt8(Int(random_si64(0, 255)))
    for i in range(n_tokens * n_cols):
        xb[i] = Float16(Float64(Int(random_si64(-4, 4))) * 0.25)
    var w = ctx.enqueue_create_buffer[DType.uint8](n_rows * blocks * BLOCK_BYTES)
    var x = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_cols)
    var y = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(w, wb)
    ctx.enqueue_copy(x, xb)
    ctx.synchronize()
    yp = y.unsafe_ptr().bitcast[Float16]()
    wp = w.unsafe_ptr().bitcast[UInt8]()
    xp = x.unsafe_ptr().bitcast[Float16]()
    flops = 2.0 * Float64(n_tokens) * Float64(n_rows) * Float64(n_cols)
    print("rows=", n_rows, "cols=", n_cols, "T=", n_tokens)

    comptime BM_obecny = 128
    comptime BN_obecny = 64
    var t_obecny: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t_obecny = perf_counter_ns()
        ctx.enqueue_function[gemm_nvfp4_gguf_wmma_impl[2, 2, 4, 2]](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(),
            n_cols, n_rows, n_tokens, Float32(1.0),
            grid_dim=(
                (n_rows + BN_obecny - 1) // BN_obecny,
                (n_tokens + BM_obecny - 1) // BM_obecny,
            ),
            block_dim=128,
        )
    ctx.synchronize()
    s_obecny = Float64(perf_counter_ns() - t_obecny) / 1e9 / Float64(ITERS)
    print("    obecny     BM", BM_obecny, "BN", BN_obecny,
          "|", Int(s_obecny * 1e6), "us =",
          Int(flops / s_obecny / 1e12), "TFLOPS")

    comptime BM_M4N4 = 128
    comptime BN_M4N4 = 128
    var t_M4N4: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t_M4N4 = perf_counter_ns()
        ctx.enqueue_function[gemm_nvfp4_gguf_wmma_impl[2, 2, 4, 4]](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(),
            n_cols, n_rows, n_tokens, Float32(1.0),
            grid_dim=(
                (n_rows + BN_M4N4 - 1) // BN_M4N4,
                (n_tokens + BM_M4N4 - 1) // BM_M4N4,
            ),
            block_dim=128,
        )
    ctx.synchronize()
    s_M4N4 = Float64(perf_counter_ns() - t_M4N4) / 1e9 / Float64(ITERS)
    print("    M4N4       BM", BM_M4N4, "BN", BN_M4N4,
          "|", Int(s_M4N4 * 1e6), "us =",
          Int(flops / s_M4N4 / 1e12), "TFLOPS")

    comptime BM_M2N4 = 64
    comptime BN_M2N4 = 128
    var t_M2N4: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t_M2N4 = perf_counter_ns()
        ctx.enqueue_function[gemm_nvfp4_gguf_wmma_impl[2, 2, 2, 4]](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(),
            n_cols, n_rows, n_tokens, Float32(1.0),
            grid_dim=(
                (n_rows + BN_M2N4 - 1) // BN_M2N4,
                (n_tokens + BM_M2N4 - 1) // BM_M2N4,
            ),
            block_dim=128,
        )
    ctx.synchronize()
    s_M2N4 = Float64(perf_counter_ns() - t_M2N4) / 1e9 / Float64(ITERS)
    print("    M2N4       BM", BM_M2N4, "BN", BN_M2N4,
          "|", Int(s_M2N4 * 1e6), "us =",
          Int(flops / s_M2N4 / 1e12), "TFLOPS")

    comptime BM_M8N2 = 256
    comptime BN_M8N2 = 64
    var t_M8N2: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t_M8N2 = perf_counter_ns()
        ctx.enqueue_function[gemm_nvfp4_gguf_wmma_impl[2, 2, 8, 2]](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(),
            n_cols, n_rows, n_tokens, Float32(1.0),
            grid_dim=(
                (n_rows + BN_M8N2 - 1) // BN_M8N2,
                (n_tokens + BM_M8N2 - 1) // BM_M8N2,
            ),
            block_dim=128,
        )
    ctx.synchronize()
    s_M8N2 = Float64(perf_counter_ns() - t_M8N2) / 1e9 / Float64(ITERS)
    print("    M8N2       BM", BM_M8N2, "BN", BN_M8N2,
          "|", Int(s_M8N2 * 1e6), "us =",
          Int(flops / s_M8N2 / 1e12), "TFLOPS")

    comptime BM_8fal_M4N2 = 256
    comptime BN_8fal_M4N2 = 64
    var t_8fal_M4N2: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t_8fal_M4N2 = perf_counter_ns()
        ctx.enqueue_function[gemm_nvfp4_gguf_wmma_impl[4, 2, 4, 2]](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(),
            n_cols, n_rows, n_tokens, Float32(1.0),
            grid_dim=(
                (n_rows + BN_8fal_M4N2 - 1) // BN_8fal_M4N2,
                (n_tokens + BM_8fal_M4N2 - 1) // BM_8fal_M4N2,
            ),
            block_dim=256,
        )
    ctx.synchronize()
    s_8fal_M4N2 = Float64(perf_counter_ns() - t_8fal_M4N2) / 1e9 / Float64(ITERS)
    print("    8fal-M4N2  BM", BM_8fal_M4N2, "BN", BN_8fal_M4N2,
          "|", Int(s_8fal_M4N2 * 1e6), "us =",
          Int(flops / s_8fal_M4N2 / 1e12), "TFLOPS")

    comptime BM_8fal_N4 = 128
    comptime BN_8fal_N4 = 128
    var t_8fal_N4: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t_8fal_N4 = perf_counter_ns()
        ctx.enqueue_function[gemm_nvfp4_gguf_wmma_impl[2, 4, 4, 2]](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(),
            n_cols, n_rows, n_tokens, Float32(1.0),
            grid_dim=(
                (n_rows + BN_8fal_N4 - 1) // BN_8fal_N4,
                (n_tokens + BM_8fal_N4 - 1) // BM_8fal_N4,
            ),
            block_dim=256,
        )
    ctx.synchronize()
    s_8fal_N4 = Float64(perf_counter_ns() - t_8fal_N4) / 1e9 / Float64(ITERS)
    print("    8fal-N4    BM", BM_8fal_N4, "BN", BN_8fal_N4,
          "|", Int(s_8fal_N4 * 1e6), "us =",
          Int(flops / s_8fal_N4 / 1e12), "TFLOPS")

    # BM=512: calkowity koszt dekwantyzacji skaluje sie jak 1/BM, bo wagi
    # rozpakowuja sie RAZ NA BLOK do LDS. Dwa ulozenia fal o tym samym BM.
    comptime BM_M8 = 512
    comptime BN_M8 = 64
    var t_M8: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t_M8 = perf_counter_ns()
        ctx.enqueue_function[gemm_nvfp4_gguf_wmma_impl[4, 2, 8, 2]](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(),
            n_cols, n_rows, n_tokens, Float32(1.0),
            grid_dim=(
                (n_rows + BN_M8 - 1) // BN_M8,
                (n_tokens + BM_M8 - 1) // BM_M8,
            ),
            block_dim=256,
        )
    ctx.synchronize()
    s_M8 = Float64(perf_counter_ns() - t_M8) / 1e9 / Float64(ITERS)
    print("    8fal-M8N2  BM", BM_M8, "BN", BN_M8,
          "|", Int(s_M8 * 1e6), "us =",
          Int(flops / s_M8 / 1e12), "TFLOPS")

    comptime BM_W8 = 512
    comptime BN_W8 = 64
    var t_W8: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t_W8 = perf_counter_ns()
        ctx.enqueue_function[gemm_nvfp4_gguf_wmma_impl[8, 2, 4, 2]](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(),
            n_cols, n_rows, n_tokens, Float32(1.0),
            grid_dim=(
                (n_rows + BN_W8 - 1) // BN_W8,
                (n_tokens + BM_W8 - 1) // BM_W8,
            ),
            block_dim=512,
        )
    ctx.synchronize()
    s_W8 = Float64(perf_counter_ns() - t_W8) / 1e9 / Float64(ITERS)
    print("    16fal-M4N2 BM", BM_W8, "BN", BN_W8,
          "|", Int(s_W8 * 1e6), "us =",
          Int(flops / s_W8 / 1e12), "TFLOPS")


def main() raises:
    seed(20260727)
    var ctx = DeviceContext()
    shape(ctx, 17408, 5120, 1024)
    shape(ctx, 5120, 17408, 1024)
    shape(ctx, 6144, 5120, 1024)
