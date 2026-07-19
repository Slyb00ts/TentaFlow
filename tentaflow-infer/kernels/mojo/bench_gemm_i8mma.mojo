# ===== File: bench_gemm_i8mma.mojo — f16-dequant vs int8 tensor-core prefill GEMM =====
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.gemm import gemm_q4_k_f16, gemm_q4_k_f16_bm64
from src.gemm import gemm_q4_k_i8mma, gemm_q4_k_i8mma_bm64
from src.gemm import gemm_q8_0_f16, gemm_q8_0_i8mma


def _bench_q4k[BM: Int](ctx: DeviceContext, BR: Int, BC: Int, BT: Int) raises:
    var bwk = ctx.enqueue_create_buffer[DType.uint8](BR * (BC // 256) * 144)
    var bx = ctx.enqueue_create_buffer[DType.float16](BT * BC)
    var by = ctx.enqueue_create_buffer[DType.float16](BT * BR)
    comptime ITERS = 30
    flops = 2.0 * Float64(BR) * Float64(BC) * Float64(BT)
    gy = (BR + 63) // 64
    gt128 = (BT + 127) // 128
    gt64 = (BT + 63) // 64

    for _ in range(300):
        ctx.enqueue_function[gemm_q4_k_f16](
            by.unsafe_ptr(), bwk.unsafe_ptr(), bx.unsafe_ptr(), BC, BR, BT,
            grid_dim=(gy, gt128), block_dim=256,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q4_k_f16](
            by.unsafe_ptr(), bwk.unsafe_ptr(), bx.unsafe_ptr(), BC, BR, BT,
            grid_dim=(gy, gt128), block_dim=256,
        )
    ctx.synchronize()
    ms_f16 = Float64(perf_counter_ns() - t0) / 1e6 / ITERS

    for _ in range(100):
        ctx.enqueue_function[gemm_q4_k_i8mma](
            by.unsafe_ptr(), bwk.unsafe_ptr(), bx.unsafe_ptr(), BC, BR, BT,
            grid_dim=(gy, gt128), block_dim=256,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q4_k_i8mma](
            by.unsafe_ptr(), bwk.unsafe_ptr(), bx.unsafe_ptr(), BC, BR, BT,
            grid_dim=(gy, gt128), block_dim=256,
        )
    ctx.synchronize()
    ms_m128 = Float64(perf_counter_ns() - t0) / 1e6 / ITERS

    for _ in range(100):
        ctx.enqueue_function[gemm_q4_k_i8mma_bm64](
            by.unsafe_ptr(), bwk.unsafe_ptr(), bx.unsafe_ptr(), BC, BR, BT,
            grid_dim=(gy, gt64), block_dim=256,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q4_k_i8mma_bm64](
            by.unsafe_ptr(), bwk.unsafe_ptr(), bx.unsafe_ptr(), BC, BR, BT,
            grid_dim=(gy, gt64), block_dim=256,
        )
    ctx.synchronize()
    ms_m64 = Float64(perf_counter_ns() - t0) / 1e6 / ITERS

    print(
        "Q4K", BR, "x", BC, "T=", BT,
        " f16:", flops / (ms_f16 / 1e3) / 1e12, "TFLOP/s (", ms_f16, "ms)",
        " i8mma128:", flops / (ms_m128 / 1e3) / 1e12, "(", ms_m128, "ms)",
        " i8mma64:", flops / (ms_m64 / 1e3) / 1e12, "(", ms_m64, "ms)",
    )


def main() raises:
    var ctx = DeviceContext()
    # FFN-sized (down/gate) and attention-proj-sized, at prefill batch sizes.
    _bench_q4k[128](ctx, 14336, 4096, 128)
    _bench_q4k[128](ctx, 14336, 4096, 512)
    _bench_q4k[128](ctx, 4096, 4096, 512)
    _bench_q4k[128](ctx, 4096, 14336, 512)
