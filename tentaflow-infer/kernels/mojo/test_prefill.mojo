# ===== File: test_prefill.mojo — numeric checks for GEMM/prefill kernels + micro-bench =====

from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from std.math import exp
from src.gemm import gemm_q8_0_f16, gemm_f16, gemm_nvfp4_f16, gemm_q4_k_f16
from src.gemm import gemm_q8_0_f16_bm64, gemm_f16_bm64, gemm_nvfp4_f16_bm64
from src.gemm import gemm_q4_k_f16_bm64
from src.gemm import gemm_q6_k_f16, gemm_q6_k_f16_bm64
from src.prefill import kv_append_batch_f16, attn_prefill_f16_hd64, QT


def _fill(i: Int) -> Float32:
    return Float32((i * 37 % 19) - 9) * 0.25


def _e2m1_ref(code: Int) -> Float32:
    comptime mags = SIMD[DType.float32, 8](0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0)
    v = mags[code & 7]
    if code >= 8:
        return -v
    return v


def _f8_ref(b: Int) -> Float32:
    var sign: Float32 = 1.0
    if b >= 128:
        sign = -1.0
    e = (b >> 3) & 0x0F
    man = Float32(b & 0x07)
    if e == 0:
        return sign * man * (1.0 / 512.0)
    var scale: Float32 = 1.0
    var k = e - 7
    while k > 0:
        scale *= 2.0
        k -= 1
    while k < 0:
        scale *= 0.5
        k += 1
    return sign * (1.0 + man / 8.0) * scale


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
    var max_err: Float32 = 0.0

    # --- q8_0 GEMM vs CPU ---
    comptime T = 40
    comptime COLS = 128
    comptime ROWS = 24
    var x = ctx.enqueue_create_buffer[DType.float16](T * COLS)
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

    ctx.enqueue_function[gemm_q8_0_f16](
        y.unsafe_ptr(), wq.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS, T,
        grid_dim=((ROWS + 63) // 64, (T + 127) // 128), block_dim=256,
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

    # BM=64 variant must be BIT-identical to BM=128 (same mma chain per element)
    var y64 = ctx.enqueue_create_buffer[DType.float16](T * ROWS)
    ctx.enqueue_function[gemm_q8_0_f16_bm64](
        y64.unsafe_ptr(), wq.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS, T,
        grid_dim=((ROWS + 63) // 64, (T + 63) // 64), block_dim=128,
    )
    ctx.synchronize()
    with y.map_to_host() as ya, y64.map_to_host() as yb:
        for i in range(T * ROWS):
            if ya[i] != yb[i]:
                raise Error("gemm_q8_0 bm64 mismatch")
    print("gemm_q8_0 bm64 bit-identical")

    # --- q4_k GEMM vs CPU (24 rows, 40 tokens, 512 cols = 2 superblocks) ---
    comptime KCOLS = 512
    comptime KBL = KCOLS // 256
    var xk = ctx.enqueue_create_buffer[DType.float16](T * KCOLS)
    var wk = ctx.enqueue_create_buffer[DType.uint8](ROWS * KBL * 144)
    var yk = ctx.enqueue_create_buffer[DType.float16](T * ROWS)
    with xk.map_to_host() as h:
        for i in range(T * KCOLS):
            h[i] = Float16(_fill(i) * 0.1)
    with wk.map_to_host() as h:
        for r in range(ROWS):
            for b in range(KBL):
                off = (r * KBL + b) * 144
                d = Float16(0.008 + Float32((r + b) % 7) * 0.004)
                dmin = Float16(0.005 + Float32((r + 2 * b) % 5) * 0.003)
                bits = d.to_bits()
                h[off] = UInt8(bits & 0xFF)
                h[off + 1] = UInt8((bits >> 8) & 0xFF)
                bits = dmin.to_bits()
                h[off + 2] = UInt8(bits & 0xFF)
                h[off + 3] = UInt8((bits >> 8) & 0xFF)
                for i in range(12):
                    h[off + 4 + i] = UInt8((r * 53 + b * 19 + i * 41 + 7) % 256)
                for i in range(128):
                    h[off + 16 + i] = UInt8((r * 31 + b * 17 + i * 13) % 256)

    ctx.enqueue_function[gemm_q4_k_f16](
        yk.unsafe_ptr(), wk.unsafe_ptr(), xk.unsafe_ptr(), KCOLS, ROWS, T,
        grid_dim=((ROWS + 63) // 64, (T + 127) // 128), block_dim=256,
    )
    ctx.synchronize()

    max_err = 0.0
    with yk.map_to_host() as yh, xk.map_to_host() as xh, wk.map_to_host() as wh:
        for t in range(T):
            for r in range(ROWS):
                var acc: Float32 = 0.0
                for b in range(KBL):
                    off = (r * KBL + b) * 144
                    dref = Float32(Float16(0.008 + Float32((r + b) % 7) * 0.004))
                    mref = Float32(Float16(0.005 + Float32((r + 2 * b) % 5) * 0.003))
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
                            qk = Int(wh[off + 16 + j64 * 32 + i])
                            col_lo = b * 256 + j64 * 64 + i
                            acc += (dref * sc1 * Float32(qk & 0x0F) - mref * mn1) * Float32(
                                xh[t * KCOLS + col_lo]
                            )
                            acc += (dref * sc2 * Float32(qk >> 4) - mref * mn2) * Float32(
                                xh[t * KCOLS + col_lo + 32]
                            )
                rel = abs(Float32(yh[t * ROWS + r]) - acc) / (abs(acc) + 1.0)
                if rel > max_err:
                    max_err = rel
    print("gemm_q4_k max_rel_err:", max_err)
    if max_err > 0.02:
        raise Error("gemm_q4_k FAILED")

    var yk64 = ctx.enqueue_create_buffer[DType.float16](T * ROWS)
    ctx.enqueue_function[gemm_q4_k_f16_bm64](
        yk64.unsafe_ptr(), wk.unsafe_ptr(), xk.unsafe_ptr(), KCOLS, ROWS, T,
        grid_dim=((ROWS + 63) // 64, (T + 63) // 64), block_dim=128,
    )
    ctx.synchronize()
    with yk.map_to_host() as ya, yk64.map_to_host() as yb:
        for i in range(T * ROWS):
            if ya[i] != yb[i]:
                raise Error("gemm_q4_k bm64 mismatch")
    print("gemm_q4_k bm64 bit-identical")

    # --- q6_k GEMM vs CPU (24 rows, 40 tokens, 512 cols = 2 superblocks) ---
    var wk6 = ctx.enqueue_create_buffer[DType.uint8](ROWS * KBL * 210)
    var yk6 = ctx.enqueue_create_buffer[DType.float16](T * ROWS)
    with wk6.map_to_host() as h:
        for r in range(ROWS):
            for b in range(KBL):
                off = (r * KBL + b) * 210
                for i in range(208):
                    h[off + i] = UInt8((r * 37 + b * 23 + i * 11 + 5) % 256)
                d = Float16(0.006 + Float32((r + b) % 7) * 0.003)
                bits = d.to_bits()
                h[off + 208] = UInt8(bits & 0xFF)
                h[off + 209] = UInt8((bits >> 8) & 0xFF)

    ctx.enqueue_function[gemm_q6_k_f16](
        yk6.unsafe_ptr(), wk6.unsafe_ptr(), xk.unsafe_ptr(), KCOLS, ROWS, T,
        grid_dim=((ROWS + 63) // 64, (T + 127) // 128), block_dim=256,
    )
    ctx.synchronize()

    max_err = 0.0
    with yk6.map_to_host() as yh, xk.map_to_host() as xh, wk6.map_to_host() as wh:
        for t in range(T):
            for r in range(ROWS):
                var acc: Float32 = 0.0
                for b in range(KBL):
                    off = (r * KBL + b) * 210
                    dref = Float32(Float16(0.006 + Float32((r + b) % 7) * 0.003))
                    # dq_q6_k reference: two 128-halves of 4 quadrants.
                    for n2 in range(2):
                        for l in range(32):
                            is_ = l // 16
                            ql_l = Int(wh[off + n2 * 64 + l])
                            ql_h = Int(wh[off + n2 * 64 + l + 32])
                            qh_b = Int(wh[off + 128 + n2 * 32 + l])
                            var sc8 = InlineArray[Float32, 8](uninitialized=True)
                            for si in range(8):
                                var sv = Int(wh[off + 192 + n2 * 8 + si])
                                if sv > 127:
                                    sv -= 256
                                sc8[si] = Float32(sv)
                            q1 = Float32(((ql_l & 0x0F) | ((qh_b & 3) << 4)) - 32)
                            q2 = Float32(((ql_h & 0x0F) | (((qh_b >> 2) & 3) << 4)) - 32)
                            q3 = Float32(((ql_l >> 4) | (((qh_b >> 4) & 3) << 4)) - 32)
                            q4 = Float32(((ql_h >> 4) | (((qh_b >> 6) & 3) << 4)) - 32)
                            base_col = t * KCOLS + b * 256 + n2 * 128
                            acc += dref * sc8[is_] * q1 * Float32(xh[base_col + l])
                            acc += dref * sc8[2 + is_] * q2 * Float32(xh[base_col + l + 32])
                            acc += dref * sc8[4 + is_] * q3 * Float32(xh[base_col + l + 64])
                            acc += dref * sc8[6 + is_] * q4 * Float32(xh[base_col + l + 96])
                rel = abs(Float32(yh[t * ROWS + r]) - acc) / (abs(acc) + 1.0)
                if rel > max_err:
                    max_err = rel
    print("gemm_q6_k max_rel_err:", max_err)
    if max_err > 0.02:
        raise Error("gemm_q6_k FAILED")

    var yk6_64 = ctx.enqueue_create_buffer[DType.float16](T * ROWS)
    ctx.enqueue_function[gemm_q6_k_f16_bm64](
        yk6_64.unsafe_ptr(), wk6.unsafe_ptr(), xk.unsafe_ptr(), KCOLS, ROWS, T,
        grid_dim=((ROWS + 63) // 64, (T + 63) // 64), block_dim=128,
    )
    ctx.synchronize()
    with yk6.map_to_host() as ya, yk6_64.map_to_host() as yb:
        for i in range(T * ROWS):
            if ya[i] != yb[i]:
                raise Error("gemm_q6_k bm64 mismatch")
    print("gemm_q6_k bm64 bit-identical")

    # --- f16 GEMM vs CPU (row/token/k tails: 90 rows, 40 tokens, 136 cols) ---
    comptime FROWS = 90
    comptime FCOLS = 136
    var xf = ctx.enqueue_create_buffer[DType.float16](T * FCOLS)
    var wf = ctx.enqueue_create_buffer[DType.float16](FROWS * FCOLS)
    var yf = ctx.enqueue_create_buffer[DType.float16](T * FROWS)
    with xf.map_to_host() as h:
        for i in range(T * FCOLS):
            h[i] = Float16(_fill(i) * 0.1)
    with wf.map_to_host() as h:
        for i in range(FROWS * FCOLS):
            h[i] = Float16(_fill(i + 7) * 0.15)
    ctx.enqueue_function[gemm_f16](
        yf.unsafe_ptr(), wf.unsafe_ptr(), xf.unsafe_ptr(), FCOLS, FROWS, T,
        grid_dim=((FROWS + 63) // 64, (T + 127) // 128), block_dim=256,
    )
    ctx.synchronize()
    max_err = 0.0
    with yf.map_to_host() as yh, xf.map_to_host() as xh, wf.map_to_host() as wh:
        for t in range(T):
            for r in range(FROWS):
                var acc: Float32 = 0.0
                for c in range(FCOLS):
                    acc += Float32(wh[r * FCOLS + c]) * Float32(xh[t * FCOLS + c])
                rel = abs(Float32(yh[t * FROWS + r]) - acc) / (abs(acc) + 1.0)
                if rel > max_err:
                    max_err = rel
    print("gemm_f16 max_rel_err:", max_err)
    if max_err > 0.02:
        raise Error("gemm_f16 FAILED")

    var yf64 = ctx.enqueue_create_buffer[DType.float16](T * FROWS)
    ctx.enqueue_function[gemm_f16_bm64](
        yf64.unsafe_ptr(), wf.unsafe_ptr(), xf.unsafe_ptr(), FCOLS, FROWS, T,
        grid_dim=((FROWS + 63) // 64, (T + 63) // 64), block_dim=128,
    )
    ctx.synchronize()
    with yf.map_to_host() as ya, yf64.map_to_host() as yb:
        for i in range(T * FROWS):
            if ya[i] != yb[i]:
                raise Error("gemm_f16 bm64 mismatch")
    print("gemm_f16 bm64 bit-identical")

    # --- nvfp4 GEMM vs CPU (112 cols exercises the 16-col group tail) ---
    comptime NCOLS = 112
    comptime IGS: Float32 = 0.75
    var xnv = ctx.enqueue_create_buffer[DType.float16](T * NCOLS)
    var npk = ctx.enqueue_create_buffer[DType.uint8](FROWS * NCOLS // 2)
    var nsc = ctx.enqueue_create_buffer[DType.uint8](FROWS * NCOLS // 16)
    var ynv = ctx.enqueue_create_buffer[DType.float16](T * FROWS)
    with xnv.map_to_host() as h:
        for i in range(T * NCOLS):
            h[i] = Float16(_fill(i) * 0.3)
    with npk.map_to_host() as h:
        for i in range(FROWS * NCOLS // 2):
            h[i] = UInt8((i * 73 + 11) % 256)
    with nsc.map_to_host() as h:
        for i in range(FROWS * NCOLS // 16):
            h[i] = UInt8((i * 29 + 40) % 100 + 8)
    ctx.enqueue_function[gemm_nvfp4_f16](
        ynv.unsafe_ptr(), npk.unsafe_ptr(), nsc.unsafe_ptr(), xnv.unsafe_ptr(),
        NCOLS, FROWS, T, IGS,
        grid_dim=((FROWS + 63) // 64, (T + 127) // 128), block_dim=256,
    )
    ctx.synchronize()
    max_err = 0.0
    with ynv.map_to_host() as yh, xnv.map_to_host() as xh, npk.map_to_host() as ph, nsc.map_to_host() as sh:
        for t in range(T):
            for r in range(FROWS):
                var acc: Float32 = 0.0
                for g in range(NCOLS // 16):
                    sref = _f8_ref(Int(sh[r * (NCOLS // 16) + g])) * IGS
                    var dot: Float32 = 0.0
                    for j in range(8):
                        b = Int(ph[r * (NCOLS // 2) + g * 8 + j])
                        dot += _e2m1_ref(b & 0x0F) * Float32(xh[t * NCOLS + g * 16 + 2 * j])
                        dot += _e2m1_ref(b >> 4) * Float32(xh[t * NCOLS + g * 16 + 2 * j + 1])
                    acc += sref * dot
                rel = abs(Float32(yh[t * FROWS + r]) - acc) / (abs(acc) + 1.0)
                if rel > max_err:
                    max_err = rel
    print("gemm_nvfp4 max_rel_err:", max_err)
    if max_err > 0.02:
        raise Error("gemm_nvfp4 FAILED")

    var yn64 = ctx.enqueue_create_buffer[DType.float16](T * FROWS)
    ctx.enqueue_function[gemm_nvfp4_f16_bm64](
        yn64.unsafe_ptr(), npk.unsafe_ptr(), nsc.unsafe_ptr(), xnv.unsafe_ptr(),
        NCOLS, FROWS, T, IGS,
        grid_dim=((FROWS + 63) // 64, (T + 63) // 64), block_dim=128,
    )
    ctx.synchronize()
    with ynv.map_to_host() as ya, yn64.map_to_host() as yb:
        for i in range(T * FROWS):
            if ya[i] != yb[i]:
                raise Error("gemm_nvfp4 bm64 mismatch")
    print("gemm_nvfp4 bm64 bit-identical")

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
        pt.unsafe_ptr(), BASE, NQH, NKVH, PAGE, SCALE, TT,
        grid_dim=((TT + QT - 1) // QT, NQH), block_dim=256,
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
    var bx = ctx.enqueue_create_buffer[DType.float16](BT * BC)
    var by = ctx.enqueue_create_buffer[DType.float16](BT * BR)
    # Long warmup: the timing below is garbage unless the GPU reaches boost
    # clocks first (idle RTX 4090 sits at ~210 MHz).
    for _ in range(300):
        ctx.enqueue_function[gemm_q8_0_f16](
            by.unsafe_ptr(), bw.unsafe_ptr(), bx.unsafe_ptr(), BC, BR, BT,
            grid_dim=((BR + 63) // 64, (BT + 127) // 128), block_dim=256,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    comptime ITERS = 20
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q8_0_f16](
            by.unsafe_ptr(), bw.unsafe_ptr(), bx.unsafe_ptr(), BC, BR, BT,
            grid_dim=((BR + 63) // 64, (BT + 127) // 128), block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    flops = 2.0 * Float64(BR) * Float64(BC) * Float64(BT)
    print("gemm_q8_0 11264x4096 T=256:", ms, "ms  ", flops / (ms / 1e3) / 1e12, "TFLOP/s")

    var bwf = ctx.enqueue_create_buffer[DType.float16](BR * BC)
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_f16](
            by.unsafe_ptr(), bwf.unsafe_ptr(), bx.unsafe_ptr(), BC, BR, BT,
            grid_dim=((BR + 63) // 64, (BT + 127) // 128), block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    print("gemm_f16 11264x4096 T=256:", ms, "ms  ", flops / (ms / 1e3) / 1e12, "TFLOP/s")

    var bwk = ctx.enqueue_create_buffer[DType.uint8](BR * (BC // 256) * 144)
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q4_k_f16](
            by.unsafe_ptr(), bwk.unsafe_ptr(), bx.unsafe_ptr(), BC, BR, BT,
            grid_dim=((BR + 63) // 64, (BT + 127) // 128), block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    print("gemm_q4_k 11264x4096 T=256:", ms, "ms  ", flops / (ms / 1e3) / 1e12, "TFLOP/s")

    var bpk = ctx.enqueue_create_buffer[DType.uint8](BR * BC // 2)
    var bsc = ctx.enqueue_create_buffer[DType.uint8](BR * BC // 16)
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_nvfp4_f16](
            by.unsafe_ptr(), bpk.unsafe_ptr(), bsc.unsafe_ptr(), bx.unsafe_ptr(),
            BC, BR, BT, Float32(1.0),
            grid_dim=((BR + 63) // 64, (BT + 127) // 128), block_dim=256,
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    ms = Float64(t1 - t0) / 1e6 / ITERS
    print("gemm_nvfp4 11264x4096 T=256:", ms, "ms  ", flops / (ms / 1e3) / 1e12, "TFLOP/s")

    print("ALL PREFILL KERNEL CHECKS PASSED")
