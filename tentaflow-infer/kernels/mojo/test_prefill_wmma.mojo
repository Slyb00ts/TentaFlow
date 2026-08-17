# Test zloty flash attention prefillu na WMMA: porownanie z referencja liczona
# na hoscie w f32. Kryterium jest zgodnosc, dopiero potem czas.
from std.gpu.host import DeviceContext
from std.math import exp, sqrt
from src.prefill_wmma import attn_prefill_wmma_hd128

comptime HD: Int = 128
comptime NEG: Float32 = -3.0e38


def rnd(seed: Int, i: Int) -> Float32:
    x = (seed * 1103515245 + i * 12345 + 7919) % 65536
    return (Float32(x) / 65536.0 - 0.5) * 0.5


def run(ctx: DeviceContext, n_tokens: Int, n_q_heads: Int, n_kv_heads: Int) raises:
    page_size = 256
    n_pages = (n_tokens + page_size - 1) // page_size
    kv_elems = n_pages * n_kv_heads * page_size * HD

    var qh_ = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_q_heads * HD)
    var kh_ = ctx.enqueue_create_host_buffer[DType.float16](kv_elems)
    var vh_ = ctx.enqueue_create_host_buffer[DType.float16](kv_elems)
    var ph_ = ctx.enqueue_create_host_buffer[DType.int32](n_pages)
    ctx.synchronize()

    for i in range(n_tokens * n_q_heads * HD):
        qh_[i] = rnd(1, i).cast[DType.float16]()
    for i in range(kv_elems):
        kh_[i] = rnd(2, i).cast[DType.float16]()
        vh_[i] = rnd(3, i).cast[DType.float16]()
    for i in range(n_pages):
        ph_[i] = Int32(i)

    var qd = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_q_heads * HD)
    var kd = ctx.enqueue_create_buffer[DType.float16](kv_elems)
    var vd = ctx.enqueue_create_buffer[DType.float16](kv_elems)
    var pd = ctx.enqueue_create_buffer[DType.int32](n_pages)
    var od = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_q_heads * HD)
    ctx.enqueue_copy(qd, qh_)
    ctx.enqueue_copy(kd, kh_)
    ctx.enqueue_copy(vd, vh_)
    ctx.enqueue_copy(pd, ph_)
    ctx.synchronize()

    scale = 1.0 / sqrt(Float32(HD))
    ctx.enqueue_function[attn_prefill_wmma_hd128](
        od.unsafe_ptr(), qd.unsafe_ptr(), kd.unsafe_ptr(), vd.unsafe_ptr(),
        pd.unsafe_ptr(), 0, n_q_heads, n_kv_heads, page_size, scale, n_tokens,
        grid_dim=((n_tokens + 63) // 64, n_q_heads), block_dim=128,
    )
    ctx.synchronize()

    var oh_ = ctx.enqueue_create_host_buffer[DType.float16](
        n_tokens * n_q_heads * HD
    )
    ctx.enqueue_copy(oh_, od)
    ctx.synchronize()

    var worst: Float32 = 0.0
    var ref_at: Float32 = 0.0
    for t in range(n_tokens):
        for h in range(n_q_heads):
            kvh = h * n_kv_heads // n_q_heads
            var mx: Float32 = NEG
            for p in range(t + 1):
                var d: Float32 = 0.0
                for c in range(HD):
                    kv = ((0 * n_kv_heads + kvh) * page_size + p) * HD + c
                    d += Float32(qh_[(t * n_q_heads + h) * HD + c]) * Float32(
                        kh_[kv]
                    )
                d *= scale
                if d > mx:
                    mx = d
            var den: Float32 = 0.0
            var acc = InlineArray[Float32, HD](fill=0.0)
            for p in range(t + 1):
                var d: Float32 = 0.0
                for c in range(HD):
                    kv = ((0 * n_kv_heads + kvh) * page_size + p) * HD + c
                    d += Float32(qh_[(t * n_q_heads + h) * HD + c]) * Float32(
                        kh_[kv]
                    )
                w = exp(d * scale - mx)
                den += w
                for c in range(HD):
                    kv = ((0 * n_kv_heads + kvh) * page_size + p) * HD + c
                    acc[c] += w * Float32(vh_[kv])
            for c in range(HD):
                got = Float32(oh_[(t * n_q_heads + h) * HD + c])
                want = acc[c] / den
                diff = abs(got - want)
                if diff > worst:
                    worst = diff
                    ref_at = want
    print(
        "T=", n_tokens, "Hq=", n_q_heads, "Hkv=", n_kv_heads,
        "| najgorsza roznica", worst, "przy referencji", ref_at,
    )
    if worst > 0.01:
        raise Error("flash attention WMMA rozjezdza sie z referencja")


def main() raises:
    var ctx = DeviceContext()
    run(ctx, 64, 2, 2)
    run(ctx, 96, 4, 2)
    run(ctx, 160, 2, 1)
    print("flash attention WMMA: wszystkie ksztalty zgodne z referencja")
