# ===== File: test_kernels3.mojo — numeric sanity for the extended quant GEMV set =====
# Q5_K / Q3_K / Q2_K superblock kernels and the legacy 32-element formats
# (Q4_0 / Q4_1 / Q5_0 / Q5_1) against scalar CPU math implementing
# forge-formats dequant.rs semantics. The authoritative golden tests live in
# Rust (forge-kernels) against forge-formats references.

from std.gpu.host import DeviceContext
from src.gemv2 import gemv_q5_k_f16_v2, gemv_q5_k_out_f32_v2
from src.gemv2 import gemv_q3_k_f16_v2, gemv_q3_k_out_f32_v2
from src.gemv2 import gemv_q2_k_f16_v2, gemv_q2_k_out_f32_v2
from src.gemv2 import gemv_q4_0_f16_v2, gemv_q4_0_out_f32_v2
from src.gemv2 import gemv_q4_1_f16_v2, gemv_q4_1_out_f32_v2
from src.gemv2 import gemv_q5_0_f16_v2, gemv_q5_0_out_f32_v2
from src.gemv2 import gemv_q5_1_f16_v2, gemv_q5_1_out_f32_v2

comptime ROWS_K = 17  # odd row count exercises the row guard
comptime COLS_K = 512  # two 256-element superblocks per row


def _fill(i: Int) -> Float32:
    return Float32((i * 37 % 19) - 9) * 0.25


def _gsm_ref(sm4: Int, s: Int, sp4: Int, j: Int) -> Tuple[Float32, Float32]:
    # CPU oracle for llama.cpp get_scale_min_k4.
    if j < 4:
        return (Float32(s & 63), Float32(sp4 & 63))
    sc = (sp4 & 0x0F) | ((sm4 >> 6) << 4)
    mn = (sp4 >> 4) | ((s >> 6) << 4)
    return (Float32(sc), Float32(mn))


def _check(
    name: StringSlice,
    got_h: List[Float32],
    got_32: List[Float32],
    expected: List[Float32],
) raises:
    var max_err: Float32 = 0.0
    for r in range(ROWS_K):
        rel = abs(got_h[r] - expected[r]) / (abs(expected[r]) + 1.0)
        if rel > max_err:
            max_err = rel
    print(String(name), "max_rel_err:", max_err)
    if max_err > 0.02:
        raise Error(String(name) + " numeric check FAILED")
    max_err = 0.0
    for r in range(ROWS_K):
        rel = abs(got_32[r] - expected[r]) / (abs(expected[r]) + 1.0)
        if rel > max_err:
            max_err = rel
    print(String(name), "out_f32 max_rel_err:", max_err)
    if max_err > 0.02:
        raise Error(String(name) + " out_f32 numeric check FAILED")


def main() raises:
    var ctx = DeviceContext()
    comptime KBLOCKS = COLS_K // 256

    var xk = ctx.enqueue_create_buffer[DType.float16](COLS_K)
    with xk.map_to_host() as xh:
        for c in range(COLS_K):
            xh[c] = Float16(_fill(c) * 0.1)

    # --- gemv q5_k ---
    var w5 = ctx.enqueue_create_buffer[DType.uint8](ROWS_K * KBLOCKS * 176)
    var y5 = ctx.enqueue_create_buffer[DType.float16](ROWS_K)
    var y5_32 = ctx.enqueue_create_buffer[DType.float32](ROWS_K)
    var exp5 = List[Float32]()
    with w5.map_to_host() as wh, xk.map_to_host() as xh:
        for r in range(ROWS_K):
            var acc: Float32 = 0.0
            for bl in range(KBLOCKS):
                off = (r * KBLOCKS + bl) * 176
                d = Float16(0.008 + Float32((r + bl) % 7) * 0.004)
                dmin = Float16(0.005 + Float32((r + 2 * bl) % 5) * 0.003)
                bits = (d).to_bits()
                wh[off] = UInt8(bits & 0xFF)
                wh[(off) + 1] = UInt8((bits >> 8) & 0xFF)
                bits = (dmin).to_bits()
                wh[off + 2] = UInt8(bits & 0xFF)
                wh[(off + 2) + 1] = UInt8((bits >> 8) & 0xFF)
                for i in range(12):
                    wh[off + 4 + i] = UInt8((r * 53 + bl * 19 + i * 41 + 7) % 256)
                for i in range(32):
                    wh[off + 16 + i] = UInt8((r * 43 + bl * 29 + i * 3) % 256)
                for i in range(128):
                    wh[off + 48 + i] = UInt8((r * 31 + bl * 17 + i * 13) % 256)
                # dq_q5_k reference.
                for g in range(4):
                    j1 = 2 * g
                    j2 = 2 * g + 1
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
                    for l in range(32):
                        ql = Int(wh[off + 48 + g * 32 + l])
                        qh = Int(wh[off + 16 + l])
                        var v1 = ql & 0x0F
                        if qh & (1 << (2 * g)) != 0:
                            v1 += 16
                        var v2 = ql >> 4
                        if qh & (1 << (2 * g + 1)) != 0:
                            v2 += 16
                        col = bl * 256 + g * 64 + l
                        xlo = Float32(xh[col])
                        xhi = Float32(xh[col + 32])
                        acc += (Float32(d) * sc1 * Float32(v1) - Float32(dmin) * mn1) * xlo
                        acc += (Float32(d) * sc2 * Float32(v2) - Float32(dmin) * mn2) * xhi
            exp5.append(acc)
    ctx.enqueue_function[gemv_q5_k_f16_v2](
        y5.unsafe_ptr(), w5.unsafe_ptr(), xk.unsafe_ptr(), COLS_K, ROWS_K,
        grid_dim=(ROWS_K + 7) // 8, block_dim=256,
    )
    ctx.enqueue_function[gemv_q5_k_out_f32_v2](
        y5_32.unsafe_ptr(), w5.unsafe_ptr(), xk.unsafe_ptr(), COLS_K, ROWS_K,
        grid_dim=(ROWS_K + 7) // 8, block_dim=256,
    )
    ctx.synchronize()
    var got_h = List[Float32]()
    var got_32 = List[Float32]()
    with y5.map_to_host() as yh:
        for r in range(ROWS_K):
            got_h.append(Float32(yh[r]))
    with y5_32.map_to_host() as yh:
        for r in range(ROWS_K):
            got_32.append(yh[r])
    _check("gemv_q5_k", got_h, got_32, exp5)

    # --- gemv q3_k ---
    var w3 = ctx.enqueue_create_buffer[DType.uint8](ROWS_K * KBLOCKS * 110)
    var y3 = ctx.enqueue_create_buffer[DType.float16](ROWS_K)
    var y3_32 = ctx.enqueue_create_buffer[DType.float32](ROWS_K)
    var exp3 = List[Float32]()
    with w3.map_to_host() as wh, xk.map_to_host() as xh:
        for r in range(ROWS_K):
            var acc: Float32 = 0.0
            for bl in range(KBLOCKS):
                off = (r * KBLOCKS + bl) * 110
                d = Float16(0.01 + Float32((r + bl) % 7) * 0.004)
                for i in range(32):
                    wh[off + i] = UInt8((r * 47 + bl * 5 + i * 11) % 256)
                for i in range(64):
                    wh[off + 32 + i] = UInt8((r * 31 + bl * 17 + i * 13) % 256)
                for i in range(12):
                    wh[off + 96 + i] = UInt8((r * 53 + bl * 19 + i * 41 + 7) % 256)
                bits = (d).to_bits()
                wh[off + 108] = UInt8(bits & 0xFF)
                wh[(off + 108) + 1] = UInt8((bits >> 8) & 0xFF)
                # dq_q3_k reference: kmask scale unpack, then 2-bit codes with
                # the hmask -4 offset.
                var aux = InlineArray[UInt32, 4](fill=0)
                a0 = (
                    UInt32(wh[off + 96])
                    | (UInt32(wh[off + 97]) << 8)
                    | (UInt32(wh[off + 98]) << 16)
                    | (UInt32(wh[off + 99]) << 24)
                )
                a1 = (
                    UInt32(wh[off + 100])
                    | (UInt32(wh[off + 101]) << 8)
                    | (UInt32(wh[off + 102]) << 16)
                    | (UInt32(wh[off + 103]) << 24)
                )
                tmp = (
                    UInt32(wh[off + 104])
                    | (UInt32(wh[off + 105]) << 8)
                    | (UInt32(wh[off + 106]) << 16)
                    | (UInt32(wh[off + 107]) << 24)
                )
                aux[0] = (a0 & 0x0F0F0F0F) | ((tmp & 0x03030303) << 4)
                aux[1] = (a1 & 0x0F0F0F0F) | (((tmp >> 2) & 0x03030303) << 4)
                aux[2] = ((a0 >> 4) & 0x0F0F0F0F) | (((tmp >> 4) & 0x03030303) << 4)
                aux[3] = ((a1 >> 4) & 0x0F0F0F0F) | (((tmp >> 6) & 0x03030303) << 4)
                for e in range(256):
                    n = e // 128
                    rr = e % 128
                    s = rr // 32
                    half = (rr % 32) // 16
                    l = rr % 16
                    is_ = n * 8 + 2 * s + half
                    scv = Int((aux[is_ // 4] >> UInt32(8 * (is_ % 4))) & 0xFF) - 32
                    qb = Int(wh[off + 32 + n * 32 + half * 16 + l])
                    var q = (qb >> (2 * s)) & 3
                    mbit = 1 << (4 * n + s)
                    if Int(wh[off + half * 16 + l]) & mbit == 0:
                        q -= 4
                    acc += Float32(d) * Float32(scv) * Float32(q) * Float32(
                        xh[bl * 256 + e]
                    )
            exp3.append(acc)
    ctx.enqueue_function[gemv_q3_k_f16_v2](
        y3.unsafe_ptr(), w3.unsafe_ptr(), xk.unsafe_ptr(), COLS_K, ROWS_K,
        grid_dim=(ROWS_K + 7) // 8, block_dim=256,
    )
    ctx.enqueue_function[gemv_q3_k_out_f32_v2](
        y3_32.unsafe_ptr(), w3.unsafe_ptr(), xk.unsafe_ptr(), COLS_K, ROWS_K,
        grid_dim=(ROWS_K + 7) // 8, block_dim=256,
    )
    ctx.synchronize()
    got_h = List[Float32]()
    got_32 = List[Float32]()
    with y3.map_to_host() as yh:
        for r in range(ROWS_K):
            got_h.append(Float32(yh[r]))
    with y3_32.map_to_host() as yh:
        for r in range(ROWS_K):
            got_32.append(yh[r])
    _check("gemv_q3_k", got_h, got_32, exp3)

    # --- gemv q2_k ---
    var w2 = ctx.enqueue_create_buffer[DType.uint8](ROWS_K * KBLOCKS * 84)
    var y2 = ctx.enqueue_create_buffer[DType.float16](ROWS_K)
    var y2_32 = ctx.enqueue_create_buffer[DType.float32](ROWS_K)
    var exp2 = List[Float32]()
    with w2.map_to_host() as wh, xk.map_to_host() as xh:
        for r in range(ROWS_K):
            var acc: Float32 = 0.0
            for bl in range(KBLOCKS):
                off = (r * KBLOCKS + bl) * 84
                d = Float16(0.02 + Float32((r + bl) % 7) * 0.01)
                dmin = Float16(0.01 + Float32((r + 2 * bl) % 5) * 0.005)
                for i in range(16):
                    wh[off + i] = UInt8((r * 53 + bl * 19 + i * 41 + 7) % 256)
                for i in range(64):
                    wh[off + 16 + i] = UInt8((r * 31 + bl * 17 + i * 13) % 256)
                bits = (d).to_bits()
                wh[off + 80] = UInt8(bits & 0xFF)
                wh[(off + 80) + 1] = UInt8((bits >> 8) & 0xFF)
                bits = (dmin).to_bits()
                wh[off + 82] = UInt8(bits & 0xFF)
                wh[(off + 82) + 1] = UInt8((bits >> 8) & 0xFF)
                # dq_q2_k reference.
                for e in range(256):
                    n = e // 128
                    rr = e % 128
                    s = rr // 32
                    half = (rr % 32) // 16
                    l = rr % 16
                    is_ = n * 8 + 2 * s + half
                    scb = Int(wh[off + is_])
                    qb = Int(wh[off + 16 + n * 32 + half * 16 + l])
                    q = (qb >> (2 * s)) & 3
                    v = Float32(d) * Float32(scb & 0x0F) * Float32(q) - Float32(
                        dmin
                    ) * Float32(scb >> 4)
                    acc += v * Float32(xh[bl * 256 + e])
            exp2.append(acc)
    ctx.enqueue_function[gemv_q2_k_f16_v2](
        y2.unsafe_ptr(), w2.unsafe_ptr(), xk.unsafe_ptr(), COLS_K, ROWS_K,
        grid_dim=(ROWS_K + 7) // 8, block_dim=256,
    )
    ctx.enqueue_function[gemv_q2_k_out_f32_v2](
        y2_32.unsafe_ptr(), w2.unsafe_ptr(), xk.unsafe_ptr(), COLS_K, ROWS_K,
        grid_dim=(ROWS_K + 7) // 8, block_dim=256,
    )
    ctx.synchronize()
    got_h = List[Float32]()
    got_32 = List[Float32]()
    with y2.map_to_host() as yh:
        for r in range(ROWS_K):
            got_h.append(Float32(yh[r]))
    with y2_32.map_to_host() as yh:
        for r in range(ROWS_K):
            got_32.append(yh[r])
    _check("gemv_q2_k", got_h, got_32, exp2)

    # --- legacy 32-element formats ---
    comptime BLOCKS_L = COLS_K // 32
    for fmt in range(4):
        # fmt 0: q4_0 (18B), 1: q4_1 (20B), 2: q5_0 (22B), 3: q5_1 (24B)
        var bb = 18
        if fmt == 1:
            bb = 20
        if fmt == 2:
            bb = 22
        if fmt == 3:
            bb = 24
        var wl = ctx.enqueue_create_buffer[DType.uint8](ROWS_K * BLOCKS_L * bb)
        var yl = ctx.enqueue_create_buffer[DType.float16](ROWS_K)
        var yl_32 = ctx.enqueue_create_buffer[DType.float32](ROWS_K)
        var expl = List[Float32]()
        with wl.map_to_host() as wh, xk.map_to_host() as xh:
            for r in range(ROWS_K):
                var acc: Float32 = 0.0
                for bl in range(BLOCKS_L):
                    off = (r * BLOCKS_L + bl) * bb
                    d = Float16(0.02 + Float32((r + bl) % 7) * 0.01)
                    m = Float16(-0.05 + Float32((r + 2 * bl) % 5) * 0.03)
                    bits = (d).to_bits()
                    wh[off] = UInt8(bits & 0xFF)
                    wh[(off) + 1] = UInt8((bits >> 8) & 0xFF)
                    var qs_off = off + 2
                    var qh: UInt32 = 0
                    if fmt == 1:
                        bits = (m).to_bits()
                        wh[off + 2] = UInt8(bits & 0xFF)
                        wh[(off + 2) + 1] = UInt8((bits >> 8) & 0xFF)
                        qs_off = off + 4
                    if fmt == 2:
                        qh = UInt32((r * 2654435761 + bl * 40503) & 0xFFFFFFFF)
                        wh[off + 2] = UInt8(qh & 0xFF)
                        wh[off + 3] = UInt8((qh >> 8) & 0xFF)
                        wh[off + 4] = UInt8((qh >> 16) & 0xFF)
                        wh[off + 5] = UInt8((qh >> 24) & 0xFF)
                        qs_off = off + 6
                    if fmt == 3:
                        bits = (m).to_bits()
                        wh[off + 2] = UInt8(bits & 0xFF)
                        wh[(off + 2) + 1] = UInt8((bits >> 8) & 0xFF)
                        qh = UInt32((r * 2654435761 + bl * 40503) & 0xFFFFFFFF)
                        wh[off + 4] = UInt8(qh & 0xFF)
                        wh[off + 5] = UInt8((qh >> 8) & 0xFF)
                        wh[off + 6] = UInt8((qh >> 16) & 0xFF)
                        wh[off + 7] = UInt8((qh >> 24) & 0xFF)
                        qs_off = off + 8
                    for i in range(16):
                        wh[qs_off + i] = UInt8((r * 31 + bl * 17 + i * 13) % 256)
                    for j in range(16):
                        qb = Int(wh[qs_off + j])
                        lo = qb & 0x0F
                        hi = qb >> 4
                        xlo = Float32(xh[bl * 32 + j])
                        xhi = Float32(xh[bl * 32 + 16 + j])
                        if fmt == 0:
                            acc += Float32(d) * Float32(lo - 8) * xlo
                            acc += Float32(d) * Float32(hi - 8) * xhi
                        elif fmt == 1:
                            acc += (Float32(d) * Float32(lo) + Float32(m)) * xlo
                            acc += (Float32(d) * Float32(hi) + Float32(m)) * xhi
                        else:
                            var v1 = lo
                            if (qh >> UInt32(j)) & 1 != 0:
                                v1 += 16
                            var v2 = hi
                            if (qh >> UInt32(16 + j)) & 1 != 0:
                                v2 += 16
                            if fmt == 2:
                                acc += Float32(d) * Float32(v1 - 16) * xlo
                                acc += Float32(d) * Float32(v2 - 16) * xhi
                            else:
                                acc += (Float32(d) * Float32(v1) + Float32(m)) * xlo
                                acc += (Float32(d) * Float32(v2) + Float32(m)) * xhi
                expl.append(acc)
        if fmt == 0:
            ctx.enqueue_function[gemv_q4_0_f16_v2](
                yl.unsafe_ptr(), wl.unsafe_ptr(), xk.unsafe_ptr(), COLS_K, ROWS_K,
                grid_dim=(ROWS_K + 7) // 8, block_dim=256,
            )
            ctx.enqueue_function[gemv_q4_0_out_f32_v2](
                yl_32.unsafe_ptr(), wl.unsafe_ptr(), xk.unsafe_ptr(), COLS_K, ROWS_K,
                grid_dim=(ROWS_K + 7) // 8, block_dim=256,
            )
        elif fmt == 1:
            ctx.enqueue_function[gemv_q4_1_f16_v2](
                yl.unsafe_ptr(), wl.unsafe_ptr(), xk.unsafe_ptr(), COLS_K, ROWS_K,
                grid_dim=(ROWS_K + 7) // 8, block_dim=256,
            )
            ctx.enqueue_function[gemv_q4_1_out_f32_v2](
                yl_32.unsafe_ptr(), wl.unsafe_ptr(), xk.unsafe_ptr(), COLS_K, ROWS_K,
                grid_dim=(ROWS_K + 7) // 8, block_dim=256,
            )
        elif fmt == 2:
            ctx.enqueue_function[gemv_q5_0_f16_v2](
                yl.unsafe_ptr(), wl.unsafe_ptr(), xk.unsafe_ptr(), COLS_K, ROWS_K,
                grid_dim=(ROWS_K + 7) // 8, block_dim=256,
            )
            ctx.enqueue_function[gemv_q5_0_out_f32_v2](
                yl_32.unsafe_ptr(), wl.unsafe_ptr(), xk.unsafe_ptr(), COLS_K, ROWS_K,
                grid_dim=(ROWS_K + 7) // 8, block_dim=256,
            )
        else:
            ctx.enqueue_function[gemv_q5_1_f16_v2](
                yl.unsafe_ptr(), wl.unsafe_ptr(), xk.unsafe_ptr(), COLS_K, ROWS_K,
                grid_dim=(ROWS_K + 7) // 8, block_dim=256,
            )
            ctx.enqueue_function[gemv_q5_1_out_f32_v2](
                yl_32.unsafe_ptr(), wl.unsafe_ptr(), xk.unsafe_ptr(), COLS_K, ROWS_K,
                grid_dim=(ROWS_K + 7) // 8, block_dim=256,
            )
        ctx.synchronize()
        got_h = List[Float32]()
        got_32 = List[Float32]()
        with yl.map_to_host() as yh:
            for r in range(ROWS_K):
                got_h.append(Float32(yh[r]))
        with yl_32.map_to_host() as yh:
            for r in range(ROWS_K):
                got_32.append(yh[r])
        if fmt == 0:
            _check("gemv_q4_0", got_h, got_32, expl)
        elif fmt == 1:
            _check("gemv_q4_1", got_h, got_32, expl)
        elif fmt == 2:
            _check("gemv_q5_0", got_h, got_32, expl)
        else:
            _check("gemv_q5_1", got_h, got_32, expl)

    print("ALL EXTENDED QUANT GEMV CHECKS PASSED")
