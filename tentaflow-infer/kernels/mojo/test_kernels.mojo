# ===== File: test_kernels.mojo — on-GPU numeric sanity for kernel sources =====
# Fast feedback while editing kernels: runs each kernel on deterministic data
# and compares against scalar CPU math computed here. The authoritative golden
# tests live in Rust (forge-kernels) against forge-formats references.

from std.gpu.host import DeviceContext
from std.math import rsqrt, exp, sqrt, cos, sin, pow
from src.norm import rmsnorm_f16
from src.activation import silu_mul_f16
from src.rope import rope_neox_f16
from src.gemv import gemv_q8_0_f16, gemv_f16
from src.gemv2 import gemv_q4_k_f16_v2, gemv_q4_k_out_f32_v2
from src.attention import attn_decode_f16_hd64
from src.kv_append import kv_append_f16
from src.qkv_post import qkv_post_f16

comptime ROWS = 4
comptime COLS = 1024
comptime EPS: Float32 = 1e-6


def _fill(i: Int) -> Float32:
    # Deterministic pseudo-data with sign changes and magnitude variation.
    return Float32((i * 37 % 19) - 9) * 0.25


def _gsm_ref(sm4: Int, s: Int, sp4: Int, j: Int) -> Tuple[Float32, Float32]:
    # Independent CPU oracle for llama.cpp get_scale_min_k4:
    # sm4 = scales[j-4] (ignored for j < 4), s = scales[j], sp4 = scales[j+4].
    if j < 4:
        return (Float32(s & 63), Float32(sp4 & 63))
    sc = (sp4 & 0x0F) | ((sm4 >> 6) << 4)
    mn = (sp4 >> 4) | ((s >> 6) << 4)
    return (Float32(sc), Float32(mn))


def main() raises:
    var ctx = DeviceContext()

    # --- rmsnorm ---
    var x = ctx.enqueue_create_buffer[DType.float16](ROWS * COLS)
    var w = ctx.enqueue_create_buffer[DType.float16](COLS)
    var y = ctx.enqueue_create_buffer[DType.float16](ROWS * COLS)
    with x.map_to_host() as xh, w.map_to_host() as wh:
        for i in range(ROWS * COLS):
            xh[i] = Float16(_fill(i))
        for i in range(COLS):
            wh[i] = Float16(1.0 + Float32(i % 5) * 0.1)
    ctx.enqueue_function[rmsnorm_f16](
        y.unsafe_ptr(), x.unsafe_ptr(), w.unsafe_ptr(), COLS, EPS,
        grid_dim=ROWS, block_dim=256,
    )
    ctx.synchronize()

    var max_err: Float32 = 0.0
    with y.map_to_host() as yh:
        for r in range(ROWS):
            var ss: Float32 = 0.0
            for c in range(COLS):
                v = _fill(r * COLS + c)
                ss += v * v
            inv = rsqrt(ss / Float32(COLS) + EPS)
            for c in range(COLS):
                expected = _fill(r * COLS + c) * inv * (1.0 + Float32(c % 5) * 0.1)
                got = Float32(yh[r * COLS + c])
                err = abs(got - expected)
                if err > max_err:
                    max_err = err
    print("rmsnorm max_err:", max_err)
    if max_err > 0.01:
        raise Error("rmsnorm_f16 numeric check FAILED")

    # --- silu_mul ---
    comptime N = 4096
    var g = ctx.enqueue_create_buffer[DType.float16](N)
    var u = ctx.enqueue_create_buffer[DType.float16](N)
    var o = ctx.enqueue_create_buffer[DType.float16](N)
    with g.map_to_host() as gh, u.map_to_host() as uh:
        for i in range(N):
            gh[i] = Float16(_fill(i) * 0.5)
            uh[i] = Float16(_fill(i + 7) * 0.5)
    ctx.enqueue_function[silu_mul_f16](
        o.unsafe_ptr(), g.unsafe_ptr(), u.unsafe_ptr(), N,
        grid_dim=(N + 255) // 256, block_dim=256,
    )
    ctx.synchronize()

    max_err = 0.0
    with o.map_to_host() as oh:
        for i in range(N):
            gv = Float32(Float16(_fill(i) * 0.5))
            uv = Float32(Float16(_fill(i + 7) * 0.5))
            expected = gv / (1.0 + exp(-gv)) * uv
            err = abs(Float32(oh[i]) - expected)
            if err > max_err:
                max_err = err
    print("silu_mul max_err:", max_err)
    if max_err > 0.01:
        raise Error("silu_mul_f16 numeric check FAILED")

    # --- rope (neox) ---
    comptime N_TOK = 3
    comptime N_HEADS = 4
    comptime HEAD_DIM = 64
    comptime HALF = HEAD_DIM // 2
    comptime THETA: Float32 = 10000.0
    var q = ctx.enqueue_create_buffer[DType.float16](N_TOK * N_HEADS * HEAD_DIM)
    var posbuf = ctx.enqueue_create_buffer[DType.int32](N_TOK)
    with q.map_to_host() as qh, posbuf.map_to_host() as ph:
        for i in range(N_TOK * N_HEADS * HEAD_DIM):
            qh[i] = Float16(_fill(i) * 0.3)
        for t in range(N_TOK):
            ph[t] = Int32(t + 5)
    ctx.enqueue_function[rope_neox_f16](
        q.unsafe_ptr(), posbuf.unsafe_ptr(), N_HEADS, HEAD_DIM, THETA,
        grid_dim=(N_TOK, N_HEADS), block_dim=64,
    )
    ctx.synchronize()

    max_err = 0.0
    with q.map_to_host() as qh:
        for t in range(N_TOK):
            for h in range(N_HEADS):
                base = (t * N_HEADS + h) * HEAD_DIM
                for j in range(HALF):
                    freq = pow(THETA, Float32(-2 * j) / Float32(HEAD_DIM))
                    angle = Float32(t + 5) * freq
                    a = Float32(Float16(_fill(base + j) * 0.3))
                    b = Float32(Float16(_fill(base + HALF + j) * 0.3))
                    e1 = a * cos(angle) - b * sin(angle)
                    e2 = a * sin(angle) + b * cos(angle)
                    err = abs(Float32(qh[base + j]) - e1)
                    if err > max_err:
                        max_err = err
                    err = abs(Float32(qh[base + HALF + j]) - e2)
                    if err > max_err:
                        max_err = err
    print("rope max_err:", max_err)
    if max_err > 0.01:
        raise Error("rope_neox_f16 numeric check FAILED")

    # --- gemv q8_0 ---
    comptime ROWS_G = 16
    comptime COLS_G = 256
    comptime BLOCKS_PER_ROW = COLS_G // 32
    comptime WBYTES = ROWS_G * BLOCKS_PER_ROW * 34
    var wq = ctx.enqueue_create_buffer[DType.uint8](WBYTES)
    var xv = ctx.enqueue_create_buffer[DType.float16](COLS_G)
    var yv = ctx.enqueue_create_buffer[DType.float16](ROWS_G)
    var expected_y = List[Float32]()
    with wq.map_to_host() as wh, xv.map_to_host() as xh:
        for c in range(COLS_G):
            xh[c] = Float16(_fill(c) * 0.1)
        for r in range(ROWS_G):
            var acc: Float32 = 0.0
            for bl in range(BLOCKS_PER_ROW):
                off = (r * BLOCKS_PER_ROW + bl) * 34
                scale = Float16(0.02 + Float32((r + bl) % 7) * 0.01)
                # Scale is stored little-endian f16 at the block head.
                bits = scale.to_bits()
                wh[off] = UInt8(bits & 0xFF)
                wh[off + 1] = UInt8((bits >> 8) & 0xFF)
                var s: Float32 = 0.0
                for k in range(32):
                    qv = Int(((r * 31 + bl * 17 + k * 13) % 255)) - 127
                    wh[off + 2 + k] = UInt8(qv & 0xFF)
                    s += Float32(qv) * Float32(Float16(_fill(bl * 32 + k) * 0.1))
                acc += Float32(scale) * s
            expected_y.append(acc)
    ctx.enqueue_function[gemv_q8_0_f16](
        yv.unsafe_ptr(), wq.unsafe_ptr(), xv.unsafe_ptr(), COLS_G,
        grid_dim=ROWS_G, block_dim=256,
    )
    ctx.synchronize()

    max_err = 0.0
    with yv.map_to_host() as yh:
        for r in range(ROWS_G):
            err = abs(Float32(yh[r]) - expected_y[r])
            rel = err / (abs(expected_y[r]) + 1.0)
            if rel > max_err:
                max_err = rel
    print("gemv_q8_0 max_rel_err:", max_err)
    if max_err > 0.02:
        raise Error("gemv_q8_0_f16 numeric check FAILED")

    # --- gemv q4_k (v2, warp-per-row) ---
    comptime ROWS_K = 17  # odd row count exercises the row guard
    comptime COLS_K = 512  # two 256-element superblocks per row
    comptime KBLOCKS = COLS_K // 256
    var wk4 = ctx.enqueue_create_buffer[DType.uint8](ROWS_K * KBLOCKS * 144)
    var xk = ctx.enqueue_create_buffer[DType.float16](COLS_K)
    var yk = ctx.enqueue_create_buffer[DType.float16](ROWS_K)
    var yk32 = ctx.enqueue_create_buffer[DType.float32](ROWS_K)
    var expected_k = List[Float32]()
    with wk4.map_to_host() as wh, xk.map_to_host() as xh:
        for c in range(COLS_K):
            xh[c] = Float16(_fill(c) * 0.1)
        for r in range(ROWS_K):
            var acc: Float32 = 0.0
            for bl in range(KBLOCKS):
                off = (r * KBLOCKS + bl) * 144
                d = Float16(0.008 + Float32((r + bl) % 7) * 0.004)
                dmin = Float16(0.005 + Float32((r + 2 * bl) % 5) * 0.003)
                bits = d.to_bits()
                wh[off] = UInt8(bits & 0xFF)
                wh[off + 1] = UInt8((bits >> 8) & 0xFF)
                bits = dmin.to_bits()
                wh[off + 2] = UInt8(bits & 0xFF)
                wh[off + 3] = UInt8((bits >> 8) & 0xFF)
                # Arbitrary scale bytes exercise both get_scale_min_k4
                # branches including the high 2 bits.
                for i in range(12):
                    wh[off + 4 + i] = UInt8((r * 53 + bl * 19 + i * 41 + 7) % 256)
                for i in range(128):
                    wh[off + 16 + i] = UInt8((r * 31 + bl * 17 + i * 13) % 256)
                # CPU reference (dequant.rs dq_q4_k semantics).
                for j64 in range(4):
                    j1 = 2 * j64
                    j2 = 2 * j64 + 1
                    sc1, mn1 = _gsm_ref(
                        Int(wh[off + j1]) if j1 >= 4 else 0,
                        Int(wh[off + 4 + j1]),
                        Int(wh[off + 8 + j1]),
                        j1,
                    )
                    sc2, mn2 = _gsm_ref(
                        Int(wh[off + j2]) if j2 >= 4 else 0,
                        Int(wh[off + 4 + j2]),
                        Int(wh[off + 8 + j2]),
                        j2,
                    )
                    for i in range(32):
                        qb = Int(wh[off + 16 + j64 * 32 + i])
                        col_lo = bl * 256 + j64 * 64 + i
                        col_hi = col_lo + 32
                        xlo = Float32(Float16(_fill(col_lo) * 0.1))
                        xhi = Float32(Float16(_fill(col_hi) * 0.1))
                        acc += (Float32(d) * sc1 * Float32(qb & 0x0F) - Float32(dmin) * mn1) * xlo
                        acc += (Float32(d) * sc2 * Float32(qb >> 4) - Float32(dmin) * mn2) * xhi
            expected_k.append(acc)
    ctx.enqueue_function[gemv_q4_k_f16_v2](
        yk.unsafe_ptr(), wk4.unsafe_ptr(), xk.unsafe_ptr(), COLS_K, ROWS_K,
        grid_dim=(ROWS_K + 7) // 8, block_dim=256,
    )
    ctx.enqueue_function[gemv_q4_k_out_f32_v2](
        yk32.unsafe_ptr(), wk4.unsafe_ptr(), xk.unsafe_ptr(), COLS_K, ROWS_K,
        grid_dim=(ROWS_K + 7) // 8, block_dim=256,
    )
    ctx.synchronize()

    max_err = 0.0
    with yk.map_to_host() as yh:
        for r in range(ROWS_K):
            err = abs(Float32(yh[r]) - expected_k[r])
            rel = err / (abs(expected_k[r]) + 1.0)
            if rel > max_err:
                max_err = rel
    print("gemv_q4_k max_rel_err:", max_err)
    if max_err > 0.02:
        raise Error("gemv_q4_k_f16_v2 numeric check FAILED")

    max_err = 0.0
    with yk32.map_to_host() as yh:
        for r in range(ROWS_K):
            err = abs(yh[r] - expected_k[r])
            rel = err / (abs(expected_k[r]) + 1.0)
            if rel > max_err:
                max_err = rel
    print("gemv_q4_k_out_f32 max_rel_err:", max_err)
    if max_err > 0.02:
        raise Error("gemv_q4_k_out_f32_v2 numeric check FAILED")

    # --- gemv f16 ---
    var wf = ctx.enqueue_create_buffer[DType.float16](ROWS_G * COLS_G)
    var yf = ctx.enqueue_create_buffer[DType.float16](ROWS_G)
    var expected_f = List[Float32]()
    with wf.map_to_host() as wh, xv.map_to_host() as xh:
        for r in range(ROWS_G):
            var acc: Float32 = 0.0
            for c in range(COLS_G):
                wv = Float16(_fill(r * COLS_G + c) * 0.05)
                wh[r * COLS_G + c] = wv
                acc += Float32(wv) * Float32(xh[c])
            expected_f.append(acc)
    ctx.enqueue_function[gemv_f16](
        yf.unsafe_ptr(), wf.unsafe_ptr(), xv.unsafe_ptr(), COLS_G,
        grid_dim=ROWS_G, block_dim=256,
    )
    ctx.synchronize()

    max_err = 0.0
    with yf.map_to_host() as yh:
        for r in range(ROWS_G):
            err = abs(Float32(yh[r]) - expected_f[r])
            rel = err / (abs(expected_f[r]) + 1.0)
            if rel > max_err:
                max_err = rel
    print("gemv_f16 max_rel_err:", max_err)
    if max_err > 0.02:
        raise Error("gemv_f16 numeric check FAILED")

    # --- paged flash-decode attention (GQA, non-contiguous pages) ---
    comptime N_SEQS = 2
    comptime NQH = 4
    comptime NKVH = 2
    comptime HD = 64
    comptime PAGE = 16
    comptime MAXP = 4
    comptime NPAGES = 4
    comptime SCALE: Float32 = 0.125

    var qa = ctx.enqueue_create_buffer[DType.float16](N_SEQS * NQH * HD)
    var oa = ctx.enqueue_create_buffer[DType.float16](N_SEQS * NQH * HD)
    var kc = ctx.enqueue_create_buffer[DType.float16](NPAGES * NKVH * PAGE * HD)
    var vc = ctx.enqueue_create_buffer[DType.float16](NPAGES * NKVH * PAGE * HD)
    var pt = ctx.enqueue_create_buffer[DType.int32](N_SEQS * MAXP)
    var sl = ctx.enqueue_create_buffer[DType.int32](N_SEQS)

    # seq0: 5 tokens on page 3; seq1: 23 tokens on pages 1,0 — deliberately
    # scattered to exercise the page indirection.
    with pt.map_to_host() as pth, sl.map_to_host() as slh:
        for i in range(N_SEQS * MAXP):
            pth[i] = Int32(-1)
        pth[0] = 3
        pth[MAXP + 0] = 1
        pth[MAXP + 1] = 0
        slh[0] = 5
        slh[1] = 23

    with qa.map_to_host() as qh, kc.map_to_host() as kh, vc.map_to_host() as vh:
        for i in range(N_SEQS * NQH * HD):
            qh[i] = Float16(_fill(i) * 0.2)
        for i in range(NPAGES * NKVH * PAGE * HD):
            kh[i] = Float16(_fill(i + 3) * 0.2)
            vh[i] = Float16(_fill(i + 11) * 0.2)

    ctx.enqueue_function[attn_decode_f16_hd64](
        oa.unsafe_ptr(), qa.unsafe_ptr(), kc.unsafe_ptr(), vc.unsafe_ptr(),
        pt.unsafe_ptr(), sl.unsafe_ptr(), NQH, NKVH, PAGE, MAXP, SCALE,
        grid_dim=(N_SEQS, NQH), block_dim=128,
    )
    ctx.synchronize()

    # CPU reference with the same f16-rounded inputs.
    max_err = 0.0
    with oa.map_to_host() as oh:
        for s in range(N_SEQS):
            ctx_len = 5 if s == 0 else 23
            for h in range(NQH):
                kvh = h // (NQH // NKVH)
                q_base = (s * NQH + h) * HD
                var m_star: Float32 = -1e30
                var scores = List[Float32]()
                for p_i in range(ctx_len):
                    pg = 3 if s == 0 else (1 if p_i < PAGE else 0)
                    kv_base = ((pg * NKVH + kvh) * PAGE + (p_i % PAGE)) * HD
                    var dot: Float32 = 0.0
                    for e in range(HD):
                        qf = Float32(Float16(_fill(q_base + e) * 0.2))
                        kf = Float32(Float16(_fill(kv_base + e + 3) * 0.2))
                        dot += qf * kf
                    sc = dot * SCALE
                    scores.append(sc)
                    if sc > m_star:
                        m_star = sc
                var denom: Float32 = 0.0
                for p_i in range(ctx_len):
                    denom += exp(scores[p_i] - m_star)
                for e in range(HD):
                    var num: Float32 = 0.0
                    for p_i in range(ctx_len):
                        pg = 3 if s == 0 else (1 if p_i < PAGE else 0)
                        kv_base = ((pg * NKVH + kvh) * PAGE + (p_i % PAGE)) * HD
                        vf = Float32(Float16(_fill(kv_base + e + 11) * 0.2))
                        num += exp(scores[p_i] - m_star) * vf
                    expected = num / denom
                    err = abs(Float32(oh[q_base + e]) - expected)
                    if err > max_err:
                        max_err = err
    print("attn_decode max_err:", max_err)
    if max_err > 0.01:
        raise Error("attn_decode_f16 numeric check FAILED")

    # --- fused qkv_post vs the separate-kernel reference (bit-exact) ---
    # Reference = rmsnorm_f16(q), rmsnorm_f16(k), rope(q), rope(k),
    # kv_append_f16; fused = one qkv_post_f16 launch. Both norm variants.
    comptime PQH = 4
    comptime PKVH = 2
    comptime PHD = 128
    comptime PPAGE = 16
    comptime PNPAGES = 3
    comptime PEPS: Float32 = 1e-6
    comptime PTHETA: Float32 = 1000000.0
    comptime PSEQ = 21  # current token at position 20 -> page_table[1]

    var pq_in = ctx.enqueue_create_buffer[DType.float16](PQH * PHD)
    var pk_in = ctx.enqueue_create_buffer[DType.float16](PKVH * PHD)
    var pq_ref = ctx.enqueue_create_buffer[DType.float16](PQH * PHD)
    var pk_ref = ctx.enqueue_create_buffer[DType.float16](PKVH * PHD)
    var pv_ref = ctx.enqueue_create_buffer[DType.float16](PKVH * PHD)
    var pq_fus = ctx.enqueue_create_buffer[DType.float16](PQH * PHD)
    var pk_fus = ctx.enqueue_create_buffer[DType.float16](PKVH * PHD)
    var pv_fus = ctx.enqueue_create_buffer[DType.float16](PKVH * PHD)
    var pqw = ctx.enqueue_create_buffer[DType.float16](PHD)
    var pkw = ctx.enqueue_create_buffer[DType.float16](PHD)
    var pkc_ref = ctx.enqueue_create_buffer[DType.float16](PNPAGES * PKVH * PPAGE * PHD)
    var pvc_ref = ctx.enqueue_create_buffer[DType.float16](PNPAGES * PKVH * PPAGE * PHD)
    var pkc_fus = ctx.enqueue_create_buffer[DType.float16](PNPAGES * PKVH * PPAGE * PHD)
    var pvc_fus = ctx.enqueue_create_buffer[DType.float16](PNPAGES * PKVH * PPAGE * PHD)
    var ppt = ctx.enqueue_create_buffer[DType.int32](PNPAGES)
    var pslen = ctx.enqueue_create_buffer[DType.int32](1)
    var ppos = ctx.enqueue_create_buffer[DType.int32](1)

    with ppt.map_to_host() as h:
        h[0] = 2
        h[1] = 1
        h[2] = 0
    with pslen.map_to_host() as h:
        h[0] = Int32(PSEQ)
    with ppos.map_to_host() as h:
        h[0] = Int32(PSEQ - 1)
    with pqw.map_to_host() as qwh, pkw.map_to_host() as kwh:
        for i in range(PHD):
            qwh[i] = Float16(0.9 + Float32(i % 5) * 0.06)
            kwh[i] = Float16(1.1 - Float32(i % 7) * 0.04)

    for norm_case in range(2):
        with pq_in.map_to_host() as src, pq_ref.map_to_host() as a, pq_fus.map_to_host() as bq:
            for i in range(PQH * PHD):
                src[i] = Float16(_fill(i) * 0.3)
                a[i] = src[i]
                bq[i] = src[i]
        with pk_in.map_to_host() as src, pk_ref.map_to_host() as a, pk_fus.map_to_host() as bk:
            for i in range(PKVH * PHD):
                src[i] = Float16(_fill(i + 5) * 0.3)
                a[i] = src[i]
                bk[i] = src[i]
        with pv_ref.map_to_host() as a, pv_fus.map_to_host() as bv:
            for i in range(PKVH * PHD):
                a[i] = Float16(_fill(i + 9) * 0.3)
                bv[i] = a[i]
        with pkc_ref.map_to_host() as a, pkc_fus.map_to_host() as bc:
            for i in range(PNPAGES * PKVH * PPAGE * PHD):
                a[i] = Float16(0.0)
                bc[i] = Float16(0.0)
        with pvc_ref.map_to_host() as a, pvc_fus.map_to_host() as bc:
            for i in range(PNPAGES * PKVH * PPAGE * PHD):
                a[i] = Float16(0.0)
                bc[i] = Float16(0.0)

        # Reference chain (the exact launch geometry launchers.rs uses).
        if norm_case == 1:
            ctx.enqueue_function[rmsnorm_f16](
                pq_ref.unsafe_ptr(), pq_in.unsafe_ptr(), pqw.unsafe_ptr(), PHD, PEPS,
                grid_dim=PQH, block_dim=256,
            )
            ctx.enqueue_function[rmsnorm_f16](
                pk_ref.unsafe_ptr(), pk_in.unsafe_ptr(), pkw.unsafe_ptr(), PHD, PEPS,
                grid_dim=PKVH, block_dim=256,
            )
        ctx.enqueue_function[rope_neox_f16](
            pq_ref.unsafe_ptr(), ppos.unsafe_ptr(), PQH, PHD, PTHETA,
            grid_dim=(1, PQH), block_dim=PHD // 2,
        )
        ctx.enqueue_function[rope_neox_f16](
            pk_ref.unsafe_ptr(), ppos.unsafe_ptr(), PKVH, PHD, PTHETA,
            grid_dim=(1, PKVH), block_dim=PHD // 2,
        )
        ctx.enqueue_function[kv_append_f16](
            pkc_ref.unsafe_ptr(), pvc_ref.unsafe_ptr(),
            pk_ref.unsafe_ptr(), pv_ref.unsafe_ptr(),
            ppt.unsafe_ptr(), pslen.unsafe_ptr(), PKVH, PPAGE, PHD,
            grid_dim=PKVH, block_dim=PHD,
        )

        # Fused launch.
        ctx.enqueue_function[qkv_post_f16](
            pq_fus.unsafe_ptr(), pk_fus.unsafe_ptr(), pv_fus.unsafe_ptr(),
            pqw.unsafe_ptr(), pkw.unsafe_ptr(),
            pkc_fus.unsafe_ptr(), pvc_fus.unsafe_ptr(),
            ppos.unsafe_ptr(), ppt.unsafe_ptr(), pslen.unsafe_ptr(),
            PQH, PKVH, PHD, PPAGE, norm_case, norm_case, PEPS, PTHETA,
            grid_dim=PQH + PKVH, block_dim=PHD,
        )
        ctx.synchronize()

        max_err = 0.0
        with pq_ref.map_to_host() as a, pq_fus.map_to_host() as bq:
            for i in range(PQH * PHD):
                err = abs(Float32(a[i]) - Float32(bq[i]))
                if err > max_err:
                    max_err = err
        with pkc_ref.map_to_host() as a, pkc_fus.map_to_host() as bc:
            for i in range(PNPAGES * PKVH * PPAGE * PHD):
                err = abs(Float32(a[i]) - Float32(bc[i]))
                if err > max_err:
                    max_err = err
        with pvc_ref.map_to_host() as a, pvc_fus.map_to_host() as bc:
            for i in range(PNPAGES * PKVH * PPAGE * PHD):
                err = abs(Float32(a[i]) - Float32(bc[i]))
                if err > max_err:
                    max_err = err
        print("qkv_post norm =", norm_case, "max_err vs separate kernels:", max_err)
        if max_err > 0.0:
            raise Error("qkv_post_f16 is not bit-exact vs the separate kernels")

    print("ALL KERNEL CHECKS PASSED")
