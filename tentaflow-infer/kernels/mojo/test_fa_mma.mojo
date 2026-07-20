# ===== File: test_fa_mma.mojo — GPU-vs-CPU golden for the tensor-core FA prefill =====
# Validates attn_prefill_fa_mma (f16 mma QK^T + online softmax + f16 mma P·V)
# against an f32 CPU reference over the paged cache, exercising GQA, causal
# masking, tile tails (BK=32) and the BQ=64 query-block boundary. Also compares
# against the committed scalar attn_prefill on the SAME inputs.

from std.gpu.host import DeviceContext
from std.math import exp, sqrt
from src.prefill import (
    kv_append_batch_f16,
    attn_prefill_f16_hd64,
    attn_prefill_f16_hd128,
    attn_prefill_fa_f16_hd64,
    attn_prefill_fa_f16_hd128,
    QT,
    BQ,
)


def _fill(i: Int) -> Float32:
    return Float32((i * 37 % 19) - 9) * 0.25


def _check[
    HD: Int
](ctx: DeviceContext, NKVH: Int, NQH: Int, PAGE: Int, BASE: Int, TT: Int) raises:
    NPAGES = (BASE + TT + PAGE - 1) // PAGE + 1
    var kc = ctx.enqueue_create_buffer[DType.float16](NPAGES * NKVH * PAGE * HD)
    var vc = ctx.enqueue_create_buffer[DType.float16](NPAGES * NKVH * PAGE * HD)
    var pt = ctx.enqueue_create_buffer[DType.int32](NPAGES)
    var kin = ctx.enqueue_create_buffer[DType.float16](TT * NKVH * HD)
    var vin = ctx.enqueue_create_buffer[DType.float16](TT * NKVH * HD)
    var qb = ctx.enqueue_create_buffer[DType.float16](TT * NQH * HD)
    var ofa = ctx.enqueue_create_buffer[DType.float16](TT * NQH * HD)
    var osc = ctx.enqueue_create_buffer[DType.float16](TT * NQH * HD)

    with pt.map_to_host() as h:
        for i in range(NPAGES):
            h[i] = Int32(NPAGES - 1 - i)  # scattered pages
    with kc.map_to_host() as kh, vc.map_to_host() as vh:
        for i in range(NPAGES * NKVH * PAGE * HD):
            kh[i] = Float16(0.0)
            vh[i] = Float16(0.0)
        for pos in range(BASE):
            page = NPAGES - 1 - (pos // PAGE)
            slot = pos % PAGE
            for kvh_i in range(NKVH):
                base_off = ((page * NKVH + kvh_i) * PAGE + slot) * HD
                for ei in range(HD):
                    kh[base_off + ei] = Float16(
                        _fill(pos * 1000 + kvh_i * 100 + ei + 3) * 0.2
                    )
                    vh[base_off + ei] = Float16(
                        _fill(pos * 1000 + kvh_i * 100 + ei + 11) * 0.2
                    )
    with kin.map_to_host() as h:
        for t in range(TT):
            for kvh_i in range(NKVH):
                for ei in range(HD):
                    h[(t * NKVH + kvh_i) * HD + ei] = Float16(
                        _fill((BASE + t) * 1000 + kvh_i * 100 + ei + 3) * 0.2
                    )
    with vin.map_to_host() as h:
        for t in range(TT):
            for kvh_i in range(NKVH):
                for ei in range(HD):
                    h[(t * NKVH + kvh_i) * HD + ei] = Float16(
                        _fill((BASE + t) * 1000 + kvh_i * 100 + ei + 11) * 0.2
                    )
    with qb.map_to_host() as h:
        for i in range(TT * NQH * HD):
            h[i] = Float16(_fill(i) * 0.2)

    ctx.enqueue_function[kv_append_batch_f16](
        kc.unsafe_ptr(),
        vc.unsafe_ptr(),
        kin.unsafe_ptr(),
        vin.unsafe_ptr(),
        pt.unsafe_ptr(),
        BASE,
        NKVH,
        PAGE,
        HD,
        grid_dim=(TT, NKVH),
        block_dim=64,
    )
    var SCALE: Float32 = 1.0 / sqrt(Float32(HD))

    comptime if HD == 64:
        ctx.enqueue_function[attn_prefill_fa_f16_hd64](
            ofa.unsafe_ptr(),
            qb.unsafe_ptr(),
            kc.unsafe_ptr(),
            vc.unsafe_ptr(),
            pt.unsafe_ptr(),
            BASE,
            NQH,
            NKVH,
            PAGE,
            SCALE,
            TT,
            grid_dim=((TT + BQ - 1) // BQ, NQH),
            block_dim=128,
        )
        ctx.enqueue_function[attn_prefill_f16_hd64](
            osc.unsafe_ptr(),
            qb.unsafe_ptr(),
            kc.unsafe_ptr(),
            vc.unsafe_ptr(),
            pt.unsafe_ptr(),
            BASE,
            NQH,
            NKVH,
            PAGE,
            SCALE,
            TT,
            grid_dim=((TT + QT - 1) // QT, NQH),
            block_dim=256,
        )
    else:
        ctx.enqueue_function[attn_prefill_fa_f16_hd128](
            ofa.unsafe_ptr(),
            qb.unsafe_ptr(),
            kc.unsafe_ptr(),
            vc.unsafe_ptr(),
            pt.unsafe_ptr(),
            BASE,
            NQH,
            NKVH,
            PAGE,
            SCALE,
            TT,
            grid_dim=((TT + BQ - 1) // BQ, NQH),
            block_dim=128,
        )
        ctx.enqueue_function[attn_prefill_f16_hd128](
            osc.unsafe_ptr(),
            qb.unsafe_ptr(),
            kc.unsafe_ptr(),
            vc.unsafe_ptr(),
            pt.unsafe_ptr(),
            BASE,
            NQH,
            NKVH,
            PAGE,
            SCALE,
            TT,
            grid_dim=((TT + QT - 1) // QT, NQH),
            block_dim=256,
        )
    ctx.synchronize()

    var max_abs_cpu: Float32 = 0.0
    var max_rel_cpu: Float32 = 0.0
    var max_abs_sc: Float32 = 0.0
    with ofa.map_to_host() as oh, osc.map_to_host() as sh:
        for t in range(TT):
            ctx_len = BASE + t + 1
            for hh in range(NQH):
                kvh_i = hh // (NQH // NKVH)
                q_base = (t * NQH + hh) * HD
                var m_star: Float32 = -1e30
                var scores = List[Float32]()
                for pos in range(ctx_len):
                    var dot: Float32 = 0.0
                    for ei in range(HD):
                        qf = Float32(Float16(_fill(q_base + ei) * 0.2))
                        kf = Float32(
                            Float16(_fill(pos * 1000 + kvh_i * 100 + ei + 3) * 0.2)
                        )
                        dot += qf * kf
                    sref = dot * SCALE
                    scores.append(sref)
                    if sref > m_star:
                        m_star = sref
                var denom: Float32 = 0.0
                for pos in range(ctx_len):
                    denom += exp(scores[pos] - m_star)
                for ei in range(HD):
                    var num: Float32 = 0.0
                    for pos in range(ctx_len):
                        vf = Float32(
                            Float16(_fill(pos * 1000 + kvh_i * 100 + ei + 11) * 0.2)
                        )
                        num += exp(scores[pos] - m_star) * vf
                    expected = num / denom
                    got = Float32(oh[q_base + ei])
                    ea = abs(got - expected)
                    if ea > max_abs_cpu:
                        max_abs_cpu = ea
                    # Relative error only for non-negligible outputs (near-zero
                    # attention outputs blow up rel from a f16-ULP abs error).
                    if abs(expected) > 0.05:
                        er = ea / abs(expected)
                        if er > max_rel_cpu:
                            max_rel_cpu = er
                    esc = abs(got - Float32(sh[q_base + ei]))
                    if esc > max_abs_sc:
                        max_abs_sc = esc
    print(
        "  HD",
        HD,
        "NQH",
        NQH,
        "base",
        BASE,
        "T",
        TT,
        "| fa-vs-cpu abs",
        max_abs_cpu,
        "rel",
        max_rel_cpu,
        "| fa-vs-scalar abs",
        max_abs_sc,
    )
    if max_abs_cpu > 0.01 or max_rel_cpu > 0.02:
        raise Error("FA vs CPU golden FAILED")
    if max_abs_sc > 0.02:
        raise Error("FA vs scalar mismatch too large")


def main() raises:
    var ctx = DeviceContext()
    # hd64: GQA 4:2, short prefill (tile tails, single query block)
    _check[64](ctx, NKVH=2, NQH=4, PAGE=16, BASE=5, TT=19)
    # hd128: GQA 16:8, crosses the BQ=64 query-block boundary + long context
    _check[128](ctx, NKVH=8, NQH=16, PAGE=32, BASE=768, TT=200)
    # hd128: no history (pure causal within chunk), non-multiple-of-32 tail
    _check[128](ctx, NKVH=1, NQH=8, PAGE=64, BASE=0, TT=100)
    # hd64: longer, multiple query blocks
    _check[64](ctx, NKVH=4, NQH=8, PAGE=32, BASE=100, TT=140)
    print("ALL FA MMA CHECKS PASSED")
