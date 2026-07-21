# ===== File: bench_decode_mistral.mojo — decode-kernel timing at Mistral-7B Q4_K_M shapes =====
# Temporary profiling helper: per-launch time and effective bandwidth of every
# weight-reading kernel in the fused Mistral decode step.

from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.gemv2 import gemv_q4_k_f16_v2, gemv_q6_k_f16_v2, gemv_q6_k_out_f32_v2
from src.decode_fused import gemv_norm_q4_k_f16, gemv_norm_q6_k_f16
from src.decode_fused import gemv_norm_silu_q4_k_f16
from src.decode_fused import gemv_residual_q4_k_f16, gemv_residual_q6_k_f16
from src.decode_dp4a import gemv_norm_q4_k_dp4a_f16, gemv_norm_q6_k_dp4a_f16
from src.decode_dp4a import gemv_norm_silu_q4_k_dp4a_f16
from src.decode_dp4a import gemv_residual_q4_k_dp4a_f16, gemv_residual_q6_k_dp4a_f16
from src.decode_dp4a import gemv_q4_k_dp4a_f16, gemv_q6_k_dp4a_out_f32

comptime HID = 4096
comptime QDIM = 4096
comptime KVDIM = 1024
comptime INTER = 14336
comptime VOCAB = 32768
comptime EPS: Float32 = 1e-5


def main() raises:
    var ctx = DeviceContext()
    comptime ITERS = 200

    var h = ctx.enqueue_create_buffer[DType.float16](HID)
    var h32 = ctx.enqueue_create_buffer[DType.float32](HID)
    var nw = ctx.enqueue_create_buffer[DType.float16](HID)
    var x_inter = ctx.enqueue_create_buffer[DType.float16](INTER)
    var act = ctx.enqueue_create_buffer[DType.float16](INTER)
    var yq = ctx.enqueue_create_buffer[DType.float16](QDIM)
    var ykv = ctx.enqueue_create_buffer[DType.float16](KVDIM)
    var ylog = ctx.enqueue_create_buffer[DType.float32](VOCAB)

    var wq4_q = ctx.enqueue_create_buffer[DType.uint8](QDIM * (HID // 256) * 144)
    var wq4_kv = ctx.enqueue_create_buffer[DType.uint8](KVDIM * (HID // 256) * 144)
    var wq6_kv = ctx.enqueue_create_buffer[DType.uint8](KVDIM * (HID // 256) * 210)
    var wq4_gu = ctx.enqueue_create_buffer[DType.uint8](2 * INTER * (HID // 256) * 144)
    var wq6_down = ctx.enqueue_create_buffer[DType.uint8](HID * (INTER // 256) * 210)
    var wq6_head = ctx.enqueue_create_buffer[DType.uint8](VOCAB * (HID // 256) * 210)

    # warmup to boost clocks
    for _ in range(300):
        ctx.enqueue_function[gemv_norm_silu_q4_k_f16](
            act.unsafe_ptr(), wq4_gu.unsafe_ptr(), h.unsafe_ptr(), h32.unsafe_ptr(),
            nw.unsafe_ptr(), HID, INTER, EPS, 7,
            grid_dim=(INTER + 55) // 56, block_dim=256,
        )
    ctx.synchronize()

    # gemv_norm q4k (attn q projection)
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_norm_q4_k_f16](
            yq.unsafe_ptr(), wq4_q.unsafe_ptr(), h.unsafe_ptr(), h32.unsafe_ptr(),
            nw.unsafe_ptr(), HID, QDIM, 0, EPS, 2,
            grid_dim=(QDIM + 15) // 16, block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bts = Float64(QDIM * (HID // 256) * 144)
    print("gemv_norm_q4k 4096x4096:", ms, "ms ", bts / (ms / 1e3) / 1e9, "GB/s")

    # plain q4k gemv same shape (fusion overhead reference)
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q4_k_f16_v2](
            yq.unsafe_ptr(), wq4_q.unsafe_ptr(), h.unsafe_ptr(), HID, QDIM,
            grid_dim=(QDIM + 7) // 8, block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    print("gemv_q4k      4096x4096:", ms, "ms ", bts / (ms / 1e3) / 1e9, "GB/s")

    # gemv_norm q4k (k) + q6k (v)
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_norm_q4_k_f16](
            ykv.unsafe_ptr(), wq4_kv.unsafe_ptr(), h.unsafe_ptr(), h32.unsafe_ptr(),
            nw.unsafe_ptr(), HID, KVDIM, 0, EPS, 1,
            grid_dim=(KVDIM + 7) // 8, block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bts = Float64(KVDIM * (HID // 256) * 144)
    print("gemv_norm_q4k 1024x4096:", ms, "ms ", bts / (ms / 1e3) / 1e9, "GB/s")

    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_norm_q6_k_f16](
            ykv.unsafe_ptr(), wq6_kv.unsafe_ptr(), h.unsafe_ptr(), h32.unsafe_ptr(),
            nw.unsafe_ptr(), HID, KVDIM, 0, EPS, 1,
            grid_dim=(KVDIM + 7) // 8, block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bts = Float64(KVDIM * (HID // 256) * 210)
    print("gemv_norm_q6k 1024x4096:", ms, "ms ", bts / (ms / 1e3) / 1e9, "GB/s")

    # o-projection residual (q4k 4096x4096)
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_residual_q4_k_f16](
            h.unsafe_ptr(), h32.unsafe_ptr(), wq4_q.unsafe_ptr(), yq.unsafe_ptr(), QDIM, HID,
            grid_dim=(HID + 7) // 8, block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bts = Float64(HID * (QDIM // 256) * 144)
    print("gemv_res_q4k  4096x4096:", ms, "ms ", bts / (ms / 1e3) / 1e9, "GB/s")

    # gate|up fused silu (q4k 28672x4096)
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_norm_silu_q4_k_f16](
            act.unsafe_ptr(), wq4_gu.unsafe_ptr(), h.unsafe_ptr(), h32.unsafe_ptr(),
            nw.unsafe_ptr(), HID, INTER, EPS, 7,
            grid_dim=(INTER + 55) // 56, block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bts = Float64(2 * INTER * (HID // 256) * 144)
    print("gemv_silu_q4k 28672x4096:", ms, "ms ", bts / (ms / 1e3) / 1e9, "GB/s")

    # down-projection residual (q6k 4096x14336)
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_residual_q6_k_f16](
            h.unsafe_ptr(), h32.unsafe_ptr(), wq6_down.unsafe_ptr(), x_inter.unsafe_ptr(), INTER, HID,
            grid_dim=(HID + 7) // 8, block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bts = Float64(HID * (INTER // 256) * 210)
    print("gemv_res_q6k  4096x14336:", ms, "ms ", bts / (ms / 1e3) / 1e9, "GB/s")

    # logits (q6k 32768x4096)
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q6_k_out_f32_v2](
            ylog.unsafe_ptr(), wq6_head.unsafe_ptr(), h.unsafe_ptr(), HID, VOCAB,
            grid_dim=(VOCAB + 7) // 8, block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bts = Float64(VOCAB * (HID // 256) * 210)
    print("gemv_q6k_f32  32768x4096:", ms, "ms ", bts / (ms / 1e3) / 1e9, "GB/s")

    # ---- dp4a (int8-activation) variants of the same launches ----
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_norm_q4_k_dp4a_f16](
            yq.unsafe_ptr(), wq4_q.unsafe_ptr(), h.unsafe_ptr(), h32.unsafe_ptr(),
            nw.unsafe_ptr(), HID, QDIM, 0, EPS, 2,
            grid_dim=(QDIM + 15) // 16, block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bts = Float64(QDIM * (HID // 256) * 144)
    print("dp4a_norm_q4k 4096x4096:", ms, "ms ", bts / (ms / 1e3) / 1e9, "GB/s")

    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q4_k_dp4a_f16](
            yq.unsafe_ptr(), wq4_q.unsafe_ptr(), h.unsafe_ptr(), HID, QDIM,
            grid_dim=(QDIM + 7) // 8, block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    print("dp4a_q4k      4096x4096:", ms, "ms ", bts / (ms / 1e3) / 1e9, "GB/s")

    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_norm_q4_k_dp4a_f16](
            ykv.unsafe_ptr(), wq4_kv.unsafe_ptr(), h.unsafe_ptr(), h32.unsafe_ptr(),
            nw.unsafe_ptr(), HID, KVDIM, 0, EPS, 1,
            grid_dim=(KVDIM + 7) // 8, block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bts = Float64(KVDIM * (HID // 256) * 144)
    print("dp4a_norm_q4k 1024x4096:", ms, "ms ", bts / (ms / 1e3) / 1e9, "GB/s")

    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_norm_q6_k_dp4a_f16](
            ykv.unsafe_ptr(), wq6_kv.unsafe_ptr(), h.unsafe_ptr(), h32.unsafe_ptr(),
            nw.unsafe_ptr(), HID, KVDIM, 0, EPS, 1,
            grid_dim=(KVDIM + 7) // 8, block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bts = Float64(KVDIM * (HID // 256) * 210)
    print("dp4a_norm_q6k 1024x4096:", ms, "ms ", bts / (ms / 1e3) / 1e9, "GB/s")

    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_residual_q4_k_dp4a_f16](
            h.unsafe_ptr(), h32.unsafe_ptr(), wq4_q.unsafe_ptr(), yq.unsafe_ptr(), QDIM, HID,
            grid_dim=(HID + 7) // 8, block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bts = Float64(HID * (QDIM // 256) * 144)
    print("dp4a_res_q4k  4096x4096:", ms, "ms ", bts / (ms / 1e3) / 1e9, "GB/s")

    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_norm_silu_q4_k_dp4a_f16](
            act.unsafe_ptr(), wq4_gu.unsafe_ptr(), h.unsafe_ptr(), h32.unsafe_ptr(),
            nw.unsafe_ptr(), HID, INTER, EPS, 7,
            grid_dim=(INTER + 55) // 56, block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bts = Float64(2 * INTER * (HID // 256) * 144)
    print("dp4a_silu_q4k 28672x4096:", ms, "ms ", bts / (ms / 1e3) / 1e9, "GB/s")

    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_residual_q6_k_dp4a_f16](
            h.unsafe_ptr(), h32.unsafe_ptr(), wq6_down.unsafe_ptr(), x_inter.unsafe_ptr(), INTER, HID,
            grid_dim=(HID + 7) // 8, block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bts = Float64(HID * (INTER // 256) * 210)
    print("dp4a_res_q6k  4096x14336:", ms, "ms ", bts / (ms / 1e3) / 1e9, "GB/s")

    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q6_k_dp4a_out_f32](
            ylog.unsafe_ptr(), wq6_head.unsafe_ptr(), h.unsafe_ptr(), HID, VOCAB,
            grid_dim=(VOCAB + 7) // 8, block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    bts = Float64(VOCAB * (HID // 256) * 210)
    print("dp4a_q6k_f32  32768x4096:", ms, "ms ", bts / (ms / 1e3) / 1e9, "GB/s")
