# ===== File: test_decode_fused.mojo — bit-exactness of the fused decode-layer kernels =====
# Every fused kernel is compared against the exact separate-kernel chain it
# replaces (same launch geometry launchers.rs uses), requiring max_err == 0:
#   gemv_norm_*        vs rmsnorm_residual_f16/rmsnorm_f16 + gemv_*_v2
#   gemv_norm_silu_*   vs rmsnorm_residual_f16 + gemv_*_v2 + silu_mul_f16
#   gemv_residual_*    vs gemv_*_v2 + rmsnorm_residual_f16's residual add
#   rmsnorm_h32_f16    vs rmsnorm_residual_f16's norm output
#   attn_decode_split  vs qkv_post_f16 + attn_decode_f16 (n_splits=1 exact)
# The h/h32 residual pair fed to the fused kernels is reconstructed with the
# same IEEE f32 adds the separate chain performs, so 0.0 is achievable.

from std.gpu.host import DeviceContext
from src.norm import rmsnorm_f16, rmsnorm_residual_f16
from src.activation import silu_mul_f16
from src.gemv2 import gemv_q8_0_f16_v2, gemv_nvfp4_f16_v2, gemv_f16_v2
from src.qkv_post import qkv_post_f16
from src.attention import attn_decode_f16_hd64, attn_decode_f16_hd128
from src.attention import attn_decode_split_f16_hd64, attn_decode_split_f16_hd128
from src.attention import attn_decode_combine_f16_hd64, attn_decode_combine_f16_hd128
from src.decode_fused import gemv_norm_q8_0_f16, gemv_norm_nvfp4_f16, gemv_norm_f16
from src.decode_fused import gemv_norm_silu_q8_0_f16, gemv_norm_silu_nvfp4_f16, gemv_norm_silu_f16
from src.decode_fused import gemv_residual_q8_0_f16, gemv_residual_nvfp4_f16, gemv_residual_f16
from src.decode_fused import rmsnorm_h32_f16

comptime HID = 1024
comptime QROWS = 60  # deliberately not a multiple of 8: exercises the row guard
comptime INTER = 48
comptime EPS: Float32 = 1e-6
comptime Q8_BPR = HID // 32
comptime NV_GROUPS = HID // 16
comptime NV_IGS: Float32 = 0.015


def _fill(i: Int) -> Float32:
    return Float32((i * 37 % 19) - 9) * 0.25


def main() raises:
    var ctx = DeviceContext()

    # ---- shared inputs: residual pair (h f16, h32 f32) + norm weight ----
    var h_prev = ctx.enqueue_create_buffer[DType.float16](HID)
    var delta = ctx.enqueue_create_buffer[DType.float16](HID)
    var nw = ctx.enqueue_create_buffer[DType.float16](HID)
    var resid = ctx.enqueue_create_buffer[DType.float16](HID)
    var h32 = ctx.enqueue_create_buffer[DType.float32](HID)
    var x_ref = ctx.enqueue_create_buffer[DType.float16](HID)
    with h_prev.map_to_host() as hp, delta.map_to_host() as dl, nw.map_to_host() as wh:
        for i in range(HID):
            hp[i] = Float16(_fill(i) * 0.1)
            dl[i] = Float16(_fill(i + 3) * 0.05)
            wh[i] = Float16(1.0 + Float32(i % 5) * 0.1)
    # The separate chain: resid = h_prev; rmsnorm_residual adds delta in f32,
    # stores the f16 residual and the normed x. h32 = the unrounded f32 adds
    # (bit-identical to what the chain computed internally).
    with h_prev.map_to_host() as hp, delta.map_to_host() as dl, resid.map_to_host() as rs, h32.map_to_host() as h3:
        for i in range(HID):
            rs[i] = hp[i]
            h3[i] = Float32(hp[i]) + Float32(dl[i])
    ctx.enqueue_function[rmsnorm_residual_f16](
        x_ref.unsafe_ptr(), resid.unsafe_ptr(), delta.unsafe_ptr(), nw.unsafe_ptr(), HID, EPS,
        grid_dim=1, block_dim=256,
    )
    ctx.synchronize()
    # resid now holds the f16 residual the fused kernels will see as h.

    # ---- weights: q8_0, nvfp4 and f16 for QROWS x HID and 2*INTER x HID ----
    var wq8 = ctx.enqueue_create_buffer[DType.uint8](QROWS * Q8_BPR * 34)
    var wq8_ffn = ctx.enqueue_create_buffer[DType.uint8](2 * INTER * Q8_BPR * 34)
    with wq8.map_to_host() as wh:
        for r in range(QROWS):
            for bl in range(Q8_BPR):
                off = (r * Q8_BPR + bl) * 34
                scale = Float16(0.02 + Float32((r + bl) % 7) * 0.01)
                bits = scale.to_bits()
                wh[off] = UInt8(bits & 0xFF)
                wh[off + 1] = UInt8((bits >> 8) & 0xFF)
                for k in range(32):
                    wh[off + 2 + k] = UInt8(((r * 31 + bl * 17 + k * 13) % 255) & 0xFF)
    with wq8_ffn.map_to_host() as wh:
        for r in range(2 * INTER):
            for bl in range(Q8_BPR):
                off = (r * Q8_BPR + bl) * 34
                scale = Float16(0.02 + Float32((r + bl) % 5) * 0.01)
                bits = scale.to_bits()
                wh[off] = UInt8(bits & 0xFF)
                wh[off + 1] = UInt8((bits >> 8) & 0xFF)
                for k in range(32):
                    wh[off + 2 + k] = UInt8(((r * 23 + bl * 19 + k * 11) % 255) & 0xFF)

    var wnv = ctx.enqueue_create_buffer[DType.uint8](QROWS * HID // 2)
    var wnv_s = ctx.enqueue_create_buffer[DType.uint8](QROWS * NV_GROUPS)
    var wnv_ffn = ctx.enqueue_create_buffer[DType.uint8](2 * INTER * HID // 2)
    var wnv_ffn_s = ctx.enqueue_create_buffer[DType.uint8](2 * INTER * NV_GROUPS)
    with wnv.map_to_host() as p, wnv_s.map_to_host() as s:
        for i in range(QROWS * HID // 2):
            p[i] = UInt8((i * 29) % 256)
        for i in range(QROWS * NV_GROUPS):
            s[i] = UInt8((i * 13) % 256)
    with wnv_ffn.map_to_host() as p, wnv_ffn_s.map_to_host() as s:
        for i in range(2 * INTER * HID // 2):
            p[i] = UInt8((i * 41) % 256)
        for i in range(2 * INTER * NV_GROUPS):
            s[i] = UInt8((i * 7) % 256)

    var wf = ctx.enqueue_create_buffer[DType.float16](QROWS * HID)
    var wf_ffn = ctx.enqueue_create_buffer[DType.float16](2 * INTER * HID)
    with wf.map_to_host() as wh:
        for i in range(QROWS * HID):
            wh[i] = Float16(_fill(i) * 0.05)
    with wf_ffn.map_to_host() as wh:
        for i in range(2 * INTER * HID):
            wh[i] = Float16(_fill(i + 7) * 0.05)

    var y_ref = ctx.enqueue_create_buffer[DType.float16](QROWS)
    var y_fus = ctx.enqueue_create_buffer[DType.float16](QROWS)
    var max_err: Float32 = 0.0

    # ---- gemv_norm_* (both ss sources) ----
    for ss_case in range(2):
        # ss_from_h16=1 references rmsnorm_f16 on the f16 residual instead.
        if ss_case == 1:
            ctx.enqueue_function[rmsnorm_f16](
                x_ref.unsafe_ptr(), resid.unsafe_ptr(), nw.unsafe_ptr(), HID, EPS,
                grid_dim=1, block_dim=256,
            )
            ctx.synchronize()

        for rpw in range(1, 3):
          for fmt in range(3):
            if fmt == 0:
                ctx.enqueue_function[gemv_q8_0_f16_v2](
                    y_ref.unsafe_ptr(), wq8.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                    grid_dim=(QROWS + 7) // 8, block_dim=256,
                )
                ctx.enqueue_function[gemv_norm_q8_0_f16](
                    y_fus.unsafe_ptr(), wq8.unsafe_ptr(), resid.unsafe_ptr(),
                    h32.unsafe_ptr(), nw.unsafe_ptr(), HID, QROWS, ss_case, EPS, rpw,
                    grid_dim=(QROWS + 8 * rpw - 1) // (8 * rpw), block_dim=256,
                )
            elif fmt == 1:
                ctx.enqueue_function[gemv_nvfp4_f16_v2](
                    y_ref.unsafe_ptr(), wnv.unsafe_ptr(), wnv_s.unsafe_ptr(),
                    x_ref.unsafe_ptr(), HID, QROWS, NV_IGS,
                    grid_dim=(QROWS + 7) // 8, block_dim=256,
                )
                ctx.enqueue_function[gemv_norm_nvfp4_f16](
                    y_fus.unsafe_ptr(), wnv.unsafe_ptr(), wnv_s.unsafe_ptr(),
                    resid.unsafe_ptr(), h32.unsafe_ptr(), nw.unsafe_ptr(),
                    HID, QROWS, NV_IGS, ss_case, EPS, rpw,
                    grid_dim=(QROWS + 8 * rpw - 1) // (8 * rpw), block_dim=256,
                )
            else:
                ctx.enqueue_function[gemv_f16_v2](
                    y_ref.unsafe_ptr(), wf.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                    grid_dim=(QROWS + 7) // 8, block_dim=256,
                )
                ctx.enqueue_function[gemv_norm_f16](
                    y_fus.unsafe_ptr(), wf.unsafe_ptr(), resid.unsafe_ptr(),
                    h32.unsafe_ptr(), nw.unsafe_ptr(), HID, QROWS, ss_case, EPS, rpw,
                    grid_dim=(QROWS + 8 * rpw - 1) // (8 * rpw), block_dim=256,
                )
            ctx.synchronize()
            with y_ref.map_to_host() as a, y_fus.map_to_host() as b:
                for i in range(QROWS):
                    err = abs(Float32(a[i]) - Float32(b[i]))
                    if err > max_err:
                        max_err = err
            print("gemv_norm fmt =", fmt, "rpw =", rpw, "ss_from_h16 =", ss_case, "max_err:", max_err)
            if max_err > 0.0:
                raise Error("gemv_norm is not bit-exact vs the separate chain")

    # Restore x_ref to the rmsnorm_residual output for the FFN tests.
    with h_prev.map_to_host() as hp, delta.map_to_host() as dl, resid.map_to_host() as rs:
        for i in range(HID):
            rs[i] = hp[i]
    ctx.enqueue_function[rmsnorm_residual_f16](
        x_ref.unsafe_ptr(), resid.unsafe_ptr(), delta.unsafe_ptr(), nw.unsafe_ptr(), HID, EPS,
        grid_dim=1, block_dim=256,
    )
    ctx.synchronize()

    # ---- gemv_norm_silu_* ----
    var gu_ref = ctx.enqueue_create_buffer[DType.float16](2 * INTER)
    var up_ref = ctx.enqueue_create_buffer[DType.float16](INTER)
    var act_ref = ctx.enqueue_create_buffer[DType.float16](INTER)
    var act_fus = ctx.enqueue_create_buffer[DType.float16](INTER)
    for rpw in range(1, 3):
      for fmt in range(3):
        if fmt == 0:
            ctx.enqueue_function[gemv_q8_0_f16_v2](
                gu_ref.unsafe_ptr(), wq8_ffn.unsafe_ptr(), x_ref.unsafe_ptr(), HID, 2 * INTER,
                grid_dim=(2 * INTER + 7) // 8, block_dim=256,
            )
        elif fmt == 1:
            ctx.enqueue_function[gemv_nvfp4_f16_v2](
                gu_ref.unsafe_ptr(), wnv_ffn.unsafe_ptr(), wnv_ffn_s.unsafe_ptr(),
                x_ref.unsafe_ptr(), HID, 2 * INTER, NV_IGS,
                grid_dim=(2 * INTER + 7) // 8, block_dim=256,
            )
        else:
            ctx.enqueue_function[gemv_f16_v2](
                gu_ref.unsafe_ptr(), wf_ffn.unsafe_ptr(), x_ref.unsafe_ptr(), HID, 2 * INTER,
                grid_dim=(2 * INTER + 7) // 8, block_dim=256,
            )
        ctx.synchronize()
        # The aliasing checker rejects gate/up pointers into one buffer, so the
        # up half is copied out before the reference silu (values unchanged).
        with gu_ref.map_to_host() as g, up_ref.map_to_host() as u:
            for i in range(INTER):
                u[i] = g[INTER + i]
        ctx.enqueue_function[silu_mul_f16](
            act_ref.unsafe_ptr(), gu_ref.unsafe_ptr(), up_ref.unsafe_ptr(), INTER,
            grid_dim=(INTER + 255) // 256, block_dim=256,
        )
        if fmt == 0:
            ctx.enqueue_function[gemv_norm_silu_q8_0_f16](
                act_fus.unsafe_ptr(), wq8_ffn.unsafe_ptr(), resid.unsafe_ptr(),
                h32.unsafe_ptr(), nw.unsafe_ptr(), HID, INTER, EPS, rpw,
                grid_dim=(INTER + 8 * rpw - 1) // (8 * rpw), block_dim=256,
            )
        elif fmt == 1:
            ctx.enqueue_function[gemv_norm_silu_nvfp4_f16](
                act_fus.unsafe_ptr(), wnv_ffn.unsafe_ptr(), wnv_ffn_s.unsafe_ptr(),
                resid.unsafe_ptr(), h32.unsafe_ptr(), nw.unsafe_ptr(),
                HID, INTER, NV_IGS, EPS, rpw,
                grid_dim=(INTER + 8 * rpw - 1) // (8 * rpw), block_dim=256,
            )
        else:
            ctx.enqueue_function[gemv_norm_silu_f16](
                act_fus.unsafe_ptr(), wf_ffn.unsafe_ptr(), resid.unsafe_ptr(),
                h32.unsafe_ptr(), nw.unsafe_ptr(), HID, INTER, EPS, rpw,
                grid_dim=(INTER + 8 * rpw - 1) // (8 * rpw), block_dim=256,
            )
        ctx.synchronize()
        with act_ref.map_to_host() as a, act_fus.map_to_host() as b:
            for i in range(INTER):
                err = abs(Float32(a[i]) - Float32(b[i]))
                if err > max_err:
                    max_err = err
        print("gemv_norm_silu fmt =", fmt, "rpw =", rpw, "max_err:", max_err)
        if max_err > 0.0:
            raise Error("gemv_norm_silu is not bit-exact vs the separate chain")

    # ---- gemv_residual_* ----
    # Reference: y = gemv(x); then the separate chain's residual add
    # v = f32(h) + f32(y) (IEEE adds reproduced on the host), h' = f16(v).
    var hr_init = ctx.enqueue_create_buffer[DType.float16](QROWS)
    var hr_io = ctx.enqueue_create_buffer[DType.float16](QROWS)
    var hr32 = ctx.enqueue_create_buffer[DType.float32](QROWS)
    with hr_init.map_to_host() as hh:
        for i in range(QROWS):
            hh[i] = Float16(_fill(i + 13) * 0.2)
    for fmt in range(3):
        if fmt == 0:
            ctx.enqueue_function[gemv_q8_0_f16_v2](
                y_ref.unsafe_ptr(), wq8.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
        elif fmt == 1:
            ctx.enqueue_function[gemv_nvfp4_f16_v2](
                y_ref.unsafe_ptr(), wnv.unsafe_ptr(), wnv_s.unsafe_ptr(),
                x_ref.unsafe_ptr(), HID, QROWS, NV_IGS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
        else:
            ctx.enqueue_function[gemv_f16_v2](
                y_ref.unsafe_ptr(), wf.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
        ctx.synchronize()
        with hr_init.map_to_host() as hi, hr_io.map_to_host() as ho:
            for i in range(QROWS):
                ho[i] = hi[i]
        if fmt == 0:
            ctx.enqueue_function[gemv_residual_q8_0_f16](
                hr_io.unsafe_ptr(), hr32.unsafe_ptr(), wq8.unsafe_ptr(),
                x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
        elif fmt == 1:
            ctx.enqueue_function[gemv_residual_nvfp4_f16](
                hr_io.unsafe_ptr(), hr32.unsafe_ptr(), wnv.unsafe_ptr(),
                wnv_s.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS, NV_IGS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
        else:
            ctx.enqueue_function[gemv_residual_f16](
                hr_io.unsafe_ptr(), hr32.unsafe_ptr(), wf.unsafe_ptr(),
                x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
        ctx.synchronize()
        with y_ref.map_to_host() as yr, hr_init.map_to_host() as hi, hr_io.map_to_host() as ho, hr32.map_to_host() as h3:
            for i in range(QROWS):
                v = Float32(hi[i]) + Float32(yr[i])
                err = abs(Float32(ho[i]) - Float32(Float16(v)))
                if abs(h3[i] - v) > err:
                    err = abs(h3[i] - v)
                if err > max_err:
                    max_err = err
        print("gemv_residual fmt =", fmt, "max_err:", max_err)
        if max_err > 0.0:
            raise Error("gemv_residual is not bit-exact vs the separate chain")

    # ---- rmsnorm_h32_f16 ----
    var out_fus = ctx.enqueue_create_buffer[DType.float16](HID)
    ctx.enqueue_function[rmsnorm_h32_f16](
        out_fus.unsafe_ptr(), resid.unsafe_ptr(), h32.unsafe_ptr(), nw.unsafe_ptr(), HID, EPS,
        grid_dim=1, block_dim=256,
    )
    ctx.synchronize()
    with x_ref.map_to_host() as a, out_fus.map_to_host() as b:
        for i in range(HID):
            err = abs(Float32(a[i]) - Float32(b[i]))
            if err > max_err:
                max_err = err
    print("rmsnorm_h32 max_err:", max_err)
    if max_err > 0.0:
        raise Error("rmsnorm_h32_f16 is not bit-exact vs rmsnorm_residual_f16")

    # ---- attn_decode_split + combine vs qkv_post + attn_decode ----
    # n_splits = 1 must be BIT-exact (f32 partial, combine multiplies by
    # exp(0) == 1). n_splits = 3 regroups the online softmax, so it is only
    # required to agree within a small tolerance (documented rounding change).
    comptime PQH = 4
    comptime PKVH = 2
    comptime PPAGE = 16
    comptime PNPAGES = 3
    comptime PTHETA: Float32 = 1000000.0
    comptime PSEQ = 21
    comptime SCALE64: Float32 = 0.125
    comptime SCALE128: Float32 = 0.088388348
    comptime MAX_SPLITS = 3

    var ppt = ctx.enqueue_create_buffer[DType.int32](PNPAGES)
    var pslen = ctx.enqueue_create_buffer[DType.int32](1)
    var ppos = ctx.enqueue_create_buffer[DType.int32](1)
    with ppt.map_to_host() as hh:
        hh[0] = 2
        hh[1] = 1
        hh[2] = 0
    with pslen.map_to_host() as hh:
        hh[0] = Int32(PSEQ)
    with ppos.map_to_host() as hh:
        hh[0] = Int32(PSEQ - 1)

    for hd_case in range(2):
        phd = 64 if hd_case == 0 else 128
        var q_ref2 = ctx.enqueue_create_buffer[DType.float16](PQH * phd)
        var k_ref2 = ctx.enqueue_create_buffer[DType.float16](PKVH * phd)
        var v_ref2 = ctx.enqueue_create_buffer[DType.float16](PKVH * phd)
        var q_raw = ctx.enqueue_create_buffer[DType.float16](PQH * phd)
        var k_raw = ctx.enqueue_create_buffer[DType.float16](PKVH * phd)
        var v_raw = ctx.enqueue_create_buffer[DType.float16](PKVH * phd)
        var qw2 = ctx.enqueue_create_buffer[DType.float16](phd)
        var kw2 = ctx.enqueue_create_buffer[DType.float16](phd)
        var kc_ref = ctx.enqueue_create_buffer[DType.float16](PNPAGES * PKVH * PPAGE * phd)
        var vc_ref = ctx.enqueue_create_buffer[DType.float16](PNPAGES * PKVH * PPAGE * phd)
        var kc_fus = ctx.enqueue_create_buffer[DType.float16](PNPAGES * PKVH * PPAGE * phd)
        var vc_fus = ctx.enqueue_create_buffer[DType.float16](PNPAGES * PKVH * PPAGE * phd)
        var out_ref = ctx.enqueue_create_buffer[DType.float16](PQH * phd)
        var out_fus2 = ctx.enqueue_create_buffer[DType.float16](PQH * phd)
        var parts = ctx.enqueue_create_buffer[DType.float32](PQH * MAX_SPLITS * (phd + 2))
        with qw2.map_to_host() as a, kw2.map_to_host() as b:
            for i in range(phd):
                a[i] = Float16(0.9 + Float32(i % 5) * 0.06)
                b[i] = Float16(1.1 - Float32(i % 7) * 0.04)

        for case_idx in range(4):
            norm_case = case_idx % 2
            n_splits = 1 if case_idx < 2 else MAX_SPLITS
            with q_ref2.map_to_host() as a, q_raw.map_to_host() as b:
                for i in range(PQH * phd):
                    a[i] = Float16(_fill(i) * 0.3)
                    b[i] = a[i]
            with k_ref2.map_to_host() as a, k_raw.map_to_host() as b:
                for i in range(PKVH * phd):
                    a[i] = Float16(_fill(i + 5) * 0.3)
                    b[i] = a[i]
            with v_ref2.map_to_host() as a, v_raw.map_to_host() as b:
                for i in range(PKVH * phd):
                    a[i] = Float16(_fill(i + 9) * 0.3)
                    b[i] = a[i]
            # Non-current cache positions hold identical history in both runs.
            with kc_ref.map_to_host() as a, kc_fus.map_to_host() as b, vc_ref.map_to_host() as c, vc_fus.map_to_host() as d:
                for i in range(PNPAGES * PKVH * PPAGE * phd):
                    a[i] = Float16(_fill(i + 17) * 0.2)
                    b[i] = a[i]
                    c[i] = Float16(_fill(i + 23) * 0.2)
                    d[i] = c[i]

            attn_scale = SCALE64 if hd_case == 0 else SCALE128
            ctx.enqueue_function[qkv_post_f16](
                q_ref2.unsafe_ptr(), k_ref2.unsafe_ptr(), v_ref2.unsafe_ptr(),
                qw2.unsafe_ptr(), kw2.unsafe_ptr(),
                kc_ref.unsafe_ptr(), vc_ref.unsafe_ptr(),
                ppos.unsafe_ptr(), ppt.unsafe_ptr(), pslen.unsafe_ptr(),
                PQH, PKVH, phd, PPAGE, norm_case, norm_case, EPS, PTHETA,
                grid_dim=PQH + PKVH, block_dim=phd,
            )
            if hd_case == 0:
                ctx.enqueue_function[attn_decode_f16_hd64](
                    out_ref.unsafe_ptr(), q_ref2.unsafe_ptr(), kc_ref.unsafe_ptr(),
                    vc_ref.unsafe_ptr(), ppt.unsafe_ptr(), pslen.unsafe_ptr(),
                    PQH, PKVH, PPAGE, PNPAGES, attn_scale,
                    grid_dim=(1, PQH), block_dim=128,
                )
                ctx.enqueue_function[attn_decode_split_f16_hd64](
                    parts.unsafe_ptr(), q_raw.unsafe_ptr(), k_raw.unsafe_ptr(),
                    v_raw.unsafe_ptr(), qw2.unsafe_ptr(), kw2.unsafe_ptr(),
                    kc_fus.unsafe_ptr(), vc_fus.unsafe_ptr(),
                    ppt.unsafe_ptr(), pslen.unsafe_ptr(), ppos.unsafe_ptr(),
                    PQH, PKVH, PPAGE, PNPAGES, n_splits, norm_case, norm_case,
                    EPS, PTHETA, attn_scale,
                    grid_dim=(1, PQH, n_splits), block_dim=128,
                )
                ctx.enqueue_function[attn_decode_combine_f16_hd64](
                    out_fus2.unsafe_ptr(), parts.unsafe_ptr(), PQH, n_splits,
                    grid_dim=(1, PQH), block_dim=32,
                )
            else:
                ctx.enqueue_function[attn_decode_f16_hd128](
                    out_ref.unsafe_ptr(), q_ref2.unsafe_ptr(), kc_ref.unsafe_ptr(),
                    vc_ref.unsafe_ptr(), ppt.unsafe_ptr(), pslen.unsafe_ptr(),
                    PQH, PKVH, PPAGE, PNPAGES, attn_scale,
                    grid_dim=(1, PQH), block_dim=128,
                )
                ctx.enqueue_function[attn_decode_split_f16_hd128](
                    parts.unsafe_ptr(), q_raw.unsafe_ptr(), k_raw.unsafe_ptr(),
                    v_raw.unsafe_ptr(), qw2.unsafe_ptr(), kw2.unsafe_ptr(),
                    kc_fus.unsafe_ptr(), vc_fus.unsafe_ptr(),
                    ppt.unsafe_ptr(), pslen.unsafe_ptr(), ppos.unsafe_ptr(),
                    PQH, PKVH, PPAGE, PNPAGES, n_splits, norm_case, norm_case,
                    EPS, PTHETA, attn_scale,
                    grid_dim=(1, PQH, n_splits), block_dim=128,
                )
                ctx.enqueue_function[attn_decode_combine_f16_hd128](
                    out_fus2.unsafe_ptr(), parts.unsafe_ptr(), PQH, n_splits,
                    grid_dim=(1, PQH), block_dim=32,
                )
            ctx.synchronize()

            with out_ref.map_to_host() as a, out_fus2.map_to_host() as b:
                for i in range(PQH * phd):
                    err = abs(Float32(a[i]) - Float32(b[i]))
                    if err > max_err:
                        max_err = err
            with kc_ref.map_to_host() as a, kc_fus.map_to_host() as b:
                for i in range(PNPAGES * PKVH * PPAGE * phd):
                    err = abs(Float32(a[i]) - Float32(b[i]))
                    if err > max_err:
                        max_err = err
            with vc_ref.map_to_host() as a, vc_fus.map_to_host() as b:
                for i in range(PNPAGES * PKVH * PPAGE * phd):
                    err = abs(Float32(a[i]) - Float32(b[i]))
                    if err > max_err:
                        max_err = err
            print(
                "attn_decode_split hd =", phd, "norm =", norm_case,
                "splits =", n_splits, "max_err:", max_err,
            )
            if n_splits == 1 and max_err > 0.0:
                raise Error("attn_decode_split n=1 is not bit-exact vs qkv_post + attn_decode")
            if max_err > 1e-3:
                raise Error("attn_decode_split n>1 exceeds the documented tolerance")
            max_err = 0.0

    print("ALL DECODE-FUSED CHECKS PASSED")
