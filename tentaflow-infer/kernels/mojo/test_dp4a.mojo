# ===== File: test_dp4a.mojo — correctness of the int8-activation dp4a GEMV variants =====
# Two contracts:
#   1. Accuracy: the dp4a kernels quantize activations to q8_1 (int8), so
#      they are NOT bit-exact vs the f16-x kernels — the plain dp4a GEMVs
#      must agree with gemv_*_f16_v2 within a small relative tolerance.
#   2. Consistency WITHIN the dp4a path: per-segment quantization is
#      deterministic, so the fused variants (norm / norm_silu / residual)
#      must be BIT-exact vs the plain dp4a GEMV fed the separate chain's
#      normed x (same structure as test_decode_fused.mojo), and the
#      out_f32 variants must round to the f16 outputs exactly.

from std.gpu.host import DeviceContext
from src.norm import rmsnorm_residual_f16
from src.activation import silu_mul_f16
from src.gemv2 import gemv_q8_0_f16_v2, gemv_q4_k_f16_v2, gemv_q6_k_f16_v2
from src.decode_dp4a import gemv_q8_0_dp4a_f16, gemv_q8_0_dp4a_out_f32
from src.decode_dp4a import gemv_q4_k_dp4a_f16, gemv_q4_k_dp4a_out_f32
from src.decode_dp4a import gemv_q6_k_dp4a_f16, gemv_q6_k_dp4a_out_f32
from src.decode_dp4a import gemv_norm_q8_0_dp4a_f16, gemv_norm_q4_k_dp4a_f16, gemv_norm_q6_k_dp4a_f16
from src.decode_dp4a import gemv_norm_silu_q8_0_dp4a_f16, gemv_norm_silu_q4_k_dp4a_f16, gemv_norm_silu_q6_k_dp4a_f16
from src.decode_dp4a import gemv_residual_q8_0_dp4a_f16, gemv_residual_q4_k_dp4a_f16, gemv_residual_q6_k_dp4a_f16

comptime HID = 1024
comptime QROWS = 60  # deliberately not a multiple of 8: exercises the row guard
comptime INTER = 48
comptime EPS: Float32 = 1e-6
comptime Q8_BPR = HID // 32
comptime K4_BPR = HID // 256
comptime REL_TOL: Float32 = 0.03


def _fill(i: Int) -> Float32:
    return Float32((i * 37 % 19) - 9) * 0.25


def main() raises:
    var ctx = DeviceContext()

    # ---- residual pair (h f16, h32 f32) + normed x, as in the engine ----
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
    with h_prev.map_to_host() as hp, delta.map_to_host() as dl, resid.map_to_host() as rs, h32.map_to_host() as h3:
        for i in range(HID):
            rs[i] = hp[i]
            h3[i] = Float32(hp[i]) + Float32(dl[i])
    ctx.enqueue_function[rmsnorm_residual_f16](
        x_ref.unsafe_ptr(), resid.unsafe_ptr(), delta.unsafe_ptr(), nw.unsafe_ptr(), HID, EPS,
        grid_dim=1, block_dim=256,
    )
    ctx.synchronize()

    # ---- q8_0 / q4_k / q6_k weights (QROWS x HID and 2*INTER x HID) ----
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

    var wq4 = ctx.enqueue_create_buffer[DType.uint8](QROWS * K4_BPR * 144)
    var wq4_ffn = ctx.enqueue_create_buffer[DType.uint8](2 * INTER * K4_BPR * 144)
    with wq4.map_to_host() as wh:
        for i in range(QROWS * K4_BPR * 144):
            wh[i] = UInt8((i * 53 + 7) % 256)
        for r in range(QROWS * K4_BPR):
            d = Float16(0.008 + Float32(r % 7) * 0.004)
            dmin = Float16(0.005 + Float32(r % 5) * 0.003)
            bits = d.to_bits()
            wh[r * 144] = UInt8(bits & 0xFF)
            wh[r * 144 + 1] = UInt8((bits >> 8) & 0xFF)
            bits = dmin.to_bits()
            wh[r * 144 + 2] = UInt8(bits & 0xFF)
            wh[r * 144 + 3] = UInt8((bits >> 8) & 0xFF)
    with wq4_ffn.map_to_host() as wh:
        for i in range(2 * INTER * K4_BPR * 144):
            wh[i] = UInt8((i * 47 + 11) % 256)
        for r in range(2 * INTER * K4_BPR):
            d = Float16(0.008 + Float32(r % 5) * 0.004)
            dmin = Float16(0.005 + Float32(r % 7) * 0.003)
            bits = d.to_bits()
            wh[r * 144] = UInt8(bits & 0xFF)
            wh[r * 144 + 1] = UInt8((bits >> 8) & 0xFF)
            bits = dmin.to_bits()
            wh[r * 144 + 2] = UInt8(bits & 0xFF)
            wh[r * 144 + 3] = UInt8((bits >> 8) & 0xFF)

    var wq6 = ctx.enqueue_create_buffer[DType.uint8](QROWS * K4_BPR * 210)
    var wq6_ffn = ctx.enqueue_create_buffer[DType.uint8](2 * INTER * K4_BPR * 210)
    with wq6.map_to_host() as wh:
        for i in range(QROWS * K4_BPR * 210):
            wh[i] = UInt8((i * 37 + 3) % 256)
        for r in range(QROWS * K4_BPR):
            d = Float16(0.006 + Float32(r % 7) * 0.003)
            bits = d.to_bits()
            wh[r * 210 + 208] = UInt8(bits & 0xFF)
            wh[r * 210 + 209] = UInt8((bits >> 8) & 0xFF)
    with wq6_ffn.map_to_host() as wh:
        for i in range(2 * INTER * K4_BPR * 210):
            wh[i] = UInt8((i * 43 + 9) % 256)
        for r in range(2 * INTER * K4_BPR):
            d = Float16(0.006 + Float32(r % 5) * 0.003)
            bits = d.to_bits()
            wh[r * 210 + 208] = UInt8(bits & 0xFF)
            wh[r * 210 + 209] = UInt8((bits >> 8) & 0xFF)

    var y_f16 = ctx.enqueue_create_buffer[DType.float16](QROWS)
    var y_dp4a = ctx.enqueue_create_buffer[DType.float16](QROWS)
    var y_f32 = ctx.enqueue_create_buffer[DType.float32](QROWS)

    # ---- 1. accuracy: plain dp4a vs f16 v2, relative tolerance ----
    # ---- and out_f32 == f16 output before rounding (exact) ----
    for fmt in range(3):
        if fmt == 0:
            ctx.enqueue_function[gemv_q8_0_f16_v2](
                y_f16.unsafe_ptr(), wq8.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
            ctx.enqueue_function[gemv_q8_0_dp4a_f16](
                y_dp4a.unsafe_ptr(), wq8.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
            ctx.enqueue_function[gemv_q8_0_dp4a_out_f32](
                y_f32.unsafe_ptr(), wq8.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
        elif fmt == 1:
            ctx.enqueue_function[gemv_q4_k_f16_v2](
                y_f16.unsafe_ptr(), wq4.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
            ctx.enqueue_function[gemv_q4_k_dp4a_f16](
                y_dp4a.unsafe_ptr(), wq4.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
            ctx.enqueue_function[gemv_q4_k_dp4a_out_f32](
                y_f32.unsafe_ptr(), wq4.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
        else:
            ctx.enqueue_function[gemv_q6_k_f16_v2](
                y_f16.unsafe_ptr(), wq6.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
            ctx.enqueue_function[gemv_q6_k_dp4a_f16](
                y_dp4a.unsafe_ptr(), wq6.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
            ctx.enqueue_function[gemv_q6_k_dp4a_out_f32](
                y_f32.unsafe_ptr(), wq6.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
        ctx.synchronize()
        var ref_max: Float32 = 0.0
        var abs_err: Float32 = 0.0
        var f32_err: Float32 = 0.0
        with y_f16.map_to_host() as a, y_dp4a.map_to_host() as b, y_f32.map_to_host() as c:
            for i in range(QROWS):
                if abs(Float32(a[i])) > ref_max:
                    ref_max = abs(Float32(a[i]))
                err = abs(Float32(a[i]) - Float32(b[i]))
                if err > abs_err:
                    abs_err = err
                e32 = abs(Float32(Float16(c[i])) - Float32(b[i]))
                if e32 > f32_err:
                    f32_err = e32
        rel = abs_err / ref_max
        print("plain dp4a fmt =", fmt, "rel_err:", rel, "out_f32 mismatch:", f32_err)
        if rel > REL_TOL:
            raise Error("plain dp4a GEMV exceeds relative tolerance vs f16 kernel")
        if f32_err > 0.0:
            raise Error("dp4a out_f32 does not round to the f16 output")

    # ---- 2. gemv_norm_*_dp4a vs plain dp4a on the separate chain's x ----
    var max_err: Float32 = 0.0
    for rpw in range(1, 3):
        for fmt in range(3):
            if fmt == 0:
                ctx.enqueue_function[gemv_q8_0_dp4a_f16](
                    y_dp4a.unsafe_ptr(), wq8.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                    grid_dim=(QROWS + 7) // 8, block_dim=256,
                )
                ctx.enqueue_function[gemv_norm_q8_0_dp4a_f16](
                    y_f16.unsafe_ptr(), wq8.unsafe_ptr(), resid.unsafe_ptr(),
                    h32.unsafe_ptr(), nw.unsafe_ptr(), HID, QROWS, 0, EPS, rpw,
                    grid_dim=(QROWS + 8 * rpw - 1) // (8 * rpw), block_dim=256,
                )
            elif fmt == 1:
                ctx.enqueue_function[gemv_q4_k_dp4a_f16](
                    y_dp4a.unsafe_ptr(), wq4.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                    grid_dim=(QROWS + 7) // 8, block_dim=256,
                )
                ctx.enqueue_function[gemv_norm_q4_k_dp4a_f16](
                    y_f16.unsafe_ptr(), wq4.unsafe_ptr(), resid.unsafe_ptr(),
                    h32.unsafe_ptr(), nw.unsafe_ptr(), HID, QROWS, 0, EPS, rpw,
                    grid_dim=(QROWS + 8 * rpw - 1) // (8 * rpw), block_dim=256,
                )
            else:
                ctx.enqueue_function[gemv_q6_k_dp4a_f16](
                    y_dp4a.unsafe_ptr(), wq6.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                    grid_dim=(QROWS + 7) // 8, block_dim=256,
                )
                ctx.enqueue_function[gemv_norm_q6_k_dp4a_f16](
                    y_f16.unsafe_ptr(), wq6.unsafe_ptr(), resid.unsafe_ptr(),
                    h32.unsafe_ptr(), nw.unsafe_ptr(), HID, QROWS, 0, EPS, rpw,
                    grid_dim=(QROWS + 8 * rpw - 1) // (8 * rpw), block_dim=256,
                )
            ctx.synchronize()
            with y_f16.map_to_host() as a, y_dp4a.map_to_host() as b:
                for i in range(QROWS):
                    err = abs(Float32(a[i]) - Float32(b[i]))
                    if err > max_err:
                        max_err = err
            print("gemv_norm_dp4a fmt =", fmt, "rpw =", rpw, "max_err:", max_err)
            if max_err > 0.0:
                raise Error("gemv_norm_dp4a is not bit-exact vs the plain dp4a chain")

    # ---- 3. gemv_norm_silu_*_dp4a vs plain dp4a gate|up + silu ----
    var gu_ref = ctx.enqueue_create_buffer[DType.float16](2 * INTER)
    var up_ref = ctx.enqueue_create_buffer[DType.float16](INTER)
    var act_ref = ctx.enqueue_create_buffer[DType.float16](INTER)
    var act_fus = ctx.enqueue_create_buffer[DType.float16](INTER)
    for rpw in range(1, 3):
        for fmt in range(3):
            if fmt == 0:
                ctx.enqueue_function[gemv_q8_0_dp4a_f16](
                    gu_ref.unsafe_ptr(), wq8_ffn.unsafe_ptr(), x_ref.unsafe_ptr(), HID, 2 * INTER,
                    grid_dim=(2 * INTER + 7) // 8, block_dim=256,
                )
            elif fmt == 1:
                ctx.enqueue_function[gemv_q4_k_dp4a_f16](
                    gu_ref.unsafe_ptr(), wq4_ffn.unsafe_ptr(), x_ref.unsafe_ptr(), HID, 2 * INTER,
                    grid_dim=(2 * INTER + 7) // 8, block_dim=256,
                )
            else:
                ctx.enqueue_function[gemv_q6_k_dp4a_f16](
                    gu_ref.unsafe_ptr(), wq6_ffn.unsafe_ptr(), x_ref.unsafe_ptr(), HID, 2 * INTER,
                    grid_dim=(2 * INTER + 7) // 8, block_dim=256,
                )
            ctx.synchronize()
            # The aliasing checker rejects gate/up pointers into one buffer, so
            # the up half is copied out before the reference silu.
            with gu_ref.map_to_host() as g, up_ref.map_to_host() as u:
                for i in range(INTER):
                    u[i] = g[INTER + i]
            ctx.enqueue_function[silu_mul_f16](
                act_ref.unsafe_ptr(), gu_ref.unsafe_ptr(), up_ref.unsafe_ptr(), INTER,
                grid_dim=(INTER + 255) // 256, block_dim=256,
            )
            if fmt == 0:
                ctx.enqueue_function[gemv_norm_silu_q8_0_dp4a_f16](
                    act_fus.unsafe_ptr(), wq8_ffn.unsafe_ptr(), resid.unsafe_ptr(),
                    h32.unsafe_ptr(), nw.unsafe_ptr(), HID, INTER, EPS, rpw,
                    grid_dim=(INTER + 8 * rpw - 1) // (8 * rpw), block_dim=256,
                )
            elif fmt == 1:
                ctx.enqueue_function[gemv_norm_silu_q4_k_dp4a_f16](
                    act_fus.unsafe_ptr(), wq4_ffn.unsafe_ptr(), resid.unsafe_ptr(),
                    h32.unsafe_ptr(), nw.unsafe_ptr(), HID, INTER, EPS, rpw,
                    grid_dim=(INTER + 8 * rpw - 1) // (8 * rpw), block_dim=256,
                )
            else:
                ctx.enqueue_function[gemv_norm_silu_q6_k_dp4a_f16](
                    act_fus.unsafe_ptr(), wq6_ffn.unsafe_ptr(), resid.unsafe_ptr(),
                    h32.unsafe_ptr(), nw.unsafe_ptr(), HID, INTER, EPS, rpw,
                    grid_dim=(INTER + 8 * rpw - 1) // (8 * rpw), block_dim=256,
                )
            ctx.synchronize()
            with act_ref.map_to_host() as a, act_fus.map_to_host() as b:
                for i in range(INTER):
                    err = abs(Float32(a[i]) - Float32(b[i]))
                    if err > max_err:
                        max_err = err
            print("gemv_norm_silu_dp4a fmt =", fmt, "rpw =", rpw, "max_err:", max_err)
            if max_err > 0.0:
                raise Error("gemv_norm_silu_dp4a is not bit-exact vs the plain dp4a chain")

    # ---- 4. gemv_residual_*_dp4a vs plain dp4a + host residual add ----
    var hr_init = ctx.enqueue_create_buffer[DType.float16](QROWS)
    var hr_io = ctx.enqueue_create_buffer[DType.float16](QROWS)
    var hr32 = ctx.enqueue_create_buffer[DType.float32](QROWS)
    with hr_init.map_to_host() as hh:
        for i in range(QROWS):
            hh[i] = Float16(_fill(i + 13) * 0.2)
    for fmt in range(3):
        if fmt == 0:
            ctx.enqueue_function[gemv_q8_0_dp4a_f16](
                y_dp4a.unsafe_ptr(), wq8.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
        elif fmt == 1:
            ctx.enqueue_function[gemv_q4_k_dp4a_f16](
                y_dp4a.unsafe_ptr(), wq4.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
        else:
            ctx.enqueue_function[gemv_q6_k_dp4a_f16](
                y_dp4a.unsafe_ptr(), wq6.unsafe_ptr(), x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
        ctx.synchronize()
        with hr_init.map_to_host() as hi, hr_io.map_to_host() as ho:
            for i in range(QROWS):
                ho[i] = hi[i]
        if fmt == 0:
            ctx.enqueue_function[gemv_residual_q8_0_dp4a_f16](
                hr_io.unsafe_ptr(), hr32.unsafe_ptr(), wq8.unsafe_ptr(),
                x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
        elif fmt == 1:
            ctx.enqueue_function[gemv_residual_q4_k_dp4a_f16](
                hr_io.unsafe_ptr(), hr32.unsafe_ptr(), wq4.unsafe_ptr(),
                x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
        else:
            ctx.enqueue_function[gemv_residual_q6_k_dp4a_f16](
                hr_io.unsafe_ptr(), hr32.unsafe_ptr(), wq6.unsafe_ptr(),
                x_ref.unsafe_ptr(), HID, QROWS,
                grid_dim=(QROWS + 7) // 8, block_dim=256,
            )
        ctx.synchronize()
        with y_dp4a.map_to_host() as yr, hr_init.map_to_host() as hi, hr_io.map_to_host() as ho, hr32.map_to_host() as h3:
            for i in range(QROWS):
                v = Float32(hi[i]) + Float32(yr[i])
                err = abs(Float32(ho[i]) - Float32(Float16(v)))
                if abs(h3[i] - v) > err:
                    err = abs(h3[i] - v)
                if err > max_err:
                    max_err = err
        print("gemv_residual_dp4a fmt =", fmt, "max_err:", max_err)
        if max_err > 0.0:
            raise Error("gemv_residual_dp4a is not bit-exact vs the plain dp4a chain")

    print("all dp4a checks passed")
