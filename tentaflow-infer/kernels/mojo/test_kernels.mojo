# ===== File: test_kernels.mojo — on-GPU numeric sanity for kernel sources =====
# Fast feedback while editing kernels: runs each kernel on deterministic data
# and compares against scalar CPU math computed here. The authoritative golden
# tests live in Rust (forge-kernels) against forge-formats references.

from std.gpu.host import DeviceContext
from std.math import rsqrt, exp, sqrt, cos, sin, pow
from src.norm import rmsnorm_f16
from src.activation import silu_mul_f16
from src.rope import rope_neox_f16
from src.gemv import gemv_q8_0_f16, gemv_f16

comptime ROWS = 4
comptime COLS = 1024
comptime EPS: Float32 = 1e-6


def _fill(i: Int) -> Float32:
    # Deterministic pseudo-data with sign changes and magnitude variation.
    return Float32((i * 37 % 19) - 9) * 0.25


def main() raises:
    var ctx = DeviceContext()

    # --- rmsnorm ---
    var x = ctx.enqueue_create_buffer[DType.float16](ROWS * COLS)
    var w = ctx.enqueue_create_buffer[DType.float16](COLS)
    var y = ctx.enqueue_create_buffer[DType.float16](ROWS * COLS)
    with x.map_to_host() as xh, w.map_to_host() as wh:
        for i in range(ROWS * COLS):
            xh[i] = Float16(_fill(i))
        for i in range(COLS):
            wh[i] = Float16(1.0 + Float32(i % 5) * 0.1)
    ctx.enqueue_function[rmsnorm_f16](
        y.unsafe_ptr(), x.unsafe_ptr(), w.unsafe_ptr(), COLS, EPS,
        grid_dim=ROWS, block_dim=256,
    )
    ctx.synchronize()

    var max_err: Float32 = 0.0
    with y.map_to_host() as yh:
        for r in range(ROWS):
            var ss: Float32 = 0.0
            for c in range(COLS):
                v = _fill(r * COLS + c)
                ss += v * v
            inv = rsqrt(ss / Float32(COLS) + EPS)
            for c in range(COLS):
                expected = _fill(r * COLS + c) * inv * (1.0 + Float32(c % 5) * 0.1)
                got = Float32(yh[r * COLS + c])
                err = abs(got - expected)
                if err > max_err:
                    max_err = err
    print("rmsnorm max_err:", max_err)
    if max_err > 0.01:
        raise Error("rmsnorm_f16 numeric check FAILED")

    # --- silu_mul ---
    comptime N = 4096
    var g = ctx.enqueue_create_buffer[DType.float16](N)
    var u = ctx.enqueue_create_buffer[DType.float16](N)
    var o = ctx.enqueue_create_buffer[DType.float16](N)
    with g.map_to_host() as gh, u.map_to_host() as uh:
        for i in range(N):
            gh[i] = Float16(_fill(i) * 0.5)
            uh[i] = Float16(_fill(i + 7) * 0.5)
    ctx.enqueue_function[silu_mul_f16](
        o.unsafe_ptr(), g.unsafe_ptr(), u.unsafe_ptr(), N,
        grid_dim=(N + 255) // 256, block_dim=256,
    )
    ctx.synchronize()

    max_err = 0.0
    with o.map_to_host() as oh:
        for i in range(N):
            gv = Float32(Float16(_fill(i) * 0.5))
            uv = Float32(Float16(_fill(i + 7) * 0.5))
            expected = gv / (1.0 + exp(-gv)) * uv
            err = abs(Float32(oh[i]) - expected)
            if err > max_err:
                max_err = err
    print("silu_mul max_err:", max_err)
    if max_err > 0.01:
        raise Error("silu_mul_f16 numeric check FAILED")

    # --- rope (neox) ---
    comptime N_TOK = 3
    comptime N_HEADS = 4
    comptime HEAD_DIM = 64
    comptime HALF = HEAD_DIM // 2
    comptime THETA: Float32 = 10000.0
    var q = ctx.enqueue_create_buffer[DType.float16](N_TOK * N_HEADS * HEAD_DIM)
    var posbuf = ctx.enqueue_create_buffer[DType.int32](N_TOK)
    with q.map_to_host() as qh, posbuf.map_to_host() as ph:
        for i in range(N_TOK * N_HEADS * HEAD_DIM):
            qh[i] = Float16(_fill(i) * 0.3)
        for t in range(N_TOK):
            ph[t] = Int32(t + 5)
    ctx.enqueue_function[rope_neox_f16](
        q.unsafe_ptr(), posbuf.unsafe_ptr(), N_HEADS, HEAD_DIM, THETA,
        grid_dim=(N_TOK, N_HEADS), block_dim=64,
    )
    ctx.synchronize()

    max_err = 0.0
    with q.map_to_host() as qh:
        for t in range(N_TOK):
            for h in range(N_HEADS):
                base = (t * N_HEADS + h) * HEAD_DIM
                for j in range(HALF):
                    freq = pow(THETA, Float32(-2 * j) / Float32(HEAD_DIM))
                    angle = Float32(t + 5) * freq
                    a = Float32(Float16(_fill(base + j) * 0.3))
                    b = Float32(Float16(_fill(base + HALF + j) * 0.3))
                    e1 = a * cos(angle) - b * sin(angle)
                    e2 = a * sin(angle) + b * cos(angle)
                    err = abs(Float32(qh[base + j]) - e1)
                    if err > max_err:
                        max_err = err
                    err = abs(Float32(qh[base + HALF + j]) - e2)
                    if err > max_err:
                        max_err = err
    print("rope max_err:", max_err)
    if max_err > 0.01:
        raise Error("rope_neox_f16 numeric check FAILED")

    # --- gemv q8_0 ---
    comptime ROWS_G = 16
    comptime COLS_G = 256
    comptime BLOCKS_PER_ROW = COLS_G // 32
    comptime WBYTES = ROWS_G * BLOCKS_PER_ROW * 34
    var wq = ctx.enqueue_create_buffer[DType.uint8](WBYTES)
    var xv = ctx.enqueue_create_buffer[DType.float16](COLS_G)
    var yv = ctx.enqueue_create_buffer[DType.float16](ROWS_G)
    var expected_y = List[Float32]()
    with wq.map_to_host() as wh, xv.map_to_host() as xh:
        for c in range(COLS_G):
            xh[c] = Float16(_fill(c) * 0.1)
        for r in range(ROWS_G):
            var acc: Float32 = 0.0
            for bl in range(BLOCKS_PER_ROW):
                off = (r * BLOCKS_PER_ROW + bl) * 34
                scale = Float16(0.02 + Float32((r + bl) % 7) * 0.01)
                # Scale is stored little-endian f16 at the block head.
                bits = scale.to_bits()
                wh[off] = UInt8(bits & 0xFF)
                wh[off + 1] = UInt8((bits >> 8) & 0xFF)
                var s: Float32 = 0.0
                for k in range(32):
                    qv = Int(((r * 31 + bl * 17 + k * 13) % 255)) - 127
                    wh[off + 2 + k] = UInt8(qv & 0xFF)
                    s += Float32(qv) * Float32(Float16(_fill(bl * 32 + k) * 0.1))
                acc += Float32(scale) * s
            expected_y.append(acc)
    ctx.enqueue_function[gemv_q8_0_f16](
        yv.unsafe_ptr(), wq.unsafe_ptr(), xv.unsafe_ptr(), COLS_G,
        grid_dim=ROWS_G, block_dim=256,
    )
    ctx.synchronize()

    max_err = 0.0
    with yv.map_to_host() as yh:
        for r in range(ROWS_G):
            err = abs(Float32(yh[r]) - expected_y[r])
            rel = err / (abs(expected_y[r]) + 1.0)
            if rel > max_err:
                max_err = rel
    print("gemv_q8_0 max_rel_err:", max_err)
    if max_err > 0.02:
        raise Error("gemv_q8_0_f16 numeric check FAILED")

    # --- gemv f16 ---
    var wf = ctx.enqueue_create_buffer[DType.float16](ROWS_G * COLS_G)
    var yf = ctx.enqueue_create_buffer[DType.float16](ROWS_G)
    var expected_f = List[Float32]()
    with wf.map_to_host() as wh, xv.map_to_host() as xh:
        for r in range(ROWS_G):
            var acc: Float32 = 0.0
            for c in range(COLS_G):
                wv = Float16(_fill(r * COLS_G + c) * 0.05)
                wh[r * COLS_G + c] = wv
                acc += Float32(wv) * Float32(xh[c])
            expected_f.append(acc)
    ctx.enqueue_function[gemv_f16](
        yf.unsafe_ptr(), wf.unsafe_ptr(), xv.unsafe_ptr(), COLS_G,
        grid_dim=ROWS_G, block_dim=256,
    )
    ctx.synchronize()

    max_err = 0.0
    with yf.map_to_host() as yh:
        for r in range(ROWS_G):
            err = abs(Float32(yh[r]) - expected_f[r])
            rel = err / (abs(expected_f[r]) + 1.0)
            if rel > max_err:
                max_err = rel
    print("gemv_f16 max_rel_err:", max_err)
    if max_err > 0.02:
        raise Error("gemv_f16 numeric check FAILED")

    print("ALL KERNEL CHECKS PASSED")
