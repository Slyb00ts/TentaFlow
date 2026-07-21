# ===== File: test_kv_fp8.mojo — numeric gates for the FP8-E4M3 KV cache =====
# Gate 1: kv_append_batch_fp8 stores exactly the e4m3 codes a CPU
#         round-to-nearest-even satfinite reference produces.
# Gate 2: attn_prefill_fp8 on an FP8 cache is BIT-EXACT (max_err 0.0) vs
#         attn_prefill_f16 on the dequantized-to-f16 copy of that cache
#         (e4m3 ⊂ f16, so dequantization is exact).
# Gate 3a: attn_decode_split_fp8's fused-append prologue writes exactly the
#         e4m3 codes the CPU oracle produces for the f16 values the f16
#         split kernel appends from the same inputs (same norm+rope
#         dataflow, only the final cast differs).
# Gate 3b: attn_decode_split_fp8 (+ f16 combine) is BIT-EXACT (max_err 0.0)
#         vs the f16 split kernel when both read identical effective caches
#         (fp8 cache dequantized to f16; the new token's append made
#         lossless via e4m3-exact inputs at rope angle 0) — proving the
#         in-kernel dequant + attention loop adds zero extra error.

from std.gpu.host import DeviceContext
from src.prefill import kv_append_batch_f16, kv_append_batch_fp8
from src.prefill import attn_prefill_f16_hd64, attn_prefill_fp8_hd64
from src.attention import attn_decode_split_f16_hd64, attn_decode_split_fp8_hd64
from src.attention import attn_decode_combine_f16_hd64


def _fill(i: Int) -> Float32:
    # Spread across e4m3's dynamic range: normals, denormals, saturation.
    m = i % 11
    v = Float32((i * 37 % 19) - 9) * 0.25
    if m == 0:
        return v * 100.0  # large magnitude (up to ~450, hits satfinite)
    if m == 1:
        return v * 0.001  # denormal / underflow-to-zero territory
    return v


def _e4m3_ref_decode(b: Int) -> Float32:
    # CPU oracle: decode one e4m3 (fn) code to f32.
    var sign: Float32 = 1.0
    if b >= 128:
        sign = -1.0
    e = (b >> 3) & 0x0F
    man = Float32(b & 0x07)
    if (b & 0x7F) == 0x7F:
        return sign * 448.0  # NaN code — never produced by satfinite encode
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


def _e4m3_ref_encode(x: Float32) -> Int:
    # CPU oracle: round-to-nearest-even, saturate-to-finite e4m3 encode.
    var sign = 0
    var v = x
    if v < 0.0 or (v == 0.0 and (x != 0.0 or (1.0 / x) < 0.0)):
        sign = 128
        v = -v
    if v > 448.0:
        return sign | 0x7E  # satfinite: clamp to ±448 (code 0x7E)
    var best = 0
    var best_err = v  # distance to +0
    var best_mag: Float32 = 0.0
    for code in range(0x7F):  # skip 0x7F (NaN)
        mag = _e4m3_ref_decode(code)
        var err = v - mag
        if err < 0.0:
            err = -err
        better = err < best_err
        # Ties round to even mantissa.
        if err == best_err and (code & 1) == 0 and (best & 1) == 1:
            better = True
        if better:
            best = code
            best_err = err
            best_mag = mag
    _ = best_mag
    return sign | best


def main() raises:
    var ctx = DeviceContext()

    comptime HD = 64
    comptime N_KV = 2
    comptime N_Q = 4
    comptime PAGE = 16
    comptime T = 24
    comptime N_PAGES = 4
    comptime MAX_PAGES = 4

    # ---- Gate 1: batched append encodes like the CPU oracle ----
    kv_elems = N_PAGES * N_KV * PAGE * HD
    in_elems = T * N_KV * HD
    k_in = ctx.enqueue_create_buffer[DType.float16](in_elems)
    v_in = ctx.enqueue_create_buffer[DType.float16](in_elems)
    with k_in.map_to_host() as h:
        for i in range(in_elems):
            h[i] = Float16(_fill(i))
    with v_in.map_to_host() as h:
        for i in range(in_elems):
            h[i] = Float16(_fill(i + 5))
    pt = ctx.enqueue_create_buffer[DType.int32](MAX_PAGES)
    with pt.map_to_host() as h:
        h[0] = 2
        h[1] = 0
        h[2] = 3
        h[3] = 1
    kc8 = ctx.enqueue_create_buffer[DType.float8_e4m3fn](kv_elems)
    vc8 = ctx.enqueue_create_buffer[DType.float8_e4m3fn](kv_elems)
    ctx.enqueue_function[kv_append_batch_fp8](
        kc8.unsafe_ptr(),
        vc8.unsafe_ptr(),
        k_in.unsafe_ptr(),
        v_in.unsafe_ptr(),
        pt.unsafe_ptr(),
        0,
        N_KV,
        PAGE,
        HD,
        grid_dim=(T, N_KV),
        block_dim=HD,
    )
    ctx.synchronize()

    var code_errs = 0
    with kc8.map_to_host() as h:
        for tok in range(T):
            for kvh in range(N_KV):
                page = Int(2) if tok // PAGE == 0 else Int(0)
                dst = ((page * N_KV + kvh) * PAGE + tok % PAGE) * HD
                src = (tok * N_KV + kvh) * HD
                for i in range(HD):
                    got = Int(h[dst + i].to_bits[DType.uint8]())
                    want = _e4m3_ref_encode(Float32(Float16(_fill(src + i))))
                    if got != want:
                        if code_errs < 5:
                            print(
                                "append code mismatch tok",
                                tok,
                                "i",
                                i,
                                "got",
                                got,
                                "want",
                                want,
                            )
                        code_errs += 1
    print("gate1 kv_append_batch_fp8 code mismatches:", code_errs)

    # Dequantize the fp8 cache to f16 for the reference kernels.
    kc16 = ctx.enqueue_create_buffer[DType.float16](kv_elems)
    vc16 = ctx.enqueue_create_buffer[DType.float16](kv_elems)
    with kc8.map_to_host() as h8:
        with kc16.map_to_host() as h16:
            for i in range(kv_elems):
                h16[i] = Float16(Float32(h8[i]))
    with vc8.map_to_host() as h8:
        with vc16.map_to_host() as h16:
            for i in range(kv_elems):
                h16[i] = Float16(Float32(h8[i]))

    # ---- Gate 2: prefill attention fp8 vs f16-on-dequantized, bit-exact ----
    q = ctx.enqueue_create_buffer[DType.float16](T * N_Q * HD)
    with q.map_to_host() as h:
        for i in range(T * N_Q * HD):
            h[i] = Float16(_fill(i + 3) * 0.05)
    out8 = ctx.enqueue_create_buffer[DType.float16](T * N_Q * HD)
    out16 = ctx.enqueue_create_buffer[DType.float16](T * N_Q * HD)
    scale = Float32(0.125)
    ctx.enqueue_function[attn_prefill_fp8_hd64](
        out8.unsafe_ptr(),
        q.unsafe_ptr(),
        kc8.unsafe_ptr(),
        vc8.unsafe_ptr(),
        pt.unsafe_ptr(),
        0,
        N_Q,
        N_KV,
        PAGE,
        scale,
        T,
        grid_dim=((T + 15) // 16, N_Q),
        block_dim=256,
    )
    ctx.enqueue_function[attn_prefill_f16_hd64](
        out16.unsafe_ptr(),
        q.unsafe_ptr(),
        kc16.unsafe_ptr(),
        vc16.unsafe_ptr(),
        pt.unsafe_ptr(),
        0,
        N_Q,
        N_KV,
        PAGE,
        scale,
        T,
        grid_dim=((T + 15) // 16, N_Q),
        block_dim=256,
    )
    ctx.synchronize()
    var max_err2: Float32 = 0.0
    with out8.map_to_host() as h8:
        with out16.map_to_host() as h16:
            for i in range(T * N_Q * HD):
                e = Float32(h8[i]) - Float32(h16[i])
                if e < 0.0:
                    e = -e
                if e > max_err2:
                    max_err2 = e
    print("gate2 attn_prefill fp8-vs-f16(dequant) max_err:", max_err2)

    # ---- Gate 3a: prologue append codes match the oracle ----
    # One new token at position T with norm ON and a real rope angle: the
    # f16 split kernel appends the reference f16 row; the fp8 kernel must
    # store oracle_encode() of exactly those f16 values.
    comptime SPLITS = 4
    seq_lens = ctx.enqueue_create_buffer[DType.int32](1)
    with seq_lens.map_to_host() as h:
        h[0] = T + 1
    positions = ctx.enqueue_create_buffer[DType.int32](1)
    with positions.map_to_host() as h:
        h[0] = T
    q_raw = ctx.enqueue_create_buffer[DType.float16](N_Q * HD)
    k_raw = ctx.enqueue_create_buffer[DType.float16](N_KV * HD)
    v_raw = ctx.enqueue_create_buffer[DType.float16](N_KV * HD)
    with q_raw.map_to_host() as h:
        for i in range(N_Q * HD):
            h[i] = Float16(_fill(i + 11) * 0.1)
    with k_raw.map_to_host() as h:
        for i in range(N_KV * HD):
            h[i] = Float16(_fill(i + 7) * 0.1)
    with v_raw.map_to_host() as h:
        for i in range(N_KV * HD):
            h[i] = Float16(_fill(i + 13))
    norm_w = ctx.enqueue_create_buffer[DType.float16](HD)
    with norm_w.map_to_host() as h:
        for i in range(HD):
            h[i] = Float16(1.0 + Float32(i % 5) * 0.1)
    norm_w2 = ctx.enqueue_create_buffer[DType.float16](HD)
    with norm_w2.map_to_host() as h:
        for i in range(HD):
            h[i] = Float16(1.0 + Float32(i % 7) * 0.05)
    parts_elems = N_Q * SPLITS * (HD + 2)
    parts8 = ctx.enqueue_create_buffer[DType.float32](parts_elems)
    parts16 = ctx.enqueue_create_buffer[DType.float32](parts_elems)
    fin8 = ctx.enqueue_create_buffer[DType.float16](N_Q * HD)
    fin16 = ctx.enqueue_create_buffer[DType.float16](N_Q * HD)
    eps = Float32(1e-6)
    theta = Float32(10000.0)


    # Gate 3a reference: f16 split kernel appends token T's f16 row into the
    # dequantized cache; read it back as the pre-quantization expectation.
    ctx.enqueue_function[attn_decode_split_f16_hd64](
        parts16.unsafe_ptr(),
        q_raw.unsafe_ptr(),
        k_raw.unsafe_ptr(),
        v_raw.unsafe_ptr(),
        norm_w.unsafe_ptr(),
        norm_w2.unsafe_ptr(),
        kc16.unsafe_ptr(),
        vc16.unsafe_ptr(),
        pt.unsafe_ptr(),
        seq_lens.unsafe_ptr(),
        positions.unsafe_ptr(),
        N_Q,
        N_KV,
        PAGE,
        MAX_PAGES,
        SPLITS,
        1,
        1,
        eps,
        theta,
        scale,
        grid_dim=(1, N_Q, SPLITS),
        block_dim=128,
    )
    ctx.enqueue_function[attn_decode_split_fp8_hd64](
        parts8.unsafe_ptr(),
        q_raw.unsafe_ptr(),
        k_raw.unsafe_ptr(),
        v_raw.unsafe_ptr(),
        norm_w.unsafe_ptr(),
        norm_w2.unsafe_ptr(),
        kc8.unsafe_ptr(),
        vc8.unsafe_ptr(),
        pt.unsafe_ptr(),
        seq_lens.unsafe_ptr(),
        positions.unsafe_ptr(),
        N_Q,
        N_KV,
        PAGE,
        MAX_PAGES,
        SPLITS,
        1,
        1,
        eps,
        theta,
        scale,
        grid_dim=(1, N_Q, SPLITS),
        block_dim=128,
    )
    ctx.synchronize()

    pos_t = T
    page_t = 0  # pt[T // PAGE] with T=24, PAGE=16 -> pt[1] = 0
    var append_errs = 0
    with kc8.map_to_host() as h8:
        with kc16.map_to_host() as h16:
            for kvh in range(N_KV):
                dst = ((page_t * N_KV + kvh) * PAGE + pos_t % PAGE) * HD
                for i in range(HD):
                    code = Int(h8[dst + i].to_bits[DType.uint8]())
                    want = _e4m3_ref_encode(Float32(h16[dst + i]))
                    if code != want:
                        if append_errs < 5:
                            print(
                                "prologue k code mismatch kvh",
                                kvh,
                                "i",
                                i,
                                "got",
                                code,
                                "want",
                                want,
                            )
                        append_errs += 1
    with vc8.map_to_host() as h8:
        with vc16.map_to_host() as h16:
            for kvh in range(N_KV):
                dst = ((page_t * N_KV + kvh) * PAGE + pos_t % PAGE) * HD
                for i in range(HD):
                    code = Int(h8[dst + i].to_bits[DType.uint8]())
                    want = _e4m3_ref_encode(Float32(h16[dst + i]))
                    if code != want:
                        append_errs += 1
    print("gate3a prologue append code mismatches:", append_errs)

    # ---- Gate 3b: lossless-append construction, full-path bit-exactness ----
    # Re-dequantize the WHOLE fp8 cache (token T's row becomes the
    # fp8-rounded value in the f16 cache too — both kernels now see
    # identical effective caches). The next token (position T+1) uses rope
    # angle 0 (positions[0] = 0 -> c=1, s=0), no q/k norm, and e4m3-exact
    # raw k/v (multiples of 0.25 within +/-1.75), so BOTH kernels append
    # identical effective rows and any output difference could only come
    # from the in-kernel dequant or attention arithmetic.
    with kc8.map_to_host() as h8:
        with kc16.map_to_host() as h16:
            for i in range(kv_elems):
                h16[i] = Float16(Float32(h8[i]))
    with vc8.map_to_host() as h8:
        with vc16.map_to_host() as h16:
            for i in range(kv_elems):
                h16[i] = Float16(Float32(h8[i]))
    with seq_lens.map_to_host() as h:
        h[0] = T + 2
    with positions.map_to_host() as h:
        h[0] = 0
    k_exact = ctx.enqueue_create_buffer[DType.float16](N_KV * HD)
    v_exact = ctx.enqueue_create_buffer[DType.float16](N_KV * HD)
    with k_exact.map_to_host() as h:
        for i in range(N_KV * HD):
            h[i] = Float16(Float32((i * 5 % 15) - 7) * 0.25)
    with v_exact.map_to_host() as h:
        for i in range(N_KV * HD):
            h[i] = Float16(Float32((i * 11 % 15) - 7) * 0.25)

    ctx.enqueue_function[attn_decode_split_fp8_hd64](
        parts8.unsafe_ptr(),
        q_raw.unsafe_ptr(),
        k_exact.unsafe_ptr(),
        v_exact.unsafe_ptr(),
        norm_w.unsafe_ptr(),
        norm_w2.unsafe_ptr(),
        kc8.unsafe_ptr(),
        vc8.unsafe_ptr(),
        pt.unsafe_ptr(),
        seq_lens.unsafe_ptr(),
        positions.unsafe_ptr(),
        N_Q,
        N_KV,
        PAGE,
        MAX_PAGES,
        SPLITS,
        0,
        0,
        eps,
        theta,
        scale,
        grid_dim=(1, N_Q, SPLITS),
        block_dim=128,
    )
    ctx.enqueue_function[attn_decode_combine_f16_hd64](
        fin8.unsafe_ptr(),
        parts8.unsafe_ptr(),
        N_Q,
        SPLITS,
        grid_dim=(1, N_Q),
        block_dim=32,
    )
    ctx.enqueue_function[attn_decode_split_f16_hd64](
        parts16.unsafe_ptr(),
        q_raw.unsafe_ptr(),
        k_exact.unsafe_ptr(),
        v_exact.unsafe_ptr(),
        norm_w.unsafe_ptr(),
        norm_w2.unsafe_ptr(),
        kc16.unsafe_ptr(),
        vc16.unsafe_ptr(),
        pt.unsafe_ptr(),
        seq_lens.unsafe_ptr(),
        positions.unsafe_ptr(),
        N_Q,
        N_KV,
        PAGE,
        MAX_PAGES,
        SPLITS,
        0,
        0,
        eps,
        theta,
        scale,
        grid_dim=(1, N_Q, SPLITS),
        block_dim=128,
    )
    ctx.enqueue_function[attn_decode_combine_f16_hd64](
        fin16.unsafe_ptr(),
        parts16.unsafe_ptr(),
        N_Q,
        SPLITS,
        grid_dim=(1, N_Q),
        block_dim=32,
    )
    ctx.synchronize()

    var max_err3: Float32 = 0.0
    with fin8.map_to_host() as h8:
        with fin16.map_to_host() as h16:
            for i in range(N_Q * HD):
                e = Float32(h8[i]) - Float32(h16[i])
                if e < 0.0:
                    e = -e
                if e > max_err3:
                    max_err3 = e
    print("gate3b attn_decode_split fp8-vs-f16 max_err:", max_err3)

    if code_errs == 0 and append_errs == 0 and max_err2 == 0.0 and max_err3 == 0.0:
        print("ALL FP8 KV GATES PASS")
    else:
        raise Error("FP8 KV gate failure")
