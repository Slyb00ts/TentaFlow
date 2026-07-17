# ===== File: test_prefill.mojo — numeric checks for GEMM/prefill kernels + micro-bench =====

from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from std.math import exp
from src.gemm import transpose_f16, gemm_q8_0_xt_f16, gemm_f16_xt_f16, gemm_nvfp4_xt_f16
from src.prefill import kv_append_batch_f16, attn_prefill_f16_hd64


def _fill(i: Int) -> Float32:
    return Float32((i * 37 % 19) - 9) * 0.25


def main() raises:
    var ctx = DeviceContext()
    var max_err: Float32 = 0.0

    # --- transpose + q8_0 GEMM vs CPU ---
    comptime T = 40
    comptime COLS = 128
    comptime ROWS = 24
    var x = ctx.enqueue_create_buffer[DType.float16](T * COLS)
    var xt = ctx.enqueue_create_buffer[DType.float16](COLS * T)
    var wq = ctx.enqueue_create_buffer[DType.uint8](ROWS * (COLS // 32) * 34)
    var y = ctx.enqueue_create_buffer[DType.float16](T * ROWS)
    with x.map_to_host() as h:
        for i in range(T * COLS):
            h[i] = Float16(_fill(i) * 0.1)
    with wq.map_to_host() as h:
        for r in range(ROWS):
            for b in range(COLS // 32):
                off = (r * (COLS // 32) + b) * 34
                sc = Float16(0.02 + Float32((r + b) % 5) * 0.01)
                bits = sc.to_bits()
                h[off] = UInt8(bits & 0xFF)
                h[off + 1] = UInt8((bits >> 8) & 0xFF)
                for k in range(32):
                    h[off + 2 + k] = UInt8((Int((r * 31 + b * 17 + k * 13) % 255) - 127) & 0xFF)

    ctx.enqueue_function[transpose_f16](
        xt.unsafe_ptr(), x.unsafe_ptr(), T, COLS,
        grid_dim=((COLS + 31) // 32, (T + 7) // 8), block_dim=256,
    )
    ctx.enqueue_function[gemm_q8_0_xt_f16](
        y.unsafe_ptr(), wq.unsafe_ptr(), xt.unsafe_ptr(), COLS, ROWS, T,
        grid_dim=((ROWS + 7) // 8, (T + 127) // 128), block_dim=256,
    )
    ctx.synchronize()

    with y.map_to_host() as yh, x.map_to_host() as xh, wq.map_to_host() as wh:
        for t in range(T):
            for r in range(ROWS):
                var acc: Float32 = 0.0
                for b in range(COLS // 32):
                    off = (r * (COLS // 32) + b) * 34
                    # Recompute the scale the same way it was generated.
                    sref = Float32(Float16(0.02 + Float32((r + b) % 5) * 0.01))
                    var dot: Float32 = 0.0
                    for k in range(32):
                        var q = Int(wh[off + 2 + k])
                        if q > 127:
                            q -= 256
                        dot += Float32(q) * Float32(xh[t * COLS + b * 32 + k])
                    acc += sref * dot
                rel = abs(Float32(yh[t * ROWS + r]) - acc) / (abs(acc) + 1.0)
                if rel > max_err:
                    max_err = rel
    print("gemm_q8_0 max_rel_err:", max_err)
    if max_err > 0.02:
        raise Error("gemm_q8_0 FAILED")

    # --- kv_append_batch + attn_prefill vs reference ---
    comptime NKVH = 2
    comptime NQH = 4
    comptime HD = 64
    comptime PAGE = 16
    comptime NPAGES = 8
    comptime BASE = 5
    comptime TT = 19
    var kc = ctx.enqueue_create_buffer[DType.float16](NPAGES * NKVH * PAGE * HD)
    var vc = ctx.enqueue_create_buffer[DType.float16](NPAGES * NKVH * PAGE * HD)
    var pt = ctx.enqueue_create_buffer[DType.int32](NPAGES)
    var kin = ctx.enqueue_create_buffer[DType.float16](TT * NKVH * HD)
    var vin = ctx.enqueue_create_buffer[DType.float16](TT * NKVH * HD)
    var qb = ctx.enqueue_create_buffer[DType.float16](TT * NQH * HD)
    var ob = ctx.enqueue_create_buffer[DType.float16](TT * NQH * HD)
    with pt.map_to_host() as h:
        # scattered pages: logical i -> physical (7 - i)
        for i in range(NPAGES):
            h[i] = Int32(7 - i)
    # pre-fill positions 0..BASE with known K/V (as if already prefilled)
    with kc.map_to_host() as kh, vc.map_to_host() as vh:
        for i in range(NPAGES * NKVH * PAGE * HD):
            kh[i] = Float16(0.0)
            vh[i] = Float16(0.0)
        for pos in range(BASE):
            page = 7 - (pos // PAGE)
            slot = pos % PAGE
            for kvh_i in range(NKVH):
                base_off = ((page * NKVH + kvh_i) * PAGE + slot) * HD
                for e in range(HD):
                    kh[base_off + e] = Float16(_fill(pos * 1000 + kvh_i * 100 + e + 3) * 0.2)
                    vh[base_off + e] = Float16(_fill(pos * 1000 + kvh_i * 100 + e + 11) * 0.2)
    with kin.map_to_host() as h:
        for t in range(TT):
            for kvh_i in range(NKVH):
                for e in range(HD):
                    h[(t * NKVH + kvh_i) * HD + e] = Float16(
                        _fill((BASE + t) * 1000 + kvh_i * 100 + e + 3) * 0.2
                    )
    with vin.map_to_host() as h:
        for t in range(TT):
            for kvh_i in range(NKVH):
                for e in range(HD):
                    h[(t * NKVH + kvh_i) * HD + e] = Float16(
                        _fill((BASE + t) * 1000 + kvh_i * 100 + e + 11) * 0.2
                    )
    with qb.map_to_host() as h:
        for i in range(TT * NQH * HD):
            h[i] = Float16(_fill(i) * 0.2)

    ctx.enqueue_function[kv_append_batch_f16](
        kc.unsafe_ptr(), vc.unsafe_ptr(), kin.unsafe_ptr(), vin.unsafe_ptr(),
        pt.unsafe_ptr(), BASE, NKVH, PAGE, HD,
        grid_dim=(TT, NKVH), block_dim=64,
    )
    comptime SCALE: Float32 = 0.125
    ctx.enqueue_function[attn_prefill_f16_hd64](
        ob.unsafe_ptr(), qb.unsafe_ptr(), kc.unsafe_ptr(), vc.unsafe_ptr(),
        pt.unsafe_ptr(), BASE, NQH, NKVH, PAGE, SCALE,
        grid_dim=(TT, NQH), block_dim=128,
    )
    ctx.synchronize()

    max_err = 0.0
    with ob.map_to_host() as oh:
        for t in range(TT):
            ctx_len = BASE + t + 1
            for h in range(NQH):
                kvh_i = h // (NQH // NKVH)
                q_base = (t * NQH + h) * HD
                var m_star: Float32 = -1e30
                var scores = List[Float32]()
                for pos in range(ctx_len):
                    var dot: Float32 = 0.0
                    for e in range(HD):
                        qf = Float32(Float16(_fill(q_base + e) * 0.2))
                        kf = Float32(Float16(_fill(pos * 1000 + kvh_i * 100 + e + 3) * 0.2))
                        dot += qf * kf
                    sref2 = dot * SCALE
                    scores.append(sref2)
                    if sref2 > m_star:
                        m_star = sref2
                var denom: Float32 = 0.0
                for pos in range(ctx_len):
                    denom += exp(scores[pos] - m_star)
                for e in range(HD):
                    var num: Float32 = 0.0
                    for pos in range(ctx_len):
                        vf = Float32(Float16(_fill(pos * 1000 + kvh_i * 100 + e + 11) * 0.2))
                        num += exp(scores[pos] - m_star) * vf
                    expected = num / denom
                    err = abs(Float32(oh[q_base + e]) - expected)
                    if err > max_err:
                        max_err = err
    print("attn_prefill max_err:", max_err)
    if max_err > 0.01:
        raise Error("attn_prefill FAILED")

    # --- GEMM micro-bench (ffn-sized) ---
    comptime BR = 11264
    comptime BC = 4096
    comptime BT = 256
    var bw = ctx.enqueue_create_buffer[DType.uint8](BR * (BC // 32) * 34)
    var bxt = ctx.enqueue_create_buffer[DType.float16](BC * BT)
    var by = ctx.enqueue_create_buffer[DType.float16](BT * BR)
    for _ in range(2):
        ctx.enqueue_function[gemm_q8_0_xt_f16](
            by.unsafe_ptr(), bw.unsafe_ptr(), bxt.unsafe_ptr(), BC, BR, BT,
            grid_dim=((BR + 7) // 8, (BT + 127) // 128), block_dim=256,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    comptime ITERS = 10
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q8_0_xt_f16](
            by.unsafe_ptr(), bw.unsafe_ptr(), bxt.unsafe_ptr(), BC, BR, BT,
            grid_dim=((BR + 7) // 8, (BT + 127) // 128), block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    flops = 2.0 * Float64(BR) * Float64(BC) * Float64(BT)
    print("gemm_q8_0 11264x4096 T=256:", ms, "ms  ", flops / (ms / 1e3) / 1e12, "TFLOP/s")

    print("ALL PREFILL KERNEL CHECKS PASSED")
