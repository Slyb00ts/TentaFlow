# ===== File: test_kernels.mojo — on-GPU numeric sanity for kernel sources =====
# Fast feedback while editing kernels: runs each kernel on deterministic data
# and compares against scalar CPU math computed here. The authoritative golden
# tests live in Rust (forge-kernels) against forge-formats references.

from std.gpu.host import DeviceContext
from std.math import rsqrt, exp, sqrt
from src.norm import rmsnorm_f16
from src.activation import silu_mul_f16

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

    print("ALL KERNEL CHECKS PASSED")
