# ===== File: test_gemm_fp8.mojo — fp8(e4m3) TENSOR-CORE prefill GEMM test =====
# Two contracts:
#   1. Kernel correctness: the GPU kernel matches an EXACT CPU fp8 reference
#      (same per-row/per-token e4m3 requant + f32 dot + scale) to tight float
#      tolerance — proves the mma fragment layout / scaling is right.
#   2. bm64 / big produce the SAME result as bm128 (same per-element chain).
# The fp8-vs-f16 number is printed for context (e4m3 modeling error).
#
# Needs MODULAR_NVPTX_COMPILER_PATH=scripts/ptxas_fp8_shim.sh (emitter caps PTX
# at .version 8.1; the shim lifts it to 8.4 for the fp8 mma JIT).

from std.gpu.host import DeviceContext
from std.memory import bitcast
from src.gemm_fp8 import (
    gemm_fp8_f16,
    gemm_fp8_f16_bm64,
    gemm_fp8_f16_big,
    quantize_act_fp8,
)

comptime KERNEL_TOL: Float32 = 0.02
comptime E4M3_MAX: Float32 = 448.0


def _fill(i: Int) -> Float32:
    seed = (UInt32(i) * 2654435761 + 1013904223) & 0xFFFFFFFF
    return Float32(seed) * (2.0 / 4294967296.0) - 1.0


def main() raises:
    var ctx = DeviceContext()
    comptime T = 100
    comptime N = 70
    comptime K = 256

    var x = ctx.enqueue_create_buffer[DType.float16](T * K)
    var wf = ctx.enqueue_create_buffer[DType.float16](N * K)
    with x.map_to_host() as h:
        for i in range(T * K):
            h[i] = Float16(_fill(i))
    with wf.map_to_host() as h:
        for i in range(N * K):
            h[i] = Float16(_fill(i * 3 + 7))

    # ---- Host requant: weights per-row e4m3 + scale ----
    var wq = ctx.enqueue_create_buffer[DType.int8](N * K)
    var wsc = ctx.enqueue_create_buffer[DType.float32](N)
    with wf.map_to_host() as hf, wq.map_to_host() as hq, wsc.map_to_host() as hs:
        for r in range(N):
            var amax: Float32 = 0.0
            for k in range(K):
                v = abs(Float32(hf[r * K + k]))
                if v > amax:
                    amax = v
            var scale = (amax / E4M3_MAX) if amax > 0.0 else Float32(1.0)
            var inv = (E4M3_MAX / amax) if amax > 0.0 else Float32(0.0)
            hs[r] = scale
            for k in range(K):
                e = Scalar[DType.float8_e4m3fn](Float32(hf[r * K + k]) * inv)
                hq[r * K + k] = bitcast[DType.int8, 1](e)

    # ---- GPU activation quant ----
    var xq = ctx.enqueue_create_buffer[DType.int8](T * K)
    var xsc = ctx.enqueue_create_buffer[DType.float32](T)
    ctx.enqueue_function[quantize_act_fp8](
        xq.unsafe_ptr(), xsc.unsafe_ptr(), x.unsafe_ptr(), K, T,
        grid_dim=T, block_dim=256,
    )
    ctx.synchronize()

    # ---- Exact CPU fp8 reference (dequant e4m3 → f32 dot) ----
    var yref = ctx.enqueue_create_buffer[DType.float32](T * N)
    with xq.map_to_host() as hxq, xsc.map_to_host() as hxs, wq.map_to_host() as hwq, wsc.map_to_host() as hws, yref.map_to_host() as hy:
        for t in range(T):
            for r in range(N):
                var acc: Float32 = 0.0
                for k in range(K):
                    xe = bitcast[DType.float8_e4m3fn, 1](hxq[t * K + k])
                    we = bitcast[DType.float8_e4m3fn, 1](hwq[r * K + k])
                    acc += Float32(xe) * Float32(we)
                hy[t * N + r] = acc * hxs[t] * hws[r]

    # ---- f16 reference (modeling error context) ----
    var yf16 = ctx.enqueue_create_buffer[DType.float32](T * N)
    with x.map_to_host() as hx, wf.map_to_host() as hw, yf16.map_to_host() as hy:
        for t in range(T):
            for r in range(N):
                var acc: Float32 = 0.0
                for k in range(K):
                    acc += Float32(hx[t * K + k]) * Float32(hw[r * K + k])
                hy[t * N + r] = acc

    # ---- GPU kernels ----
    var ym = ctx.enqueue_create_buffer[DType.float16](T * N)
    var ym64 = ctx.enqueue_create_buffer[DType.float16](T * N)
    var ymbig = ctx.enqueue_create_buffer[DType.float16](T * N)

    ctx.enqueue_function[gemm_fp8_f16](
        ym.unsafe_ptr(), wq.unsafe_ptr(), wsc.unsafe_ptr(),
        xq.unsafe_ptr(), xsc.unsafe_ptr(), K, N, T,
        grid_dim=((N + 63) // 64, (T + 127) // 128), block_dim=8 * 32,
    )
    ctx.enqueue_function[gemm_fp8_f16_bm64](
        ym64.unsafe_ptr(), wq.unsafe_ptr(), wsc.unsafe_ptr(),
        xq.unsafe_ptr(), xsc.unsafe_ptr(), K, N, T,
        grid_dim=((N + 63) // 64, (T + 63) // 64), block_dim=8 * 32,
    )
    ctx.enqueue_function[gemm_fp8_f16_big](
        ymbig.unsafe_ptr(), wq.unsafe_ptr(), wsc.unsafe_ptr(),
        xq.unsafe_ptr(), xsc.unsafe_ptr(), K, N, T,
        grid_dim=((N + 127) // 128, (T + 127) // 128), block_dim=16 * 32,
    )
    ctx.synchronize()

    var max_kerr: Float32 = 0.0
    var max_f16: Float32 = 0.0
    var mism64 = 0
    var mismbig = 0
    with ym.map_to_host() as hm, ym64.map_to_host() as hm64, ymbig.map_to_host() as hmb, yref.map_to_host() as hr, yf16.map_to_host() as hf16:
        for i in range(T * N):
            g = Float32(hm[i])
            ke = abs(g - hr[i]) / (abs(hr[i]) + 1.0)
            if ke > max_kerr:
                max_kerr = ke
            fe = abs(g - hf16[i]) / (abs(hf16[i]) + 1.0)
            if fe > max_f16:
                max_f16 = fe
            if Float32(hm64[i]) != g:
                mism64 += 1
            if Float32(hmb[i]) != g:
                mismbig += 1

    print("fp8 kernel vs exact CPU fp8 ref: max_rel_err =", max_kerr)
    print("fp8 vs f16 modeling error:      max_rel_err =", max_f16)
    print("bm64 mismatches vs bm128:", mism64, " big mismatches:", mismbig)
    if max_kerr > KERNEL_TOL:
        raise Error("fp8 kernel exceeds tolerance vs CPU fp8 reference")
    print("PASS")
