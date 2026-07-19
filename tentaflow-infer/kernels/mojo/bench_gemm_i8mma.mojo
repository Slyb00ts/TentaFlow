# ===== File: bench_gemm_i8mma.mojo — isolated int8 tensor-core prefill GEMM TOPS =====
# Pre-quantizes the activation tile once (quantize_act_q8_1) then times the
# gemm_i8mma path (bm128 + bm64) at the dominant FFN/attn prefill shapes and
# reports achieved INT8 TOPS (2*M*N*K integer MACs / time) against the 184-TOPS
# s8-mma ceiling on the RTX 4090.
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.gemm import gemm_q4_k_f16
from src.gemm import gemm_q4_k_i8mma, gemm_q4_k_i8mma_bm64, gemm_q4_k_i8mma_big
from src.gemm import quantize_act_q8_1


def _bench_q4k(ctx: DeviceContext, BR: Int, BC: Int, BT: Int) raises:
    var bwk = ctx.enqueue_create_buffer[DType.uint8](BR * (BC // 256) * 144)
    var bx = ctx.enqueue_create_buffer[DType.float16](BT * BC)
    var by = ctx.enqueue_create_buffer[DType.float16](BT * BR)
    var xq = ctx.enqueue_create_buffer[DType.int8](BT * BC)
    var xd = ctx.enqueue_create_buffer[DType.float32](BT * (BC // 32))
    var xsm = ctx.enqueue_create_buffer[DType.float32](BT * (BC // 32))
    comptime ITERS = 40
    ops = 2.0 * Float64(BR) * Float64(BC) * Float64(BT)
    gy = (BR + 63) // 64
    gt128 = (BT + 127) // 128
    gt64 = (BT + 63) // 64
    nbq = (BT * (BC // 32) + 255) // 256

    ctx.enqueue_function[quantize_act_q8_1](
        xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), bx.unsafe_ptr(),
        BC, BT, grid_dim=nbq, block_dim=256,
    )
    ctx.synchronize()

    for _ in range(300):
        ctx.enqueue_function[gemm_q4_k_i8mma](
            by.unsafe_ptr(), bwk.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
            xsm.unsafe_ptr(), BC, BR, BT,
            grid_dim=(gy, gt128), block_dim=256,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q4_k_i8mma](
            by.unsafe_ptr(), bwk.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
            xsm.unsafe_ptr(), BC, BR, BT,
            grid_dim=(gy, gt128), block_dim=256,
        )
    ctx.synchronize()
    ms_m128 = Float64(perf_counter_ns() - t0) / 1e6 / ITERS

    for _ in range(300):
        ctx.enqueue_function[gemm_q4_k_i8mma_bm64](
            by.unsafe_ptr(), bwk.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
            xsm.unsafe_ptr(), BC, BR, BT,
            grid_dim=(gy, gt64), block_dim=256,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q4_k_i8mma_bm64](
            by.unsafe_ptr(), bwk.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
            xsm.unsafe_ptr(), BC, BR, BT,
            grid_dim=(gy, gt64), block_dim=256,
        )
    ctx.synchronize()
    ms_m64 = Float64(perf_counter_ns() - t0) / 1e6 / ITERS

    # big tile: BM=128 x BN=128, 16 warps (512 threads). Grid.x steps by 128.
    gyb = (BR + 127) // 128
    for _ in range(300):
        ctx.enqueue_function[gemm_q4_k_i8mma_big](
            by.unsafe_ptr(), bwk.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
            xsm.unsafe_ptr(), BC, BR, BT,
            grid_dim=(gyb, gt128), block_dim=512,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q4_k_i8mma_big](
            by.unsafe_ptr(), bwk.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
            xsm.unsafe_ptr(), BC, BR, BT,
            grid_dim=(gyb, gt128), block_dim=512,
        )
    ctx.synchronize()
    ms_big = Float64(perf_counter_ns() - t0) / 1e6 / ITERS

    print(
        "Q4K N=", BR, "K=", BC, "T=", BT,
        " i8mma128:", ops / (ms_m128 / 1e3) / 1e12, "TOPS (", ms_m128, "ms)",
        " i8mma64:", ops / (ms_m64 / 1e3) / 1e12, "TOPS (", ms_m64, "ms)",
        " big:", ops / (ms_big / 1e3) / 1e12, "TOPS (", ms_big, "ms)",
    )


def main() raises:
    var ctx = DeviceContext()
    # FFN-sized (down/gate) and attention-proj-sized, at prefill batch sizes.
    _bench_q4k(ctx, 14336, 4096, 128)
    _bench_q4k(ctx, 14336, 4096, 512)
    _bench_q4k(ctx, 4096, 4096, 512)
    _bench_q4k(ctx, 4096, 14336, 512)
    _bench_q4k(ctx, 14336, 4096, 2048)
    _bench_q4k(ctx, 4096, 14336, 2048)
