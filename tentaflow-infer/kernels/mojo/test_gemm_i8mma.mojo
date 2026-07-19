# ===== File: test_gemm_i8mma.mojo — int8 TENSOR-CORE MMQ prefill GEMM =====
# The i8mma GEMM quantizes activations to q8_1 and runs s8xs8->s32 tensor-core
# mma per 32-block, so it is NOT bit-exact vs the f16-dequant GEMM
# (gemm_q8_0_f16 / gemm_q4_k_f16) — the same class of modeling error the decode
# dp4a path and llama.cpp's mul_mat_q accept. Two contracts:
#   1. Kernel correctness: the kernel matches an EXACT CPU MMQ reference (same
#      per-32-block q8_1 quantization + integer dot + scale math) to tight
#      float tolerance — this proves the mma fragment layout / scaling is right.
#   2. bm64 vs bm128 are BIT-identical (same per-element chain).
# The i8mma-vs-f16 number is printed for context (int8 vs f16 modeling error).

from std.gpu.host import DeviceContext
from src.gemm import gemm_q8_0_f16, gemm_q4_k_f16
from src.gemm import gemm_q8_0_i8mma, gemm_q8_0_i8mma_bm64
from src.gemm import gemm_q4_k_i8mma, gemm_q4_k_i8mma_bm64
from src.gemm import quantize_act_q8_1

comptime KERNEL_TOL: Float32 = 0.01  # i8mma kernel vs exact CPU MMQ reference


def _fill(i: Int) -> Float32:
    # Continuous, well-spread activations (an LCG mapped to [-1, 1]) — the
    # realistic regime for q8_1, matching the rmsnorm output decode feeds.
    seed = (UInt32(i) * 2654435761 + 1013904223) & 0xFFFFFFFF
    return Float32(seed) * (2.0 / 4294967296.0) - 1.0


def _gsm_ref(sm4: Int, s: Int, sp4: Int, j: Int) -> Tuple[Float32, Float32]:
    if j < 4:
        return (Float32(s & 63), Float32(sp4 & 63))
    sc = (sp4 & 0x0F) | ((sm4 >> 6) << 4)
    mn = (sp4 >> 4) | ((s >> 6) << 4)
    return (Float32(sc), Float32(mn))


# Quantize one 32-element x block to q8_1: (int8 codes, d, d*Σcodes).
def _quant_block(
    xh: UnsafePointer[Float16, MutUntrackedOrigin], base: Int
) -> Tuple[InlineArray[Int32, 32], Float32, Float32]:
    var q = InlineArray[Int32, 32](fill=0)
    var amax: Float32 = 0.0
    for k in range(32):
        v = abs(Float32(xh[base + k]))
        if v > amax:
            amax = v
    if amax == 0.0:
        return (q, Float32(0.0), Float32(0.0))
    d = amax / 127.0
    var sumq: Int32 = 0
    for k in range(32):
        qi = Int32(round(Float32(xh[base + k]) * (127.0 / amax)))
        q[k] = qi
        sumq += qi
    return (q, d, d * Float32(sumq))


def main() raises:
    var ctx = DeviceContext()
    comptime T = 100  # not a multiple of 128 or 64: exercises the token guard
    comptime ROWS = 70  # not a multiple of 64: exercises the row guard

    # ---------------- Q8_0 ----------------
    comptime COLS = 256
    var x = ctx.enqueue_create_buffer[DType.float16](T * COLS)
    var wq = ctx.enqueue_create_buffer[DType.uint8](ROWS * (COLS // 32) * 34)
    var yref = ctx.enqueue_create_buffer[DType.float16](T * ROWS)
    var ym = ctx.enqueue_create_buffer[DType.float16](T * ROWS)
    var ym64 = ctx.enqueue_create_buffer[DType.float16](T * ROWS)
    with x.map_to_host() as h:
        for i in range(T * COLS):
            h[i] = Float16(_fill(i))
    with wq.map_to_host() as h:
        for r in range(ROWS):
            for b in range(COLS // 32):
                off = (r * (COLS // 32) + b) * 34
                sc = Float16(0.02 + Float32((r + b) % 5) * 0.01)
                bits = sc.to_bits()
                h[off] = UInt8(bits & 0xFF)
                h[off + 1] = UInt8((bits >> 8) & 0xFF)
                for k in range(32):
                    h[off + 2 + k] = UInt8(
                        (Int((r * 31 + b * 17 + k * 13) % 255) - 127) & 0xFF
                    )

    var xq = ctx.enqueue_create_buffer[DType.int8](T * COLS)
    var xd = ctx.enqueue_create_buffer[DType.float32](T * (COLS // 32))
    var xsm = ctx.enqueue_create_buffer[DType.float32](T * (COLS // 32))
    ctx.enqueue_function[gemm_q8_0_f16](
        yref.unsafe_ptr(), wq.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS, T,
        grid_dim=((ROWS + 63) // 64, (T + 127) // 128), block_dim=256,
    )
    ctx.enqueue_function[quantize_act_q8_1](
        xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), x.unsafe_ptr(), COLS, T,
        grid_dim=(T * (COLS // 32) + 255) // 256, block_dim=256,
    )
    ctx.enqueue_function[gemm_q8_0_i8mma](
        ym.unsafe_ptr(), wq.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
        xsm.unsafe_ptr(), COLS, ROWS, T,
        grid_dim=((ROWS + 63) // 64, (T + 127) // 128), block_dim=256,
    )
    ctx.enqueue_function[gemm_q8_0_i8mma_bm64](
        ym64.unsafe_ptr(), wq.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
        xsm.unsafe_ptr(), COLS, ROWS, T,
        grid_dim=((ROWS + 63) // 64, (T + 63) // 64), block_dim=256,
    )
    ctx.synchronize()

    var e_mma: Float32 = 0.0  # i8mma kernel vs exact CPU MMQ
    var e_f16: Float32 = 0.0  # i8mma vs f16 GEMM (informational)
    with yref.map_to_host() as a, x.map_to_host() as xh, wq.map_to_host() as wh, ym.map_to_host() as m, ym64.map_to_host() as m64:
        for t in range(T):
            for r in range(ROWS):
                var refv: Float32 = 0.0
                for bl in range(COLS // 32):
                    qb = _quant_block(xh.unsafe_ptr(), t * COLS + bl * 32)
                    q = qb[0]
                    dx = qb[1]
                    off = (r * (COLS // 32) + bl) * 34
                    dw = Float32(Float16(0.02 + Float32((r + bl) % 5) * 0.01))
                    var dot: Int32 = 0
                    for k in range(32):
                        var wc = Int32(wh[off + 2 + k])
                        if wc > 127:
                            wc -= 256
                        dot += wc * q[k]
                    refv += dw * dx * Float32(dot)
                idx = t * ROWS + r
                em = abs(Float32(m[idx]) - refv) / (abs(refv) + 1.0)
                ef = abs(Float32(m[idx]) - Float32(a[idx])) / (abs(Float32(a[idx])) + 1.0)
                if em > e_mma:
                    e_mma = em
                if ef > e_f16:
                    e_f16 = ef
                if m[idx] != m64[idx]:
                    raise Error("gemm_q8_0_i8mma bm64 mismatch")
    print("gemm_q8_0_i8mma vs exact-MMQ:", e_mma, " vs f16:", e_f16, " (bm64 bit-identical)")
    if e_mma > KERNEL_TOL:
        raise Error("gemm_q8_0_i8mma FAILED (kernel != exact MMQ)")

    # ---------------- Q4_K ----------------
    comptime KCOLS = 512
    comptime KBL = KCOLS // 256
    var xk = ctx.enqueue_create_buffer[DType.float16](T * KCOLS)
    var wk = ctx.enqueue_create_buffer[DType.uint8](ROWS * KBL * 144)
    var ykref = ctx.enqueue_create_buffer[DType.float16](T * ROWS)
    var ykm = ctx.enqueue_create_buffer[DType.float16](T * ROWS)
    var ykm64 = ctx.enqueue_create_buffer[DType.float16](T * ROWS)
    with xk.map_to_host() as h:
        for i in range(T * KCOLS):
            h[i] = Float16(_fill(i + 7))
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

    var xkq = ctx.enqueue_create_buffer[DType.int8](T * KCOLS)
    var xkd = ctx.enqueue_create_buffer[DType.float32](T * (KCOLS // 32))
    var xksm = ctx.enqueue_create_buffer[DType.float32](T * (KCOLS // 32))
    ctx.enqueue_function[gemm_q4_k_f16](
        ykref.unsafe_ptr(), wk.unsafe_ptr(), xk.unsafe_ptr(), KCOLS, ROWS, T,
        grid_dim=((ROWS + 63) // 64, (T + 127) // 128), block_dim=256,
    )
    ctx.enqueue_function[quantize_act_q8_1](
        xkq.unsafe_ptr(), xkd.unsafe_ptr(), xksm.unsafe_ptr(), xk.unsafe_ptr(), KCOLS, T,
        grid_dim=(T * (KCOLS // 32) + 255) // 256, block_dim=256,
    )
    ctx.enqueue_function[gemm_q4_k_i8mma](
        ykm.unsafe_ptr(), wk.unsafe_ptr(), xkq.unsafe_ptr(), xkd.unsafe_ptr(),
        xksm.unsafe_ptr(), KCOLS, ROWS, T,
        grid_dim=((ROWS + 63) // 64, (T + 127) // 128), block_dim=256,
    )
    ctx.enqueue_function[gemm_q4_k_i8mma_bm64](
        ykm64.unsafe_ptr(), wk.unsafe_ptr(), xkq.unsafe_ptr(), xkd.unsafe_ptr(),
        xksm.unsafe_ptr(), KCOLS, ROWS, T,
        grid_dim=((ROWS + 63) // 64, (T + 63) // 64), block_dim=256,
    )
    ctx.synchronize()

    e_mma = 0.0
    e_f16 = 0.0
    with ykref.map_to_host() as a, xk.map_to_host() as xh, wk.map_to_host() as wh, ykm.map_to_host() as m, ykm64.map_to_host() as m64:
        for t in range(T):
            for r in range(ROWS):
                var refv: Float32 = 0.0
                for bb in range(KBL):
                    off = (r * KBL + bb) * 144
                    dref = Float32(Float16(0.008 + Float32((r + bb) % 7) * 0.004))
                    mref = Float32(Float16(0.005 + Float32((r + 2 * bb) % 5) * 0.003))
                    for sub in range(8):
                        scv, mnv = _gsm_ref(
                            Int(wh[off + sub]) if sub >= 4 else 0,
                            Int(wh[off + 4 + sub]),
                            Int(wh[off + 8 + sub]),
                            sub,
                        )
                        chunk = sub // 2
                        half = sub % 2
                        bl = bb * 8 + sub
                        qb = _quant_block(xh.unsafe_ptr(), t * KCOLS + bl * 32)
                        q = qb[0]
                        dx = qb[1]
                        sq = qb[2]
                        var dot: Int32 = 0
                        for k in range(32):
                            nib = Int32(wh[off + 16 + chunk * 32 + k])
                            if half == 0:
                                nib = nib & 0x0F
                            else:
                                nib = (nib >> 4) & 0x0F
                            dot += nib * q[k]
                        refv += dref * scv * dx * Float32(dot) - mref * mnv * sq
                idx = t * ROWS + r
                em = abs(Float32(m[idx]) - refv) / (abs(refv) + 1.0)
                ef = abs(Float32(m[idx]) - Float32(a[idx])) / (abs(Float32(a[idx])) + 1.0)
                if em > e_mma:
                    e_mma = em
                if ef > e_f16:
                    e_f16 = ef
                if m[idx] != m64[idx]:
                    raise Error("gemm_q4_k_i8mma bm64 mismatch")
    print("gemm_q4_k_i8mma vs exact-MMQ:", e_mma, " vs f16:", e_f16, " (bm64 bit-identical)")
    if e_mma > KERNEL_TOL:
        raise Error("gemm_q4_k_i8mma FAILED (kernel != exact MMQ)")
    print("ALL PASSED")
