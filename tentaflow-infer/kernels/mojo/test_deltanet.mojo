# ===== File: test_deltanet.mojo — on-GPU numeric check of the DeltaNet kernels =====
# Runs each kernel on deterministic data and compares against a scalar CPU
# oracle recomputing the exact forge-formats/src/deltanet.rs math. Fast local
# feedback; the authoritative golden tests live in Rust (forge-kernels).

from std.gpu.host import DeviceContext
from std.math import exp, sqrt, log
from src.deltanet import (
    deltanet_conv_silu_f16,
    l2norm_heads_f16,
    deltanet_gated_step_f16,
    deltanet_gated_rmsnorm_f16,
    deltanet_log_decay_f32,
    deltanet_beta_sigmoid_f32,
)


def _silu(v: Float32) -> Float32:
    return v / (1.0 + exp(-v))


def _f(i: Int) -> Float32:
    return Float32((i * 37 % 19) - 9) * 0.13


def main() raises:
    var ctx = DeviceContext()
    comptime EPS: Float32 = 1e-6

    # ---------------- conv + silu ----------------
    comptime CONV_DIM = 8192
    comptime D_CONV = 4
    var win = ctx.enqueue_create_buffer[DType.float16](CONV_DIM * (D_CONV - 1))
    var xnew = ctx.enqueue_create_buffer[DType.float16](CONV_DIM)
    var cw = ctx.enqueue_create_buffer[DType.float16](CONV_DIM * D_CONV)
    var cout = ctx.enqueue_create_buffer[DType.float16](CONV_DIM)
    with win.map_to_host() as wh, xnew.map_to_host() as xh, cw.map_to_host() as ch:
        for c in range(CONV_DIM):
            for j in range(D_CONV - 1):
                wh[c * (D_CONV - 1) + j] = Float16(_f(c * 3 + j))
            xh[c] = Float16(_f(c * 7 + 1))
            for j in range(D_CONV):
                ch[c * D_CONV + j] = Float16(_f(c + j * 5) * 0.5)
    ctx.enqueue_function[deltanet_conv_silu_f16](
        cout.unsafe_ptr(), win.unsafe_ptr(), xnew.unsafe_ptr(), cw.unsafe_ptr(),
        CONV_DIM, D_CONV, grid_dim=64, block_dim=256,
    )
    ctx.synchronize()
    var conv_err: Float32 = 0.0
    with cout.map_to_host() as oh:
        for c in range(CONV_DIM):
            var acc: Float32 = 0.0
            for j in range(D_CONV - 1):
                acc += Float32(Float16(_f(c + j * 5) * 0.5)) * Float32(Float16(_f(c * 3 + j)))
            acc += Float32(Float16(_f(c + (D_CONV - 1) * 5) * 0.5)) * Float32(Float16(_f(c * 7 + 1)))
            refv = _silu(acc)
            e = abs(Float32(oh[c]) - refv)
            if e > conv_err:
                conv_err = e
    print("conv_silu   max_err:", conv_err)

    # ---------------- l2 norm per head ----------------
    comptime NH = 32
    comptime DS = 128
    var l2in = ctx.enqueue_create_buffer[DType.float16](NH * DS)
    var l2out = ctx.enqueue_create_buffer[DType.float16](NH * DS)
    with l2in.map_to_host() as ih:
        for i in range(NH * DS):
            ih[i] = Float16(_f(i * 2 + 3))
    ctx.enqueue_function[l2norm_heads_f16](
        l2out.unsafe_ptr(), l2in.unsafe_ptr(), DS, EPS,
        grid_dim=NH, block_dim=128,
    )
    ctx.synchronize()
    var l2_err: Float32 = 0.0
    with l2out.map_to_host() as oh:
        for h in range(NH):
            var ss: Float32 = 0.0
            for j in range(DS):
                v = Float32(Float16(_f((h * DS + j) * 2 + 3)))
                ss += v * v
            inv = 1.0 / sqrt(ss + EPS)
            for j in range(DS):
                v = Float32(Float16(_f((h * DS + j) * 2 + 3)))
                e = abs(Float32(oh[h * DS + j]) - v * inv)
                if e > l2_err:
                    l2_err = e
    print("l2norm      max_err:", l2_err)

    # ---------------- gated delta step ----------------
    var st = ctx.enqueue_create_buffer[DType.float32](NH * DS * DS)
    var q = ctx.enqueue_create_buffer[DType.float16](NH * DS)
    var k = ctx.enqueue_create_buffer[DType.float16](NH * DS)
    var vbuf = ctx.enqueue_create_buffer[DType.float16](NH * DS)
    var g = ctx.enqueue_create_buffer[DType.float32](NH)
    var beta = ctx.enqueue_create_buffer[DType.float32](NH)
    var dout = ctx.enqueue_create_buffer[DType.float16](NH * DS)
    with st.map_to_host() as sh, q.map_to_host() as qh, k.map_to_host() as kh, vbuf.map_to_host() as vh, g.map_to_host() as gh, beta.map_to_host() as bh:
        for i in range(NH * DS * DS):
            sh[i] = _f(i + 5) * 0.05
        for i in range(NH * DS):
            qh[i] = Float16(_f(i * 5 + 1))
            kh[i] = Float16(_f(i * 3 + 2))
            vh[i] = Float16(_f(i * 7 + 4))
        for h in range(NH):
            gh[h] = -0.1 - Float32(h % 5) * 0.05
            bh[h] = 0.3 + Float32(h % 4) * 0.1
    ctx.enqueue_function[deltanet_gated_step_f16](
        dout.unsafe_ptr(), st.unsafe_ptr(), q.unsafe_ptr(), k.unsafe_ptr(),
        vbuf.unsafe_ptr(), g.unsafe_ptr(), beta.unsafe_ptr(), DS,
        grid_dim=NH, block_dim=128,
    )
    ctx.synchronize()
    var step_err: Float32 = 0.0
    var state_err: Float32 = 0.0
    with dout.map_to_host() as oh, st.map_to_host() as sh:
        for h in range(NH):
            decay = exp(-0.1 - Float32(h % 5) * 0.05)
            bta = 0.3 + Float32(h % 4) * 0.1
            # Recompute the reference state column-by-column.
            var sr = List[Float32]()
            for idx in range(DS * DS):
                sr.append(_f(h * DS * DS + idx + 5) * 0.05 * decay)
            var kv = List[Float32]()
            for j in range(DS):
                var acc: Float32 = 0.0
                for i in range(DS):
                    acc += Float32(Float16(_f((h * DS + i) * 3 + 2))) * sr[i * DS + j]
                kv.append(acc)
            for j in range(DS):
                dj = bta * (Float32(Float16(_f((h * DS + j) * 7 + 4))) - kv[j])
                for i in range(DS):
                    sr[i * DS + j] += Float32(Float16(_f((h * DS + i) * 3 + 2))) * dj
            inv_sqrt = 1.0 / sqrt(Float32(DS))
            for j in range(DS):
                var o: Float32 = 0.0
                for i in range(DS):
                    o += Float32(Float16(_f((h * DS + i) * 5 + 1))) * inv_sqrt * sr[i * DS + j]
                e = abs(Float32(oh[h * DS + j]) - o)
                if e > step_err:
                    step_err = e
            for idx in range(DS * DS):
                e = abs(sh[h * DS * DS + idx] - sr[idx])
                if e > state_err:
                    state_err = e
    print("delta_step  max_err:", step_err, " state_err:", state_err)

    # ---------------- gated rmsnorm ----------------
    var go = ctx.enqueue_create_buffer[DType.float16](NH * DS)
    var gz = ctx.enqueue_create_buffer[DType.float16](NH * DS)
    var gw = ctx.enqueue_create_buffer[DType.float16](DS)
    var grout = ctx.enqueue_create_buffer[DType.float16](NH * DS)
    with go.map_to_host() as oh, gz.map_to_host() as zh, gw.map_to_host() as wh:
        for i in range(NH * DS):
            oh[i] = Float16(_f(i * 11 + 1))
            zh[i] = Float16(_f(i * 13 + 2))
        for j in range(DS):
            wh[j] = Float16(1.0 + Float32(j % 5) * 0.1)
    ctx.enqueue_function[deltanet_gated_rmsnorm_f16](
        grout.unsafe_ptr(), go.unsafe_ptr(), gz.unsafe_ptr(), gw.unsafe_ptr(),
        DS, EPS, grid_dim=NH, block_dim=128,
    )
    ctx.synchronize()
    var gr_err: Float32 = 0.0
    with grout.map_to_host() as oh:
        for h in range(NH):
            var ss: Float32 = 0.0
            for j in range(DS):
                ov = Float32(Float16(_f((h * DS + j) * 11 + 1)))
                ss += ov * ov
            inv = 1.0 / sqrt(ss / Float32(DS) + EPS)
            for j in range(DS):
                ov = Float32(Float16(_f((h * DS + j) * 11 + 1)))
                zv = Float32(Float16(_f((h * DS + j) * 13 + 2)))
                wv = Float32(Float16(1.0 + Float32(j % 5) * 0.1))
                refv = ov * inv * wv * _silu(zv)
                e = abs(Float32(oh[h * DS + j]) - refv)
                if e > gr_err:
                    gr_err = e
    print("gated_rmsnorm max_err:", gr_err)

    # ---------------- log-decay + beta sigmoid ----------------
    var al = ctx.enqueue_create_buffer[DType.float16](NH)
    var dt = ctx.enqueue_create_buffer[DType.float16](NH)
    var asc = ctx.enqueue_create_buffer[DType.float16](NH)
    var gres = ctx.enqueue_create_buffer[DType.float32](NH)
    var bres = ctx.enqueue_create_buffer[DType.float32](NH)
    with al.map_to_host() as ah, dt.map_to_host() as dh, asc.map_to_host() as sh:
        for h in range(NH):
            ah[h] = Float16(_f(h * 3 + 1) * 0.5)
            dh[h] = Float16(_f(h * 5 + 2) * 0.1)
            sh[h] = Float16(-exp(_f(h) * 0.2))
    ctx.enqueue_function[deltanet_log_decay_f32](
        gres.unsafe_ptr(), al.unsafe_ptr(), dt.unsafe_ptr(), asc.unsafe_ptr(),
        NH, grid_dim=1, block_dim=NH,
    )
    ctx.enqueue_function[deltanet_beta_sigmoid_f32](
        bres.unsafe_ptr(), al.unsafe_ptr(), NH, grid_dim=1, block_dim=NH,
    )
    ctx.synchronize()
    var g_err: Float32 = 0.0
    var b_err: Float32 = 0.0
    with gres.map_to_host() as gh, bres.map_to_host() as bh:
        for h in range(NH):
            x = Float32(Float16(_f(h * 3 + 1) * 0.5)) + Float32(Float16(_f(h * 5 + 2) * 0.1))
            var sp: Float32
            if x > 20.0:
                sp = x
            else:
                sp = log(1.0 + exp(x))
            gref = sp * Float32(Float16(-exp(_f(h) * 0.2)))
            e = abs(Float32(gh[h]) - gref)
            if e > g_err:
                g_err = e
            bref = 1.0 / (1.0 + exp(-Float32(Float16(_f(h * 3 + 1) * 0.5))))
            eb = abs(Float32(bh[h]) - bref)
            if eb > b_err:
                b_err = eb
    print("log_decay   max_err:", g_err, " beta_sigmoid max_err:", b_err)
