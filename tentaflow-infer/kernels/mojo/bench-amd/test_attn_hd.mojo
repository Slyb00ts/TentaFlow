# =============================================================================
# Plik: test_attn_hd.mojo
# Opis: Sprawdza prefill uwagi wobec referencji na hoście dla head_dim 256 i 512
#       (Gemma 4 ma obie geometrie). Pilnuje też, że kafel pozycji mniejszy od
#       fali nie psuje wyniku.
# Przykład: pixi run mojo run -I . bench-amd/test_attn_hd.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.math import exp
from src.prefill import attn_prefill
from src.attention import attn_decode_f16

comptime WARP = 32


def check[head_dim: Int, PT: Int](
    ctx: DeviceContext, n_tokens: Int, n_q_heads: Int, n_kv_heads: Int, window: Int
) raises:
    comptime PAGE = 32
    pages = (n_tokens + PAGE - 1) // PAGE
    kv_elems = pages * n_kv_heads * PAGE * head_dim
    var qh = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_q_heads * head_dim)
    var kh = ctx.enqueue_create_host_buffer[DType.float16](kv_elems)
    var vh = ctx.enqueue_create_host_buffer[DType.float16](kv_elems)
    var ph = ctx.enqueue_create_host_buffer[DType.int32](pages)
    ctx.synchronize()
    for i in range(n_tokens * n_q_heads * head_dim):
        qh[i] = Float32(Float32((i * 7) % 23) * 0.03125 - 0.35).cast[DType.float16]()
    for i in range(kv_elems):
        kh[i] = Float32(Float32((i * 11) % 19) * 0.03125 - 0.28).cast[DType.float16]()
        vh[i] = Float32(Float32((i * 5) % 17) * 0.03125 - 0.25).cast[DType.float16]()
    for p in range(pages):
        ph[p] = Int32(p)

    var qd = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_q_heads * head_dim)
    var kd = ctx.enqueue_create_buffer[DType.float16](kv_elems)
    var vd = ctx.enqueue_create_buffer[DType.float16](kv_elems)
    var pd = ctx.enqueue_create_buffer[DType.int32](pages)
    var od = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_q_heads * head_dim)
    ctx.enqueue_copy(qd, qh); ctx.enqueue_copy(kd, kh)
    ctx.enqueue_copy(vd, vh); ctx.enqueue_copy(pd, ph)
    ctx.synchronize()

    scale = Float32(1.0)  # Gemma 4 nie skaluje przez 1/sqrt(head_dim)
    ctx.enqueue_function[attn_prefill[head_dim, DType.float16, PT]](
        od.unsafe_ptr(), qd.unsafe_ptr(), kd.unsafe_ptr(), vd.unsafe_ptr(),
        pd.unsafe_ptr(), 0, n_q_heads, n_kv_heads, PAGE, scale, n_tokens, window,
        grid_dim=((n_tokens + 15) // 16, n_q_heads), block_dim=256,
    )
    ctx.synchronize()

    var worst: Float32 = 0.0
    with od.map_to_host() as got:
        for t in range(n_tokens):
            for h in range(n_q_heads):
                kvh = h // (n_q_heads // n_kv_heads)
                # softmax po pozycjach 0..t (przyczynowo), referencja w f32
                var lo = 0
                if window > 0 and t + 1 > window:
                    lo = t + 1 - window
                var m: Float32 = -3.0e38
                for pos in range(lo, t + 1):
                    var dot: Float32 = 0.0
                    kb = ((pos // PAGE * n_kv_heads + kvh) * PAGE + pos % PAGE) * head_dim
                    for e in range(head_dim):
                        dot += Float32(qh[(t * n_q_heads + h) * head_dim + e]) * Float32(kh[kb + e])
                    sc = dot * scale
                    if sc > m:
                        m = sc
                var denom: Float32 = 0.0
                for pos in range(lo, t + 1):
                    var dot: Float32 = 0.0
                    kb = ((pos // PAGE * n_kv_heads + kvh) * PAGE + pos % PAGE) * head_dim
                    for e in range(head_dim):
                        dot += Float32(qh[(t * n_q_heads + h) * head_dim + e]) * Float32(kh[kb + e])
                    denom += exp(dot * scale - m)
                for e in range(head_dim):
                    var acc: Float32 = 0.0
                    for pos in range(lo, t + 1):
                        var dot: Float32 = 0.0
                        kb = ((pos // PAGE * n_kv_heads + kvh) * PAGE + pos % PAGE) * head_dim
                        for e2 in range(head_dim):
                            dot += Float32(qh[(t * n_q_heads + h) * head_dim + e2]) * Float32(kh[kb + e2])
                        acc += exp(dot * scale - m) * Float32(vh[kb + e])
                    expect = acc / denom
                    have = Float32(got[(t * n_q_heads + h) * head_dim + e])
                    d = abs(have - expect)
                    if d > worst:
                        worst = d
    print("hd", head_dim, "PT", PT, "T", n_tokens, "okno", window, "blad max:", worst)
    if worst > 0.004:
        raise Error("attn_prefill poza tolerancja f16")


def main() raises:
    var ctx = DeviceContext()
    print("arch:", ctx.arch_name())
    check[256, WARP](ctx, 20, 4, 2, 0)
    check[512, WARP // 2](ctx, 20, 4, 1, 0)
    check[512, WARP // 2](ctx, 40, 2, 1, 0)
    print("--- z oknem przesuwnym ---")
    check[256, WARP](ctx, 40, 4, 2, 8)
    check[256, WARP](ctx, 40, 4, 2, 33)
    check[512, WARP // 2](ctx, 40, 2, 1, 8)
    check[512, WARP // 2](ctx, 70, 2, 1, 20)
    print("--- decode ---")
    check_decode[256](ctx, 50, 4, 2, 0)
    check_decode[256](ctx, 50, 4, 2, 16)
    check_decode[512](ctx, 50, 2, 1, 0)
    check_decode[512](ctx, 70, 2, 1, 20)


def check_decode[head_dim: Int](
    ctx: DeviceContext, ctx_len: Int, n_q_heads: Int, n_kv_heads: Int, window: Int
) raises:
    """Decode wobec referencji hosta; okno musi obcinać stare pozycje."""
    comptime PAGE = 32
    pages = (ctx_len + PAGE - 1) // PAGE
    kv_elems = pages * n_kv_heads * PAGE * head_dim
    var qh_ = ctx.enqueue_create_host_buffer[DType.float16](n_q_heads * head_dim)
    var kh = ctx.enqueue_create_host_buffer[DType.float16](kv_elems)
    var vh = ctx.enqueue_create_host_buffer[DType.float16](kv_elems)
    var ph = ctx.enqueue_create_host_buffer[DType.int32](pages)
    var sh = ctx.enqueue_create_host_buffer[DType.int32](1)
    ctx.synchronize()
    for i in range(n_q_heads * head_dim):
        qh_[i] = Float32(Float32((i * 13) % 21) * 0.03125 - 0.3).cast[DType.float16]()
    for i in range(kv_elems):
        kh[i] = Float32(Float32((i * 7) % 23) * 0.03125 - 0.33).cast[DType.float16]()
        vh[i] = Float32(Float32((i * 3) % 15) * 0.03125 - 0.22).cast[DType.float16]()
    for p in range(pages):
        ph[p] = Int32(p)
    sh[0] = Int32(ctx_len)

    var qd = ctx.enqueue_create_buffer[DType.float16](n_q_heads * head_dim)
    var kd = ctx.enqueue_create_buffer[DType.float16](kv_elems)
    var vd = ctx.enqueue_create_buffer[DType.float16](kv_elems)
    var pd = ctx.enqueue_create_buffer[DType.int32](pages)
    var sd = ctx.enqueue_create_buffer[DType.int32](1)
    var od = ctx.enqueue_create_buffer[DType.float16](n_q_heads * head_dim)
    ctx.enqueue_copy(qd, qh_); ctx.enqueue_copy(kd, kh); ctx.enqueue_copy(vd, vh)
    ctx.enqueue_copy(pd, ph); ctx.enqueue_copy(sd, sh)
    ctx.synchronize()

    scale = Float32(1.0)
    ctx.enqueue_function[attn_decode_f16[head_dim]](
        od.unsafe_ptr(), qd.unsafe_ptr(), kd.unsafe_ptr(), vd.unsafe_ptr(),
        pd.unsafe_ptr(), sd.unsafe_ptr(), n_q_heads, n_kv_heads, PAGE, pages,
        scale, window, grid_dim=(1, n_q_heads), block_dim=256,
    )
    ctx.synchronize()

    var lo = 0
    if window > 0 and ctx_len > window:
        lo = ctx_len - window
    var worst: Float32 = 0.0
    with od.map_to_host() as got:
        for h in range(n_q_heads):
            kvh = h // (n_q_heads // n_kv_heads)
            var m: Float32 = -3.0e38
            for pos in range(lo, ctx_len):
                var dot: Float32 = 0.0
                kb = ((pos // PAGE * n_kv_heads + kvh) * PAGE + pos % PAGE) * head_dim
                for e in range(head_dim):
                    dot += Float32(qh_[h * head_dim + e]) * Float32(kh[kb + e])
                if dot * scale > m:
                    m = dot * scale
            var denom: Float32 = 0.0
            for pos in range(lo, ctx_len):
                var dot: Float32 = 0.0
                kb = ((pos // PAGE * n_kv_heads + kvh) * PAGE + pos % PAGE) * head_dim
                for e in range(head_dim):
                    dot += Float32(qh_[h * head_dim + e]) * Float32(kh[kb + e])
                denom += exp(dot * scale - m)
            for e in range(head_dim):
                var acc: Float32 = 0.0
                for pos in range(lo, ctx_len):
                    var dot: Float32 = 0.0
                    kb = ((pos // PAGE * n_kv_heads + kvh) * PAGE + pos % PAGE) * head_dim
                    for e2 in range(head_dim):
                        dot += Float32(qh_[h * head_dim + e2]) * Float32(kh[kb + e2])
                    acc += exp(dot * scale - m) * Float32(vh[kb + e])
                d = abs(Float32(got[h * head_dim + e]) - acc / denom)
                if d > worst:
                    worst = d
    print("decode hd", head_dim, "ctx", ctx_len, "okno", window, "blad max:", worst)
    if worst > 0.004:
        raise Error("attn_decode poza tolerancja f16")
