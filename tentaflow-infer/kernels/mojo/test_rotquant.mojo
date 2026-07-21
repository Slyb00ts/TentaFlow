# ===== File: test_rotquant.mojo — on-GPU accuracy gate for rotational KV quant =====
# Measures the REAL deployment error of the rotational low-bit KV quantizer
# (rot4 / rot3) against fp8-e4m3 and against plain (unrotated) int4/int3 on
# synthetic post-norm K/V distributions (Gaussian bulk + heavy outliers — the
# regime rotation is designed to fix). Reports:
#   * orthogonality + self-inverse of the Walsh-Hadamard rotation
#   * per-vector reconstruction RMSE (V-style: full round trip)
#   * attention dot-product preservation (K-style: (R·q)·dequant(R·k))
# The authoritative sign of the whole feature — "is 4-bit good enough, is 3-bit
# too lossy?" — is read straight off this table.

from std.gpu.host import DeviceContext
from std.math import sqrt, log, cos, pi
from src.rotquant import (
    rotquant_roundtrip_hd128_b4,
    rotquant_roundtrip_hd128_b3,
    rotate_vec_hd128,
)

comptime D = 128
comptime N = 4096


# Deterministic LCG in [0,1).
def _lcg(mut state: UInt64) -> Float64:
    state = state * 6364136223846793005 + 1442695040888963407
    return Float64((state >> 11)) / Float64(1 << 53)


def _randn(mut state: UInt64) -> Float64:
    # Box-Muller.
    u1 = _lcg(state)
    u2 = _lcg(state)
    var lu = u1
    if lu < 1e-12:
        lu = 1e-12
    return sqrt(-2.0 * log(lu)) * cos(2.0 * pi * u2)


# Host FHT (f64 oracle) on a length-D InlineArray.
def _fht_host(mut x: InlineArray[Float64, D]):
    var h = 1
    while h < D:
        var i = 0
        while i < D:
            var j = i
            while j < i + h:
                a = x[j]
                b = x[j + h]
                x[j] = a + b
                x[j + h] = a - b
                j += 1
            i += h + h
        h += h


def _rotate_host(mut x: InlineArray[Float64, D]):
    _fht_host(x)
    inv = 1.0 / sqrt(Float64(D))
    for i in range(D):
        x[i] = x[i] * inv


# Symmetric b-bit quantization RMSE of a length-D vector (host).
def _quant_rmse(x: InlineArray[Float64, D], bits: Int) -> Float64:
    levels = Float64((1 << (bits - 1)) - 1)
    var amax: Float64 = 0.0
    for i in range(D):
        a = x[i]
        if a < 0.0:
            a = -a
        if a > amax:
            amax = a
    var scale = amax / levels
    if scale == 0.0:
        scale = 1.0
    var se: Float64 = 0.0
    for i in range(D):
        var qq = round(x[i] / scale)
        if qq > levels:
            qq = levels
        if qq < -levels:
            qq = -levels
        d = qq * scale - x[i]
        se += d * d
    return sqrt(se / Float64(D))


def main() raises:
    var ctx = DeviceContext()
    print("=== rotational low-bit KV quant accuracy (head_dim", D, "N", N, ") ===")

    # --- synthetic post-norm K/V: unit-ish Gaussian with ~6% heavy outliers ---
    var x = ctx.enqueue_create_buffer[DType.float16](N * D)
    var q = ctx.enqueue_create_buffer[DType.float16](N * D)
    var state: UInt64 = 0x1234567
    with x.map_to_host() as xh, q.map_to_host() as qh:
        for v in range(N):
            for i in range(D):
                var val = _randn(state)
                if _lcg(state) < 0.06:
                    val = val * 8.0  # outlier channel — kills naive per-vec int4
                xh[v * D + i] = Float16(val)
                qh[v * D + i] = Float16(_randn(state))

    out_r4 = ctx.enqueue_create_buffer[DType.float16](N * D)
    out_x4 = ctx.enqueue_create_buffer[DType.float16](N * D)
    sc4 = ctx.enqueue_create_buffer[DType.float16](N)
    pk4 = ctx.enqueue_create_buffer[DType.uint8](N * (D // 2))

    out_r3 = ctx.enqueue_create_buffer[DType.float16](N * D)
    out_x3 = ctx.enqueue_create_buffer[DType.float16](N * D)
    sc3 = ctx.enqueue_create_buffer[DType.float16](N)
    pk3 = ctx.enqueue_create_buffer[DType.uint8](N * (D // 8 * 3))

    rq = ctx.enqueue_create_buffer[DType.float16](N * D)  # rotated q

    ctx.enqueue_function[rotquant_roundtrip_hd128_b4](
        x.unsafe_ptr(), out_r4.unsafe_ptr(), out_x4.unsafe_ptr(),
        sc4.unsafe_ptr(), pk4.unsafe_ptr(), N,
        grid_dim=N, block_dim=32,
    )
    ctx.enqueue_function[rotquant_roundtrip_hd128_b3](
        x.unsafe_ptr(), out_r3.unsafe_ptr(), out_x3.unsafe_ptr(),
        sc3.unsafe_ptr(), pk3.unsafe_ptr(), N,
        grid_dim=N, block_dim=32,
    )
    ctx.enqueue_function[rotate_vec_hd128](
        q.unsafe_ptr(), rq.unsafe_ptr(), N,
        grid_dim=N, block_dim=32,
    )
    ctx.synchronize()

    # --- orthogonality + self-inverse of R on the first vector ---
    var orig = InlineArray[Float64, D](fill=0.0)
    var rot = InlineArray[Float64, D](fill=0.0)
    with x.map_to_host() as xh:
        for i in range(D):
            orig[i] = Float64(xh[i])
            rot[i] = Float64(xh[i])
    _rotate_host(rot)
    var n0: Float64 = 0.0
    var n1: Float64 = 0.0
    for i in range(D):
        n0 += orig[i] * orig[i]
        n1 += rot[i] * rot[i]
    _rotate_host(rot)  # apply again: should recover orig
    var rterr: Float64 = 0.0
    for i in range(D):
        d = rot[i] - orig[i]
        if d < 0.0:
            d = -d
        if d > rterr:
            rterr = d
    print("R orthogonal: ||x||^2 =", n0, " ||Rx||^2 =", n1, " (equal => norm-preserving)")
    print("R self-inverse: max|R(R(x)) - x| =", rterr)

    # --- reconstruction + dot-product errors ---
    var se_fp8: Float64 = 0.0
    var se_int4: Float64 = 0.0
    var se_int3: Float64 = 0.0
    var se_rot4: Float64 = 0.0
    var se_rot3: Float64 = 0.0
    var cnt: Float64 = 0.0

    var dot_err_fp8: Float64 = 0.0
    var dot_err_rot4: Float64 = 0.0
    var dot_err_rot3: Float64 = 0.0
    var dot_den: Float64 = 0.0

    with x.map_to_host() as xh, q.map_to_host() as qh, rq.map_to_host() as rqh, \
         out_x4.map_to_host() as x4h, out_x3.map_to_host() as x3h, \
         out_r4.map_to_host() as r4h, out_r3.map_to_host() as r3h:
        for v in range(N):
            base = v * D

            # fp8-e4m3 baseline (direct per-value cast of the RAW value)
            for i in range(D):
                f8 = Float64(Scalar[DType.float8_e4m3fn](Float32(xh[base + i])))
                d = f8 - Float64(xh[base + i])
                se_fp8 += d * d

            # plain int4/int3 on the RAW (unrotated) vector
            var raw = InlineArray[Float64, D](fill=0.0)
            for i in range(D):
                raw[i] = Float64(xh[base + i])
            r4 = _quant_rmse(raw, 4)
            r3 = _quant_rmse(raw, 3)
            se_int4 += r4 * r4 * Float64(D)
            se_int3 += r3 * r3 * Float64(D)

            # rot4 / rot3 full reconstruction error (GPU round trip vs raw)
            for i in range(D):
                d4 = Float64(x4h[base + i]) - Float64(xh[base + i])
                d3 = Float64(x3h[base + i]) - Float64(xh[base + i])
                se_rot4 += d4 * d4
                se_rot3 += d3 * d3
            cnt += Float64(D)

            # dot-product preservation on the first 512 vectors
            if v < 512:
                var true_dot: Float64 = 0.0
                for i in range(D):
                    true_dot += Float64(qh[base + i]) * Float64(xh[base + i])
                var dot_rot4: Float64 = 0.0
                var dot_rot3: Float64 = 0.0
                var dot_fp8: Float64 = 0.0
                for i in range(D):
                    dot_rot4 += Float64(rqh[base + i]) * Float64(r4h[base + i])
                    dot_rot3 += Float64(rqh[base + i]) * Float64(r3h[base + i])
                    f8 = Float64(Scalar[DType.float8_e4m3fn](Float32(xh[base + i])))
                    dot_fp8 += Float64(qh[base + i]) * f8
                dot_err_rot4 += (dot_rot4 - true_dot) * (dot_rot4 - true_dot)
                dot_err_rot3 += (dot_rot3 - true_dot) * (dot_rot3 - true_dot)
                dot_err_fp8 += (dot_fp8 - true_dot) * (dot_fp8 - true_dot)
                dot_den += true_dot * true_dot

    print("")
    print("reconstruction RMSE (lower is better):")
    print("  fp8-e4m3 (raw)       :", sqrt(se_fp8 / cnt))
    print("  int4    (raw, no rot):", sqrt(se_int4 / cnt))
    print("  int3    (raw, no rot):", sqrt(se_int3 / cnt))
    print("  rot4    (WHT + 4-bit):", sqrt(se_rot4 / cnt))
    print("  rot3    (WHT + 3-bit):", sqrt(se_rot3 / cnt))
    print("")
    print("attention dot-product relative RMSE (lower is better):")
    print("  fp8-e4m3 :", sqrt(dot_err_fp8 / dot_den))
    print("  rot4     :", sqrt(dot_err_rot4 / dot_den))
    print("  rot3     :", sqrt(dot_err_rot3 / dot_den))
    print("")
    print("bytes/element: f16=2  fp8=1  rot4=0.5+scale  rot3=0.375+scale")
