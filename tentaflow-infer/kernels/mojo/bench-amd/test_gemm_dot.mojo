# =============================================================================
# Plik: test_gemm_dot.mojo
# Opis: Sprawdza poprawność `gemm_f16_dot2` wobec referencji na hoście i mierzy
#       jego przepustowość na kształtach prefillu.
# Przykład: pixi run mojo run -I . bench-amd/test_gemm_dot.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from std.math import isclose
from std.memory import bitcast
from src.gemm_dot import (
    gemm_f16_dot2_impl,
    gemm_q8_0_dot4_impl,
    gemm_q4_k_dot4_impl,
    gemm_q6_k_dot4_impl,
    gemm_nvfp4_dot4_impl,
)

comptime ITERS = 10


def check[BM: Int, BN: Int, TM: Int, TN: Int](ctx: DeviceContext, tokens: Int, rows: Int, cols: Int) raises:
    var xh = ctx.enqueue_create_host_buffer[DType.float16](tokens * cols)
    var wh = ctx.enqueue_create_host_buffer[DType.float16](rows * cols)
    ctx.synchronize()
    for t in range(tokens):
        for k in range(cols):
            xh[t * cols + k] = Float16(Float32((t * 7 + k * 3) % 17) * 0.0625 - 0.5)
    for r in range(rows):
        for k in range(cols):
            wh[r * cols + k] = Float16(Float32((r * 5 + k * 11) % 13) * 0.0625 - 0.375)

    var xd = ctx.enqueue_create_buffer[DType.float16](tokens * cols)
    var wd = ctx.enqueue_create_buffer[DType.float16](rows * cols)
    var yd = ctx.enqueue_create_buffer[DType.float16](tokens * rows)
    ctx.enqueue_copy(xd, xh)
    ctx.enqueue_copy(wd, wh)
    ctx.synchronize()

    grid_x = (rows + BN - 1) // BN
    grid_y = (tokens + BM - 1) // BM
    ctx.enqueue_function[gemm_f16_dot2_impl[BM, BN, TM, TN]](
        yd.unsafe_ptr(),
        wd.unsafe_ptr(),
        xd.unsafe_ptr(),
        cols,
        rows,
        tokens,
        grid_dim=(grid_x, grid_y),
        block_dim=(BM // TM) * (BN // TN),
    )
    ctx.synchronize()

    var worst: Float32 = 0.0
    with yd.map_to_host() as got:
        for t in range(tokens):
            for r in range(rows):
                var expect: Float32 = 0.0
                for k in range(cols):
                    expect += Float32(xh[t * cols + k]) * Float32(wh[r * cols + k])
                have = Float32(got[t * rows + r])
                denom = abs(expect) if abs(expect) > 1.0 else 1.0
                err = abs(have - expect) / denom
                if err > worst:
                    worst = err
    print("T=", tokens, "rows=", rows, "cols=", cols, "wzgledny blad max:", worst)
    # Wyjście jest f16, więc próg to zaokrąglenie f16 (2^-11) z zapasem na
    # kolejność sumowania; referencja liczy w f32 w innej kolejności.
    if worst > 0.002:
        raise Error("gemm_f16_dot2 poza tolerancja f16")


def bench[BM: Int, BN: Int, TM: Int, TN: Int](label: String, ctx: DeviceContext, tokens: Int, rows: Int, cols: Int) raises:
    var xd = ctx.enqueue_create_buffer[DType.float16](tokens * cols)
    var wd = ctx.enqueue_create_buffer[DType.float16](rows * cols)
    var yd = ctx.enqueue_create_buffer[DType.float16](tokens * rows)
    ctx.synchronize()
    grid_x = (rows + BN - 1) // BN
    grid_y = (tokens + BM - 1) // BM
    for _ in range(3):
        ctx.enqueue_function[gemm_f16_dot2_impl[BM, BN, TM, TN]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xd.unsafe_ptr(), cols, rows, tokens,
            grid_dim=(grid_x, grid_y), block_dim=(BM // TM) * (BN // TN),
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_f16_dot2_impl[BM, BN, TM, TN]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xd.unsafe_ptr(), cols, rows, tokens,
            grid_dim=(grid_x, grid_y), block_dim=(BM // TM) * (BN // TN),
        )
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    flops = 2.0 * Float64(tokens) * Float64(rows) * Float64(cols)
    print(
        label, "T=", tokens, "rows=", rows, "cols=", cols,
        Int(flops / dt / 1e12), "TFLOPS", Int(dt * 1e6), "us",
    )


def check_q8[BM: Int, BN: Int, TM: Int, TN: Int, KB: Int](
    ctx: DeviceContext, tokens: Int, rows: Int, cols: Int
) raises:
    nb = cols // 32
    var wh = ctx.enqueue_create_host_buffer[DType.uint8](rows * nb * 34)
    var xh = ctx.enqueue_create_host_buffer[DType.int8](tokens * cols)
    var dh = ctx.enqueue_create_host_buffer[DType.float32](nb * tokens)
    ctx.synchronize()
    for r in range(rows):
        for b in range(nb):
            base = (r * nb + b) * 34
            scale = Float16(0.00390625 * Float32(1 + (r + b) % 7))
            bits = bitcast[DType.uint16](scale)
            wh[base] = UInt8(bits & 0xFF)
            wh[base + 1] = UInt8(bits >> 8)
            for i in range(32):
                wh[base + 2 + i] = bitcast[DType.uint8](
                    Int8(((r * 13 + b * 5 + i * 3) % 255) - 127)
                )
    for t in range(tokens):
        for k in range(cols):
            xh[t * cols + k] = Int8(((t * 7 + k * 11) % 255) - 127)
    for b in range(nb):
        for t in range(tokens):
            dh[b * tokens + t] = 0.0078125 * Float32(1 + (b + t) % 5)
    var wd = ctx.enqueue_create_buffer[DType.uint8](rows * nb * 34)
    var xd = ctx.enqueue_create_buffer[DType.int8](tokens * cols)
    var dd = ctx.enqueue_create_buffer[DType.float32](nb * tokens)
    var sd = ctx.enqueue_create_buffer[DType.float32](nb * tokens)
    var yd = ctx.enqueue_create_buffer[DType.float16](tokens * rows)
    ctx.enqueue_copy(wd, wh)
    ctx.enqueue_copy(xd, xh)
    ctx.enqueue_copy(dd, dh)
    ctx.synchronize()
    ctx.enqueue_function[gemm_q8_0_dot4_impl[BM, BN, TM, TN, KB]](
        yd.unsafe_ptr(), wd.unsafe_ptr(), xd.unsafe_ptr(), dd.unsafe_ptr(),
        sd.unsafe_ptr(), cols, rows, tokens,
        grid_dim=((rows + BN - 1) // BN, (tokens + BM - 1) // BM),
        block_dim=(BM // TM) * (BN // TN),
    )
    ctx.synchronize()
    var worst: Float32 = 0.0
    with yd.map_to_host() as got:
        for t in range(tokens):
            for r in range(rows):
                var expect: Float32 = 0.0
                for b in range(nb):
                    base = (r * nb + b) * 34
                    bits = UInt16(wh[base]) | (UInt16(wh[base + 1]) << 8)
                    dw = Float32(bitcast[DType.float16](bits))
                    var isum: Int32 = 0
                    for i in range(32):
                        code = bitcast[DType.int8](wh[base + 2 + i])
                        isum += Int32(code) * Int32(xh[t * cols + b * 32 + i])
                    expect += dh[b * tokens + t] * dw * Float32(isum)
                have = Float32(got[t * rows + r])
                denom = abs(expect) if abs(expect) > 1.0 else 1.0
                err = abs(have - expect) / denom
                if err > worst:
                    worst = err
    print("Q8 T=", tokens, "rows=", rows, "cols=", cols, "blad max:", worst)
    if worst > 0.002:
        raise Error("gemm_q8_0_dot4 poza tolerancja f16")


def bench_q8[BM: Int, BN: Int, TM: Int, TN: Int, KB: Int](
    label: String, ctx: DeviceContext, tokens: Int, rows: Int, cols: Int
) raises:
    nb = cols // 32
    var wd = ctx.enqueue_create_buffer[DType.uint8](rows * nb * 34)
    var xd = ctx.enqueue_create_buffer[DType.int8](tokens * cols)
    var dd = ctx.enqueue_create_buffer[DType.float32](nb * tokens)
    var sd = ctx.enqueue_create_buffer[DType.float32](nb * tokens)
    var yd = ctx.enqueue_create_buffer[DType.float16](tokens * rows)
    ctx.synchronize()
    grid = ((rows + BN - 1) // BN, (tokens + BM - 1) // BM)
    blk = (BM // TM) * (BN // TN)
    for _ in range(3):
        ctx.enqueue_function[gemm_q8_0_dot4_impl[BM, BN, TM, TN, KB]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xd.unsafe_ptr(), dd.unsafe_ptr(),
            sd.unsafe_ptr(), cols, rows, tokens, grid_dim=grid, block_dim=blk,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q8_0_dot4_impl[BM, BN, TM, TN, KB]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xd.unsafe_ptr(), dd.unsafe_ptr(),
            sd.unsafe_ptr(), cols, rows, tokens, grid_dim=grid, block_dim=blk,
        )
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    ops = 2.0 * Float64(tokens) * Float64(rows) * Float64(cols)
    print(
        label, "T=", tokens, "rows=", rows, "cols=", cols,
        Int(ops / dt / 1e12), "TOPS", Int(dt * 1e6), "us",
    )


def check_q4k[BM: Int, BN: Int, TM: Int, TN: Int, KB: Int](
    ctx: DeviceContext, tokens: Int, rows: Int, cols: Int
) raises:
    """Referencja liczona na hoście dokładnie tym samym wzorem co kernel."""
    nsuper = cols // 256
    nb = cols // 32
    var wh = ctx.enqueue_create_host_buffer[DType.uint8](rows * nsuper * 144)
    var xh = ctx.enqueue_create_host_buffer[DType.int8](tokens * cols)
    var dh = ctx.enqueue_create_host_buffer[DType.float32](nb * tokens)
    var sh = ctx.enqueue_create_host_buffer[DType.float32](nb * tokens)
    ctx.synchronize()
    for r in range(rows):
        for sb in range(nsuper):
            base = (r * nsuper + sb) * 144
            d = Float32(0.00390625 * Float32(1 + (r + sb) % 5)).cast[DType.float16]()
            dmin = Float32(0.001953125 * Float32(1 + (r + sb) % 3)).cast[DType.float16]()
            db = bitcast[DType.uint16](d)
            mb = bitcast[DType.uint16](dmin)
            wh[base] = UInt8(db & 0xFF)
            wh[base + 1] = UInt8(db >> 8)
            wh[base + 2] = UInt8(mb & 0xFF)
            wh[base + 3] = UInt8(mb >> 8)
            for i in range(12):
                wh[base + 4 + i] = UInt8((r * 7 + sb * 3 + i * 5) % 256)
            for i in range(128):
                wh[base + 16 + i] = UInt8((r * 11 + sb * 13 + i * 3) % 256)
    for t in range(tokens):
        for k in range(cols):
            xh[t * cols + k] = Int8(((t * 5 + k * 7) % 255) - 127)
    for b in range(nb):
        for t in range(tokens):
            dh[b * tokens + t] = 0.0078125 * Float32(1 + (b + t) % 5)
            var isum: Int32 = 0
            for i in range(32):
                isum += Int32(xh[t * cols + b * 32 + i])
            sh[b * tokens + t] = dh[b * tokens + t] * Float32(isum)

    var wd = ctx.enqueue_create_buffer[DType.uint8](rows * nsuper * 144)
    var xdb = ctx.enqueue_create_buffer[DType.int8](tokens * cols)
    var ddb = ctx.enqueue_create_buffer[DType.float32](nb * tokens)
    var sdb = ctx.enqueue_create_buffer[DType.float32](nb * tokens)
    var yd = ctx.enqueue_create_buffer[DType.float16](tokens * rows)
    ctx.enqueue_copy(wd, wh)
    ctx.enqueue_copy(xdb, xh)
    ctx.enqueue_copy(ddb, dh)
    ctx.enqueue_copy(sdb, sh)
    ctx.synchronize()
    ctx.enqueue_function[gemm_q4_k_dot4_impl[BM, BN, TM, TN, KB]](
        yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
        sdb.unsafe_ptr(), cols, rows, tokens,
        grid_dim=((rows + BN - 1) // BN, (tokens + BM - 1) // BM),
        block_dim=(BM // TM) * (BN // TN),
    )
    ctx.synchronize()
    var worst: Float32 = 0.0
    with yd.map_to_host() as got:
        for t in range(tokens):
            for r in range(rows):
                var expect: Float32 = 0.0
                for b in range(nb):
                    sb = b // 8
                    j = b % 8
                    base = (r * nsuper + sb) * 144
                    db2 = UInt16(wh[base]) | (UInt16(wh[base + 1]) << 8)
                    mb2 = UInt16(wh[base + 2]) | (UInt16(wh[base + 3]) << 8)
                    dv = bitcast[DType.float16](db2).cast[DType.float32]()
                    dmv = bitcast[DType.float16](mb2).cast[DType.float32]()
                    var sc: Int
                    var mn: Int
                    if j < 4:
                        sc = Int(wh[base + 4 + j] & 63)
                        mn = Int(wh[base + 8 + j] & 63)
                    else:
                        sc = Int(
                            (wh[base + 8 + j] & 0x0F)
                            | ((wh[base + j] >> 6) << 4)
                        )
                        mn = Int(
                            (wh[base + 8 + j] >> 4)
                            | ((wh[base + 4 + j] >> 6) << 4)
                        )
                    var isum: Int32 = 0
                    for i in range(32):
                        packed = wh[base + 16 + 32 * (j // 2) + i]
                        q = Int32(
                            packed & 0x0F
                        ) if j % 2 == 0 else Int32(packed >> 4)
                        isum += q * Int32(xh[t * cols + b * 32 + i])
                    expect += (
                        dh[b * tokens + t] * dv * Float32(sc) * Float32(isum)
                        - dmv * Float32(mn) * sh[b * tokens + t]
                    )
                have = Float32(got[t * rows + r])
                denom = abs(expect) if abs(expect) > 1.0 else 1.0
                err = abs(have - expect) / denom
                if err > worst:
                    worst = err
    print("Q4K T=", tokens, "rows=", rows, "cols=", cols, "blad max:", worst)
    if worst > 0.003:
        raise Error("gemm_q4_k_dot4 poza tolerancja f16")


def bench_q4k[BM: Int, BN: Int, TM: Int, TN: Int, KB: Int](
    label: String, ctx: DeviceContext, tokens: Int, rows: Int, cols: Int
) raises:
    nsuper = cols // 256
    nb = cols // 32
    var wd = ctx.enqueue_create_buffer[DType.uint8](rows * nsuper * 144)
    var xdb = ctx.enqueue_create_buffer[DType.int8](tokens * cols)
    var ddb = ctx.enqueue_create_buffer[DType.float32](nb * tokens)
    var sdb = ctx.enqueue_create_buffer[DType.float32](nb * tokens)
    var yd = ctx.enqueue_create_buffer[DType.float16](tokens * rows)
    ctx.synchronize()
    grid = ((rows + BN - 1) // BN, (tokens + BM - 1) // BM)
    blk = (BM // TM) * (BN // TN)
    for _ in range(3):
        ctx.enqueue_function[gemm_q4_k_dot4_impl[BM, BN, TM, TN, KB]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(),
            ddb.unsafe_ptr(), sdb.unsafe_ptr(), cols, rows, tokens,
            grid_dim=grid, block_dim=blk,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q4_k_dot4_impl[BM, BN, TM, TN, KB]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(),
            ddb.unsafe_ptr(), sdb.unsafe_ptr(), cols, rows, tokens,
            grid_dim=grid, block_dim=blk,
        )
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    ops = 2.0 * Float64(tokens) * Float64(rows) * Float64(cols)
    print(
        label, "T=", tokens, "rows=", rows, "cols=", cols,
        Int(ops / dt / 1e12), "TOPS", Int(dt * 1e6), "us",
    )


def check_q6k[BM: Int, BN: Int, TM: Int, TN: Int, KB: Int](
    ctx: DeviceContext, tokens: Int, rows: Int, cols: Int
) raises:
    nsuper = cols // 256
    nb = cols // 32
    var wh = ctx.enqueue_create_host_buffer[DType.uint8](rows * nsuper * 210)
    var xh = ctx.enqueue_create_host_buffer[DType.int8](tokens * cols)
    var dh = ctx.enqueue_create_host_buffer[DType.float32](nb * tokens)
    var sh = ctx.enqueue_create_host_buffer[DType.float32](nb * tokens)
    ctx.synchronize()
    for r in range(rows):
        for sb in range(nsuper):
            base = (r * nsuper + sb) * 210
            for i in range(192):
                wh[base + i] = UInt8((r * 11 + sb * 7 + i * 5) % 256)
            for i in range(16):
                wh[base + 192 + i] = bitcast[DType.uint8](
                    Int8(((r * 3 + sb + i * 9) % 127) - 63)
                )
            dv = Float32(0.001953125 * Float32(1 + (r + sb) % 4)).cast[
                DType.float16
            ]()
            db = bitcast[DType.uint16](dv)
            wh[base + 208] = UInt8(db & 0xFF)
            wh[base + 209] = UInt8(db >> 8)
    for t in range(tokens):
        for k in range(cols):
            xh[t * cols + k] = Int8(((t * 5 + k * 7) % 255) - 127)
    for b in range(nb):
        for t in range(tokens):
            dh[b * tokens + t] = 0.0078125 * Float32(1 + (b + t) % 5)
            sh[b * tokens + t] = 0.0

    var wd = ctx.enqueue_create_buffer[DType.uint8](rows * nsuper * 210)
    var xdb = ctx.enqueue_create_buffer[DType.int8](tokens * cols)
    var ddb = ctx.enqueue_create_buffer[DType.float32](nb * tokens)
    var sdb = ctx.enqueue_create_buffer[DType.float32](nb * tokens)
    var yd = ctx.enqueue_create_buffer[DType.float16](tokens * rows)
    ctx.enqueue_copy(wd, wh)
    ctx.enqueue_copy(xdb, xh)
    ctx.enqueue_copy(ddb, dh)
    ctx.enqueue_copy(sdb, sh)
    ctx.synchronize()
    ctx.enqueue_function[gemm_q6_k_dot4_impl[BM, BN, TM, TN, KB]](
        yd.unsafe_ptr(), wd.unsafe_ptr(), xdb.unsafe_ptr(), ddb.unsafe_ptr(),
        sdb.unsafe_ptr(), cols, rows, tokens,
        grid_dim=((rows + BN - 1) // BN, (tokens + BM - 1) // BM),
        block_dim=(BM // TM) * (BN // TN),
    )
    ctx.synchronize()
    var worst: Float32 = 0.0
    with yd.map_to_host() as got:
        for t in range(tokens):
            for r in range(rows):
                var expect: Float32 = 0.0
                for c in range(cols):
                    sb = c // 256
                    cc = c % 256
                    base = (r * nsuper + sb) * 210
                    half = cc // 128
                    g = (cc % 128) // 32
                    l = cc % 32
                    db2 = UInt16(wh[base + 208]) | (
                        UInt16(wh[base + 209]) << 8
                    )
                    dv2 = bitcast[DType.float16](db2).cast[DType.float32]()
                    sc = Int32(
                        bitcast[DType.int8](
                            wh[base + 192 + half * 8 + l // 16 + 2 * g]
                        )
                    )
                    low = wh[base + half * 64 + l + (g % 2) * 32]
                    high = wh[base + 128 + half * 32 + l]
                    q = ((Int32(low) >> Int32((g // 2) * 4)) & 0x0F) | (
                        ((Int32(high) >> Int32(2 * g)) & 0x03) << 4
                    )
                    expect += (
                        dh[(c // 32) * tokens + t]
                        * dv2
                        * Float32(sc)
                        * Float32((q - 32) * Int32(xh[t * cols + c]))
                    )
                have = Float32(got[t * rows + r])
                denom = abs(expect) if abs(expect) > 1.0 else 1.0
                err = abs(have - expect) / denom
                if err > worst:
                    worst = err
    print("Q6K T=", tokens, "rows=", rows, "cols=", cols, "blad max:", worst)
    if worst > 0.003:
        raise Error("gemm_q6_k_dot4 poza tolerancja f16")


def bench_nvfp4[BM: Int, BN: Int, TM: Int, TN: Int, KB: Int](
    label: String, ctx: DeviceContext, tokens: Int, rows: Int, cols: Int
) raises:
    nb = cols // 32
    ngroups = cols // 16
    var wd = ctx.enqueue_create_buffer[DType.uint8](rows * (cols // 2))
    var sd8 = ctx.enqueue_create_buffer[DType.uint8](rows * ngroups)
    var xdb = ctx.enqueue_create_buffer[DType.int8](tokens * cols)
    var ddb = ctx.enqueue_create_buffer[DType.float32](nb * tokens)
    var yd = ctx.enqueue_create_buffer[DType.float16](tokens * rows)
    ctx.synchronize()
    grid = ((rows + BN - 1) // BN, (tokens + BM - 1) // BM)
    blk = (BM // TM) * (BN // TN)
    for _ in range(3):
        ctx.enqueue_function[gemm_nvfp4_dot4_impl[BM, BN, TM, TN, KB]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), sd8.unsafe_ptr(),
            xdb.unsafe_ptr(), ddb.unsafe_ptr(), cols, rows, tokens,
            Float32(1.0), grid_dim=grid, block_dim=blk,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_nvfp4_dot4_impl[BM, BN, TM, TN, KB]](
            yd.unsafe_ptr(), wd.unsafe_ptr(), sd8.unsafe_ptr(),
            xdb.unsafe_ptr(), ddb.unsafe_ptr(), cols, rows, tokens,
            Float32(1.0), grid_dim=grid, block_dim=blk,
        )
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    ops = 2.0 * Float64(tokens) * Float64(rows) * Float64(cols)
    print(
        label, "T=", tokens, "rows=", rows, "cols=", cols,
        Int(ops / dt / 1e12), "TOPS", Int(dt * 1e6), "us",
    )


def check_nvfp4[BM: Int, BN: Int, TM: Int, TN: Int, KB: Int](
    ctx: DeviceContext, tokens: Int, rows: Int, cols: Int
) raises:
    """Referencja hosta dekoduje e2m1 i e4m3 niezaleznie od kodu kernela."""
    nb = cols // 32
    ngroups = cols // 16
    var wh = ctx.enqueue_create_host_buffer[DType.uint8](rows * (cols // 2))
    var sh8 = ctx.enqueue_create_host_buffer[DType.uint8](rows * ngroups)
    var xh = ctx.enqueue_create_host_buffer[DType.int8](tokens * cols)
    var dh = ctx.enqueue_create_host_buffer[DType.float32](nb * tokens)
    ctx.synchronize()
    for r in range(rows):
        for i in range(cols // 2):
            wh[r * (cols // 2) + i] = UInt8((r * 13 + i * 7) % 256)
        for g in range(ngroups):
            # Wykladniki 3..12 (bez subnormalnych i bez NaN), rozne mantysy.
            wh_exp = 3 + (r + g) % 10
            wh_man = (r * 3 + g) % 8
            sh8[r * ngroups + g] = UInt8((wh_exp << 3) | wh_man)
    for t in range(tokens):
        for k in range(cols):
            xh[t * cols + k] = Int8(((t * 7 + k * 5) % 255) - 127)
    for b in range(nb):
        for t in range(tokens):
            dh[b * tokens + t] = 0.0078125 * Float32(1 + (b + t) % 5)

    var wd = ctx.enqueue_create_buffer[DType.uint8](rows * (cols // 2))
    var sd8 = ctx.enqueue_create_buffer[DType.uint8](rows * ngroups)
    var xdb = ctx.enqueue_create_buffer[DType.int8](tokens * cols)
    var ddb = ctx.enqueue_create_buffer[DType.float32](nb * tokens)
    var yd = ctx.enqueue_create_buffer[DType.float16](tokens * rows)
    ctx.enqueue_copy(wd, wh)
    ctx.enqueue_copy(sd8, sh8)
    ctx.enqueue_copy(xdb, xh)
    ctx.enqueue_copy(ddb, dh)
    ctx.synchronize()
    inv_global = Float32(0.75)
    ctx.enqueue_function[gemm_nvfp4_dot4_impl[BM, BN, TM, TN, KB]](
        yd.unsafe_ptr(), wd.unsafe_ptr(), sd8.unsafe_ptr(), xdb.unsafe_ptr(),
        ddb.unsafe_ptr(), cols, rows, tokens, inv_global,
        grid_dim=((rows + BN - 1) // BN, (tokens + BM - 1) // BM),
        block_dim=(BM // TM) * (BN // TN),
    )
    ctx.synchronize()
    mags = SIMD[DType.float32, 8](0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0)
    var worst: Float32 = 0.0
    with yd.map_to_host() as got:
        for t in range(tokens):
            for r in range(rows):
                var expect: Float32 = 0.0
                for c in range(cols):
                    byte = wh[r * (cols // 2) + c // 2]
                    nib = (
                        Int32(byte & 0x0F) if c % 2
                        == 0 else Int32(byte >> 4)
                    )
                    value = mags[Int(nib & 0x07)]
                    if nib >= 8:
                        value = -value
                    sbyte = sh8[r * ngroups + c // 16]
                    exponent = Int32((sbyte >> 3) & 0x0F)
                    mantissa = Int32(sbyte & 0x07)
                    var power: Float32 = 1.0
                    for _ in range(abs(Int(exponent) - 7)):
                        power = power * 2.0
                    if Int(exponent) - 7 < 0:
                        power = 1.0 / power
                    gscale = (1.0 + Float32(mantissa) / 8.0) * power
                    expect += (
                        value
                        * gscale
                        * inv_global
                        * dh[(c // 32) * tokens + t]
                        * Float32(xh[t * cols + c])
                    )
                have = Float32(got[t * rows + r])
                denom = abs(expect) if abs(expect) > 1.0 else 1.0
                err = abs(have - expect) / denom
                if err > worst:
                    worst = err
    print("NVFP4 T=", tokens, "rows=", rows, "cols=", cols, "blad max:", worst)
    if worst > 0.003:
        raise Error("gemm_nvfp4_dot4 poza tolerancja f16")


def main() raises:
    var ctx = DeviceContext()
    print("arch:", ctx.arch_name())
    check[64, 64, 4, 4](ctx, 64, 64, 64)
    check[64, 64, 4, 4](ctx, 37, 70, 104)
    check[64, 64, 4, 4](ctx, 65, 129, 4096)
    check[128, 64, 8, 4](ctx, 200, 130, 1024)
    check[128, 128, 8, 4](ctx, 300, 300, 1024)
    check[192, 128, 8, 4](ctx, 400, 300, 1024)
    check[128, 128, 8, 8](ctx, 300, 300, 1024)
    check[256, 64, 8, 8](ctx, 300, 130, 1024)
    check[256, 128, 8, 8](ctx, 300, 300, 1024)
    check_q8[64, 64, 4, 4, 1](ctx, 64, 64, 64)
    check_q8[64, 64, 4, 4, 4](ctx, 64, 64, 64)
    check_q8[128, 64, 8, 4, 2](ctx, 200, 130, 1024)
    check_q8[128, 128, 8, 4, 4](ctx, 130, 130, 160)
    check_q8[128, 64, 8, 4, 4](ctx, 200, 130, 1024)
    check_q8[128, 128, 8, 4, 4](ctx, 300, 300, 1024)
    check_q8[192, 128, 8, 4, 4](ctx, 300, 300, 1024)
    check_q4k[64, 64, 4, 4, 4](ctx, 64, 64, 256)
    check_q4k[128, 64, 8, 4, 2](ctx, 200, 130, 512)
    check_q4k[128, 128, 8, 4, 2](ctx, 300, 300, 1024)
    check_q6k[64, 64, 4, 4, 4](ctx, 64, 64, 256)
    check_q6k[128, 64, 8, 4, 2](ctx, 200, 130, 512)
    check_nvfp4[64, 64, 4, 4, 4](ctx, 64, 64, 64)
    check_nvfp4[128, 64, 8, 4, 2](ctx, 200, 130, 512)
    print("--- kafle na kształtach warstw ---")
    # qwen0.6B: 1024/3072. 7B: 4096/14336.
    bench[64, 64, 4, 4]("64x64  ", ctx, 1024, 4096, 4096)
    bench[128, 64, 8, 4]("128x64 ", ctx, 1024, 4096, 4096)
    bench[128, 128, 8, 8]("128x128", ctx, 1024, 4096, 4096)
    bench[256, 64, 8, 8]("256x64 ", ctx, 1024, 4096, 4096)
    bench[128, 128, 8, 4]("128x128/512w", ctx, 1024, 4096, 4096)
    bench[256, 128, 8, 8]("256x128/512w", ctx, 1024, 4096, 4096)
    bench[128, 256, 8, 8]("128x256/512w", ctx, 1024, 4096, 4096)
    bench[64, 128, 4, 4]("64x128/512w ", ctx, 1024, 4096, 4096)
    bench[128, 128, 4, 4]("128x128/1024w", ctx, 1024, 4096, 4096)
    bench[128, 64, 4, 4]("128x64/512w ", ctx, 1024, 4096, 4096)
    bench[192, 128, 8, 4]("192x128/768w", ctx, 1024, 4096, 4096)
    print("--- Q8_0 int8 dot4 ---")
    bench_q8[64, 64, 4, 4, 4]("q8 64x64  ", ctx, 1024, 4096, 4096)
    bench_q8[128, 64, 8, 4, 4]("q8 128x64 ", ctx, 1024, 4096, 4096)
    bench_q8[128, 128, 8, 4, 4]("q8 128x128", ctx, 1024, 4096, 4096)
    bench_q8[128, 128, 8, 4, 1]("q8 128x128 KB1", ctx, 1024, 4096, 4096)
    bench_q8[128, 128, 8, 4, 2]("q8 128x128 KB2", ctx, 1024, 4096, 4096)
    print("--- Q4_K int8 dot4 ---")
    bench_q4k[128, 64, 8, 4, 2]("q4k 128x64 ", ctx, 1024, 4096, 4096)
    bench_q4k[128, 128, 8, 4, 2]("q4k 128x128", ctx, 1024, 4096, 4096)
    bench_q4k[128, 128, 8, 4, 2]("q4k 128x128", ctx, 1024, 14336, 4096)
    print("--- NVFP4 int8 dot4 ---")
    bench_nvfp4[128, 64, 8, 4, 2]("nvfp4 128x64", ctx, 1024, 4096, 4096)
    bench_nvfp4[128, 64, 8, 4, 2]("nvfp4 128x64", ctx, 1024, 11264, 4096)
    bench_q8[128, 128, 8, 4, 4]("q8 128x128", ctx, 1024, 14336, 4096)
    bench_q8[128, 128, 8, 4, 4]("q8 128x128", ctx, 512, 3072, 1024)
    print("--- najlepszy kafel na pozostalych kształtach ---")
    bench[128, 128, 8, 8]("128x128", ctx, 512, 1024, 1024)
    bench[128, 128, 8, 8]("128x128", ctx, 512, 3072, 1024)
    bench[128, 128, 8, 8]("128x128", ctx, 1024, 14336, 4096)
