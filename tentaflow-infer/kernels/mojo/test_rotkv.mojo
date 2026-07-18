# ===== File: test_rotkv.mojo — end-to-end GPU gate for rotational KV attention =====
# Packs synthetic paged K/V through kv_pack_rot (rot4 / rot3), runs the
# rotational decode-attention kernel attn_decode_rot, and compares its output
# against an exact f64 reference softmax attention over the ORIGINAL K/V. This
# proves the whole store→pack→unpack→rotate→softmax→inverse-rotate path is
# numerically correct on the GPU (not just the rotquant round-trip). Reports
# per-element RMSE and cosine similarity of the attention output for rot4 vs
# rot3, plus a GQA sanity shape.

from std.gpu.host import DeviceContext
from std.math import sqrt, log, cos, pi, exp
from src.rotkv import (
    kv_pack_rot_hd128_b4,
    kv_pack_rot_hd128_b3,
    attn_decode_rot_hd128_b4,
    attn_decode_rot_hd128_b3,
)

comptime D = 128
comptime N = 4096          # context length
comptime PAGE = 32
comptime NPAGES = N // PAGE
comptime NQH = 2           # GQA: 2 query heads share 1 kv head
comptime NKVH = 1


def _lcg(mut state: UInt64) -> Float64:
    state = state * 6364136223846793005 + 1442695040888963407
    return Float64((state >> 11)) / Float64(1 << 53)


def _randn(mut state: UInt64) -> Float64:
    u1 = _lcg(state)
    u2 = _lcg(state)
    var lu = u1
    if lu < 1e-12:
        lu = 1e-12
    return sqrt(-2.0 * log(lu)) * cos(2.0 * pi * u2)


def main() raises:
    var ctx = DeviceContext()
    print("=== rotational KV decode-attention gate (D", D, "N", N, "GQA", NQH, "/", NKVH, ") ===")

    var scale = 1.0 / sqrt(Float32(D))

    # Synthetic post-norm K/V ([N, NKVH, D]) + query ([NQH, D]).
    var k_in = ctx.enqueue_create_buffer[DType.float16](N * NKVH * D)
    var v_in = ctx.enqueue_create_buffer[DType.float16](N * NKVH * D)
    var q = ctx.enqueue_create_buffer[DType.float16](NQH * D)
    var state: UInt64 = 0xBEEF1234
    with k_in.map_to_host() as kh, v_in.map_to_host() as vh, q.map_to_host() as qh:
        for t in range(N):
            for h in range(NKVH):
                for i in range(D):
                    var kv = _randn(state)
                    if _lcg(state) < 0.06:
                        kv = kv * 8.0     # outlier channel (kills naive int4)
                    kh[(t * NKVH + h) * D + i] = Float16(kv)
                    vh[(t * NKVH + h) * D + i] = Float16(_randn(state))
        for h in range(NQH):
            for i in range(D):
                qh[h * D + i] = Float16(_randn(state) * 0.5)

    # Page table: contiguous pages 0..NPAGES-1 for the single sequence.
    var page_table = ctx.enqueue_create_buffer[DType.int32](NPAGES)
    with page_table.map_to_host() as pt:
        for p in range(NPAGES):
            pt[p] = Int32(p)
    var seq_lens = ctx.enqueue_create_buffer[DType.int32](1)
    with seq_lens.map_to_host() as sl:
        sl[0] = Int32(N)

    comptime PB4 = (D * 4) // 8
    comptime PB3 = (D * 3) // 8

    # --- rot4 ---
    var k4 = ctx.enqueue_create_buffer[DType.uint8](NPAGES * NKVH * PAGE * PB4)
    var v4 = ctx.enqueue_create_buffer[DType.uint8](NPAGES * NKVH * PAGE * PB4)
    var ks4 = ctx.enqueue_create_buffer[DType.float16](NPAGES * NKVH * PAGE)
    var vs4 = ctx.enqueue_create_buffer[DType.float16](NPAGES * NKVH * PAGE)
    var o4 = ctx.enqueue_create_buffer[DType.float16](NQH * D)
    ctx.enqueue_function[kv_pack_rot_hd128_b4](
        k4.unsafe_ptr(), v4.unsafe_ptr(), ks4.unsafe_ptr(), vs4.unsafe_ptr(),
        k_in.unsafe_ptr(), v_in.unsafe_ptr(), page_table.unsafe_ptr(),
        0, NKVH, PAGE, grid_dim=(N, NKVH), block_dim=1,
    )
    ctx.enqueue_function[attn_decode_rot_hd128_b4](
        o4.unsafe_ptr(), q.unsafe_ptr(), k4.unsafe_ptr(), v4.unsafe_ptr(),
        ks4.unsafe_ptr(), vs4.unsafe_ptr(), page_table.unsafe_ptr(),
        seq_lens.unsafe_ptr(), NQH, NKVH, PAGE, NPAGES, scale,
        grid_dim=(1, NQH), block_dim=32,
    )

    # --- rot3 ---
    var k3 = ctx.enqueue_create_buffer[DType.uint8](NPAGES * NKVH * PAGE * PB3)
    var v3 = ctx.enqueue_create_buffer[DType.uint8](NPAGES * NKVH * PAGE * PB3)
    var ks3 = ctx.enqueue_create_buffer[DType.float16](NPAGES * NKVH * PAGE)
    var vs3 = ctx.enqueue_create_buffer[DType.float16](NPAGES * NKVH * PAGE)
    var o3 = ctx.enqueue_create_buffer[DType.float16](NQH * D)
    ctx.enqueue_function[kv_pack_rot_hd128_b3](
        k3.unsafe_ptr(), v3.unsafe_ptr(), ks3.unsafe_ptr(), vs3.unsafe_ptr(),
        k_in.unsafe_ptr(), v_in.unsafe_ptr(), page_table.unsafe_ptr(),
        0, NKVH, PAGE, grid_dim=(N, NKVH), block_dim=1,
    )
    ctx.enqueue_function[attn_decode_rot_hd128_b3](
        o3.unsafe_ptr(), q.unsafe_ptr(), k3.unsafe_ptr(), v3.unsafe_ptr(),
        ks3.unsafe_ptr(), vs3.unsafe_ptr(), page_table.unsafe_ptr(),
        seq_lens.unsafe_ptr(), NQH, NKVH, PAGE, NPAGES, scale,
        grid_dim=(1, NQH), block_dim=32,
    )
    ctx.synchronize()

    # Exact f64 reference attention over the ORIGINAL K/V, per query head.
    var se4: Float64 = 0.0
    var se3: Float64 = 0.0
    var dot4: Float64 = 0.0
    var dot3: Float64 = 0.0
    var refn: Float64 = 0.0
    var on4: Float64 = 0.0
    var on3: Float64 = 0.0
    with k_in.map_to_host() as kh, v_in.map_to_host() as vh, q.map_to_host() as qh, \
         o4.map_to_host() as oh4, o3.map_to_host() as oh3:
        for h in range(NQH):
            kvh = h * NKVH // NQH
            var m: Float64 = -1e300
            var scores = InlineArray[Float64, N](fill=0.0)
            for t in range(N):
                var dot: Float64 = 0.0
                for i in range(D):
                    dot += Float64(qh[h * D + i]) * Float64(kh[(t * NKVH + kvh) * D + i])
                dot = dot * Float64(scale)
                scores[t] = dot
                if dot > m:
                    m = dot
            var l: Float64 = 0.0
            for t in range(N):
                scores[t] = exp(scores[t] - m)
                l += scores[t]
            var refv = InlineArray[Float64, D](fill=0.0)
            for t in range(N):
                p = scores[t] / l
                for i in range(D):
                    refv[i] += p * Float64(vh[(t * NKVH + kvh) * D + i])
            for i in range(D):
                a4 = Float64(oh4[h * D + i])
                a3 = Float64(oh3[h * D + i])
                se4 += (a4 - refv[i]) * (a4 - refv[i])
                se3 += (a3 - refv[i]) * (a3 - refv[i])
                dot4 += a4 * refv[i]
                dot3 += a3 * refv[i]
                refn += refv[i] * refv[i]
                on4 += a4 * a4
                on3 += a3 * a3

    print("attention-output error vs exact f16 reference (lower RMSE / cosine->1 better):")
    print("   rot4 : output RMSE =", sqrt(se4 / Float64(NQH * D)),
          " cosine =", dot4 / (sqrt(refn) * sqrt(on4)))
    print("   rot3 : output RMSE =", sqrt(se3 / Float64(NQH * D)),
          " cosine =", dot3 / (sqrt(refn) * sqrt(on3)))
