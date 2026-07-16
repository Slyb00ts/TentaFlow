# ===== File: test_kernels2.mojo — numeric sanity for the encoder kernel set =====
# Covers layernorm(+residual), gelu, conv1d k3, full attention (causal and
# bidirectional) against scalar CPU math.

from std.gpu.host import DeviceContext
from std.math import exp, sqrt, erf
from src.layernorm import layernorm_f16
from src.conv import gelu_f16, conv1d_k3_f16
from src.attn_full import attn_full_f16_hd64
from src.gemv import gemv_f16_bias


def _fill(i: Int) -> Float32:
    return Float32((i * 37 % 19) - 9) * 0.25


def main() raises:
    var ctx = DeviceContext()
    var max_err: Float32 = 0.0

    # --- layernorm ---
    comptime ROWS = 3
    comptime COLS = 512
    comptime EPS: Float32 = 1e-5
    var x = ctx.enqueue_create_buffer[DType.float16](ROWS * COLS)
    var w = ctx.enqueue_create_buffer[DType.float16](COLS)
    var b = ctx.enqueue_create_buffer[DType.float16](COLS)
    var y = ctx.enqueue_create_buffer[DType.float16](ROWS * COLS)
    with x.map_to_host() as xh, w.map_to_host() as wh, b.map_to_host() as bh:
        for i in range(ROWS * COLS):
            xh[i] = Float16(_fill(i) * 0.4)
        for i in range(COLS):
            wh[i] = Float16(0.8 + Float32(i % 7) * 0.05)
            bh[i] = Float16(_fill(i + 3) * 0.02)
    ctx.enqueue_function[layernorm_f16](
        y.unsafe_ptr(), x.unsafe_ptr(), w.unsafe_ptr(), b.unsafe_ptr(), COLS, EPS,
        grid_dim=ROWS, block_dim=256,
    )
    ctx.synchronize()
    with y.map_to_host() as yh:
        for r in range(ROWS):
            var mean: Float32 = 0.0
            for c in range(COLS):
                mean += Float32(Float16(_fill(r * COLS + c) * 0.4))
            mean /= Float32(COLS)
            var vv: Float32 = 0.0
            for c in range(COLS):
                d = Float32(Float16(_fill(r * COLS + c) * 0.4)) - mean
                vv += d * d
            inv = 1.0 / sqrt(vv / Float32(COLS) + EPS)
            for c in range(COLS):
                wv = 0.8 + Float32(c % 7) * 0.05
                bv = Float32(Float16(_fill(c + 3) * 0.02))
                expected = (Float32(Float16(_fill(r * COLS + c) * 0.4)) - mean) * inv * wv + bv
                err = abs(Float32(yh[r * COLS + c]) - expected)
                if err > max_err:
                    max_err = err
    print("layernorm max_err:", max_err)
    if max_err > 0.02:
        raise Error("layernorm FAILED")

    # --- conv1d k3 (stride 1 and 2) + gelu fused ---
    comptime IC = 8
    comptime OC = 6
    comptime IT = 64
    for stride_case in range(2):
        stride = stride_case + 1
        out_t = IT // stride
        var cx = ctx.enqueue_create_buffer[DType.float16](IC * IT)
        var cw = ctx.enqueue_create_buffer[DType.float16](OC * IC * 3)
        var cb = ctx.enqueue_create_buffer[DType.float16](OC)
        var cy = ctx.enqueue_create_buffer[DType.float16](OC * out_t)
        with cx.map_to_host() as h:
            for i in range(IC * IT):
                h[i] = Float16(_fill(i) * 0.2)
        with cw.map_to_host() as h:
            for i in range(OC * IC * 3):
                h[i] = Float16(_fill(i + 5) * 0.1)
        with cb.map_to_host() as h:
            for i in range(OC):
                h[i] = Float16(_fill(i + 11) * 0.05)
        ctx.enqueue_function[conv1d_k3_f16](
            cy.unsafe_ptr(), cx.unsafe_ptr(), cw.unsafe_ptr(), cb.unsafe_ptr(),
            IC, IT, out_t, stride, 1,
            grid_dim=((out_t + 127) // 128, OC), block_dim=128,
        )
        ctx.synchronize()
        max_err = 0.0
        with cy.map_to_host() as yh:
            for oc in range(OC):
                for t in range(out_t):
                    center = t * stride
                    var acc: Float32 = Float32(Float16(_fill(oc + 11) * 0.05))
                    for ic in range(IC):
                        for kk in range(3):
                            src = center + kk - 1
                            if src >= 0 and src < IT:
                                wv = Float32(Float16(_fill(oc * IC * 3 + ic * 3 + kk + 5) * 0.1))
                                xv = Float32(Float16(_fill(ic * IT + src) * 0.2))
                                acc += wv * xv
                    acc = 0.5 * acc * (1.0 + erf(acc / sqrt(Float32(2.0))))
                    err = abs(Float32(yh[oc * out_t + t]) - acc)
                    if err > max_err:
                        max_err = err
        print("conv1d stride", stride, "max_err:", max_err)
        if max_err > 0.02:
            raise Error("conv1d FAILED")

    # --- full attention, causal and bidirectional ---
    comptime NQ = 7
    comptime NKV = 7
    comptime NQH = 4
    comptime NKVH = 2
    comptime HD = 64
    comptime SCALE: Float32 = 0.125
    var qa = ctx.enqueue_create_buffer[DType.float16](NQ * NQH * HD)
    var ka = ctx.enqueue_create_buffer[DType.float16](NKV * NKVH * HD)
    var va = ctx.enqueue_create_buffer[DType.float16](NKV * NKVH * HD)
    var oa = ctx.enqueue_create_buffer[DType.float16](NQ * NQH * HD)
    with qa.map_to_host() as h:
        for i in range(NQ * NQH * HD):
            h[i] = Float16(_fill(i) * 0.2)
    with ka.map_to_host() as h:
        for i in range(NKV * NKVH * HD):
            h[i] = Float16(_fill(i + 3) * 0.2)
    with va.map_to_host() as h:
        for i in range(NKV * NKVH * HD):
            h[i] = Float16(_fill(i + 11) * 0.2)

    for causal_case in range(2):
        ctx.enqueue_function[attn_full_f16_hd64](
            oa.unsafe_ptr(), qa.unsafe_ptr(), ka.unsafe_ptr(), va.unsafe_ptr(),
            NQH, NKVH, NKV, causal_case, 0, SCALE,
            grid_dim=(NQ, NQH), block_dim=128,
        )
        ctx.synchronize()
        max_err = 0.0
        with oa.map_to_host() as oh:
            for t in range(NQ):
                limit = NKV
                if causal_case == 1:
                    limit = t + 1
                for h in range(NQH):
                    kvh = h // (NQH // NKVH)
                    q_base = (t * NQH + h) * HD
                    var m_star: Float32 = -1e30
                    var scores = List[Float32]()
                    for p_i in range(limit):
                        kv_base = (p_i * NKVH + kvh) * HD
                        var dot: Float32 = 0.0
                        for e in range(HD):
                            dot += Float32(Float16(_fill(q_base + e) * 0.2)) * Float32(
                                Float16(_fill(kv_base + e + 3) * 0.2)
                            )
                        sc = dot * SCALE
                        scores.append(sc)
                        if sc > m_star:
                            m_star = sc
                    var denom: Float32 = 0.0
                    for p_i in range(limit):
                        denom += exp(scores[p_i] - m_star)
                    for e in range(HD):
                        var num: Float32 = 0.0
                        for p_i in range(limit):
                            kv_base = (p_i * NKVH + kvh) * HD
                            num += exp(scores[p_i] - m_star) * Float32(
                                Float16(_fill(kv_base + e + 11) * 0.2)
                            )
                        expected = num / denom
                        err = abs(Float32(oh[q_base + e]) - expected)
                        if err > max_err:
                            max_err = err
        print("attn_full causal =", causal_case, "max_err:", max_err)
        if max_err > 0.01:
            raise Error("attn_full FAILED")

    # --- gemv with bias ---
    comptime GR = 12
    comptime GC = 256
    var gw = ctx.enqueue_create_buffer[DType.float16](GR * GC)
    var gx = ctx.enqueue_create_buffer[DType.float16](GC)
    var gb = ctx.enqueue_create_buffer[DType.float16](GR)
    var gy = ctx.enqueue_create_buffer[DType.float16](GR)
    with gw.map_to_host() as h:
        for i in range(GR * GC):
            h[i] = Float16(_fill(i) * 0.05)
    with gx.map_to_host() as h:
        for i in range(GC):
            h[i] = Float16(_fill(i + 7) * 0.1)
    with gb.map_to_host() as h:
        for i in range(GR):
            h[i] = Float16(_fill(i + 13) * 0.3)
    ctx.enqueue_function[gemv_f16_bias](
        gy.unsafe_ptr(), gw.unsafe_ptr(), gx.unsafe_ptr(), gb.unsafe_ptr(), GC,
        grid_dim=GR, block_dim=256,
    )
    ctx.synchronize()
    max_err = 0.0
    with gy.map_to_host() as yh:
        for r in range(GR):
            var acc: Float32 = Float32(Float16(_fill(r + 13) * 0.3))
            for c in range(GC):
                acc += Float32(Float16(_fill(r * GC + c) * 0.05)) * Float32(
                    Float16(_fill(c + 7) * 0.1)
                )
            rel = abs(Float32(yh[r]) - acc) / (abs(acc) + 1.0)
            if rel > max_err:
                max_err = rel
    print("gemv_f16_bias max_rel_err:", max_err)
    if max_err > 0.02:
        raise Error("gemv_f16_bias FAILED")

    print("ALL ENCODER KERNEL CHECKS PASSED")
