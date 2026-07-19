// ===== File: gemm_i8mma.cu — nvcc int8 MMQ prefill GEMM (Q4_K + Q8_0) =====
//
// FORGE's hot prefill GEMM compiled by nvcc/ptxas instead of Mojo. ADR-0001
// keeps kernels 100%-Mojo; this file is the single, proven exception: the
// int8 tensor-core MMQ path. docs/CODEGEN_PROOF.md shows Mojo's backend caps
// this exact algorithm at ~66 TOPS while nvcc/ptxas schedules the identical
// mma.sync.m16n8k32.s8 loop past ~200 TOPS. The escape hatch loads as a cubin
// through the existing cudarc `cuModuleLoadData` path (Exp 4), so nothing in
// the HAL or the routing changes — only the GEMM compute swaps.
//
// I/O CONTRACT — byte-identical to Mojo `gemm_i8mma_impl` (kernels/mojo/src/
// gemm.mojo). Inputs are produced by the SAME `quantize_act_q8_1` pre-pass and
// the SAME GGUF weight layout; output is the SAME f16 [token][row] tensor:
//   y     : f16   [n_tokens][n_rows]           row-major output
//   w     : u8    GGUF codes (Q8_0 34B/32-col block, Q4_K 144B/256-col block)
//   xq    : i8    [n_tokens][n_cols]           q8_1 activation codes (row-major)
//   xd    : f32   [n_cols/32][n_tokens]        per-32-block activation scale d
//   xsm   : f32   [n_cols/32][n_tokens]        d * Σ(codes)   (Q4_K min term)
//   dims  : n_cols (K), n_rows (output rows), n_tokens (T)
//
// The fragment addressing (ldmatrix source offsets) and the per-32-block f32
// scale/min epilogue replicate the Mojo kernel one-to-one, so the outputs match
// within f32 accumulation-order tolerance (integer mma is exact).
//
// The Q4_K unpack + scale-min helper is llama.cpp's get_scale_min_k4 math
// (ggml, MIT-licensed, Copyright (c) 2023-2024 The ggml authors) — adapted.

#include <cstdint>
#include <cuda_fp16.h>

namespace {

__device__ __forceinline__ unsigned smem_addr(const void* p) {
    return static_cast<unsigned>(__cvta_generic_to_shared(p));
}

// ldmatrix.x4 (four b32 = 16 int8 = mma A fragment) from a shared 8x8-b16 tile set.
__device__ __forceinline__ void ldmatrix_x4(unsigned addr, unsigned& r0, unsigned& r1,
                                            unsigned& r2, unsigned& r3) {
    asm volatile(
        "ldmatrix.sync.aligned.m8n8.x4.shared.b16 {%0,%1,%2,%3}, [%4];\n"
        : "=r"(r0), "=r"(r1), "=r"(r2), "=r"(r3)
        : "r"(addr));
}

// ldmatrix.x2 (two b32 = 8 int8 = mma B fragment).
__device__ __forceinline__ void ldmatrix_x2(unsigned addr, unsigned& r0, unsigned& r1) {
    asm volatile(
        "ldmatrix.sync.aligned.m8n8.x2.shared.b16 {%0,%1}, [%2];\n"
        : "=r"(r0), "=r"(r1)
        : "r"(addr));
}

// One mma.sync.aligned.m16n8k32.row.col.s32.s8.s8.s32 (C = 0, external f32 accum).
__device__ __forceinline__ void mma_s8(int (&d)[4], const unsigned (&a)[4],
                                       const unsigned (&b)[2]) {
    asm volatile(
        "mma.sync.aligned.m16n8k32.row.col.s32.s8.s8.s32 "
        "{%0,%1,%2,%3}, {%4,%5,%6,%7}, {%8,%9}, {%10,%11,%12,%13};\n"
        : "=r"(d[0]), "=r"(d[1]), "=r"(d[2]), "=r"(d[3])
        : "r"(a[0]), "r"(a[1]), "r"(a[2]), "r"(a[3]), "r"(b[0]), "r"(b[1]),
          "n"(0), "n"(0), "n"(0), "n"(0));
}

// llama.cpp get_scale_min_k4: 6-bit sub-block scale + min from the 12 packed
// bytes of a Q4_K/Q5_K superblock header (hdr[4..16]). j in 0..7.
__device__ __forceinline__ void q4k_scale_min(const uint8_t* hdr, int j, int& sc, int& mn) {
    if (j < 4) {
        sc = hdr[4 + j] & 63;
        mn = hdr[8 + j] & 63;
    } else {
        sc = (hdr[8 + j] & 0x0F) | ((hdr[j] >> 6) << 4);
        mn = (hdr[8 + j] >> 4) | ((hdr[4 + j] >> 6) << 4);
    }
}

// BM tokens x BN rows per block, NW warps (NW*32 threads). Warps split
// M_WARPS x N_WARPS; each warp owns MT_PER_WARP=2 m-tiles (32 tokens) x
// NT_PER_WARP n-tiles. Mirrors Mojo `gemm_i8mma_impl` parameterisation.
// FMT: 0=Q8_0, 1=Q4_K.
template <int BM, int BN, int NW, int MW, int FMT>
__device__ __forceinline__ void gemm_i8mma_core(
    __half* __restrict__ y, const uint8_t* __restrict__ w,
    const int8_t* __restrict__ xq_g, const float* __restrict__ xd_g,
    const float* __restrict__ xsm_g, int n_cols, int n_rows, int n_tokens) {
    constexpr int NTHREADS = NW * 32;
    constexpr int M_WARPS = MW;
    constexpr int N_WARPS = NW / M_WARPS;
    constexpr int MT_PER_WARP = BM / (16 * M_WARPS);
    constexpr int NT_PER_WARP = (BN / 8) / N_WARPS;
    constexpr int W_ROWS_PER_PASS = NTHREADS / 4;  // 64
    constexpr int W_PASSES = BN / W_ROWS_PER_PASS;
    constexpr int BLK_BYTES = (FMT == 0) ? 34 : 144;
    constexpr int BPR_DIV = (FMT == 0) ? 32 : 256;

    __shared__ int8_t xq_s[2][BM * 32];
    __shared__ int8_t wq_s[2][BN * 32];
    __shared__ float xd_s[2][BM];
    __shared__ float xsm_s[2][BM];
    __shared__ float wdsc_s[2][BN];
    __shared__ float wdmn_s[2][BN];

    const int tid = threadIdx.x;
    const int row0 = blockIdx.x * BN;
    const int t0 = blockIdx.y * BM;

    const int lane = tid & 31;
    const int wid = tid >> 5;
    const int wid_m = wid % M_WARPS;
    const int wid_n = wid / M_WARPS;
    const int g = lane >> 2;
    const int tt = lane & 3;
    const int sub = lane >> 3;
    const int lr = lane & 7;
    const int mt0 = wid_m * (MT_PER_WARP * 16);
    const int nbase = wid_n * NT_PER_WARP;

    const int row_l = tid >> 2;
    const int part = tid & 3;
    const int blocks_per_row = n_cols / BPR_DIV;

    int wrow_base[W_PASSES];
#pragma unroll
    for (int p = 0; p < W_PASSES; ++p) {
        int wrow = row0 + p * W_ROWS_PER_PASS + row_l;
        if (wrow > n_rows - 1) wrow = n_rows - 1;
        wrow_base[p] = wrow * blocks_per_row * BLK_BYTES;
    }

    const int n_stages = n_cols / 32;

    float acc[MT_PER_WARP * NT_PER_WARP][4];
#pragma unroll
    for (int i = 0; i < MT_PER_WARP * NT_PER_WARP; ++i)
#pragma unroll
        for (int k = 0; k < 4; ++k) acc[i][k] = 0.0f;

    // Stage s into buffer s&1 (global -> shared, with weight nibble/scale unpack).
    auto stage = [&](int s, int buf) {
        if (tid < BM) {
            int tok = t0 + tid;
            if (tok > n_tokens - 1) tok = n_tokens - 1;
            const int8_t* src = xq_g + (long)tok * n_cols + s * 32;
            int4 a = *reinterpret_cast<const int4*>(src);
            int4 b = *reinterpret_cast<const int4*>(src + 16);
            *reinterpret_cast<int4*>(&xq_s[buf][tid * 32]) = a;
            *reinterpret_cast<int4*>(&xq_s[buf][tid * 32 + 16]) = b;
            xd_s[buf][tid] = xd_g[(long)s * n_tokens + tok];
            if (FMT == 1) xsm_s[buf][tid] = xsm_g[(long)s * n_tokens + tok];
        }
#pragma unroll
        for (int p = 0; p < W_PASSES; ++p) {
            const int rl = p * W_ROWS_PER_PASS + row_l;
            if (FMT == 0) {
                const uint8_t* base = w + wrow_base[p] + s * 34;
                const int8_t* codes = reinterpret_cast<const int8_t*>(base + 2 + part * 8);
#pragma unroll
                for (int e = 0; e < 8; ++e) wq_s[buf][rl * 32 + part * 8 + e] = codes[e];
                if (part == 0) {
                    __half sc = *reinterpret_cast<const __half*>(base);
                    wdsc_s[buf][rl] = __half2float(sc);
                }
            } else {
                const int sb = s >> 3;
                const int nsub = s & 7;
                const int chunk = nsub >> 1;
                const int half_sel = nsub & 1;
                const uint8_t* bsb = w + wrow_base[p] + sb * 144;
                const uint8_t* raw = bsb + 16 + chunk * 32 + part * 8;
#pragma unroll
                for (int e = 0; e < 8; ++e) {
                    uint8_t nib = half_sel ? (raw[e] >> 4) : (raw[e] & 0x0F);
                    wq_s[buf][rl * 32 + part * 8 + e] = static_cast<int8_t>(nib);
                }
                if (part == 0) {
                    __half d = *reinterpret_cast<const __half*>(bsb);
                    __half dmin = *reinterpret_cast<const __half*>(bsb + 2);
                    int sc, mn;
                    q4k_scale_min(bsb, nsub, sc, mn);
                    wdsc_s[buf][rl] = __half2float(d) * static_cast<float>(sc);
                    wdmn_s[buf][rl] = __half2float(dmin) * static_cast<float>(mn);
                }
            }
        }
    };

    // Compute this stage's mma + f32 scale-min epilogue from buffer `buf`.
    auto compute = [&](int buf) {
        __half* xq_half = reinterpret_cast<__half*>(&xq_s[buf][0]);
        __half* wq_half = reinterpret_cast<__half*>(&wq_s[buf][0]);

        unsigned ai[MT_PER_WARP][4];
        float dxv[MT_PER_WARP][2];
        float xsv[MT_PER_WARP][2];
#pragma unroll
        for (int mi = 0; mi < MT_PER_WARP; ++mi) {
            const int row_in_tile = mt0 + mi * 16 + (sub & 1) * 8 + lr;
            __half* a_ptr = xq_half + row_in_tile * 16 + (sub >> 1) * 8;
            ldmatrix_x4(smem_addr(a_ptr), ai[mi][0], ai[mi][1], ai[mi][2], ai[mi][3]);
            dxv[mi][0] = xd_s[buf][mt0 + mi * 16 + g];
            dxv[mi][1] = xd_s[buf][mt0 + mi * 16 + g + 8];
            if (FMT == 1) {
                xsv[mi][0] = xsm_s[buf][mt0 + mi * 16 + g];
                xsv[mi][1] = xsm_s[buf][mt0 + mi * 16 + g + 8];
            }
        }
#pragma unroll
        for (int nti = 0; nti < NT_PER_WARP; ++nti) {
            const int nb = (nbase + nti) * 8;
            __half* b_ptr = wq_half + nb * 16 + lr * 16 + (sub & 1) * 8;
            unsigned bi[2];
            ldmatrix_x2(smem_addr(b_ptr), bi[0], bi[1]);
            const float dw0 = wdsc_s[buf][nb + 2 * tt];
            const float dw1 = wdsc_s[buf][nb + 2 * tt + 1];
            float mn0 = 0.0f, mn1 = 0.0f;
            if (FMT == 1) {
                mn0 = wdmn_s[buf][nb + 2 * tt];
                mn1 = wdmn_s[buf][nb + 2 * tt + 1];
            }
#pragma unroll
            for (int mi = 0; mi < MT_PER_WARP; ++mi) {
                int d[4] = {0, 0, 0, 0};
                mma_s8(d, ai[mi], bi);
                float* a4 = acc[mi * NT_PER_WARP + nti];
                a4[0] += dxv[mi][0] * dw0 * static_cast<float>(d[0]);
                a4[1] += dxv[mi][0] * dw1 * static_cast<float>(d[1]);
                a4[2] += dxv[mi][1] * dw0 * static_cast<float>(d[2]);
                a4[3] += dxv[mi][1] * dw1 * static_cast<float>(d[3]);
                if (FMT == 1) {
                    a4[0] -= mn0 * xsv[mi][0];
                    a4[1] -= mn1 * xsv[mi][0];
                    a4[2] -= mn0 * xsv[mi][1];
                    a4[3] -= mn1 * xsv[mi][1];
                }
            }
        }
    };

    stage(0, 0);
    __syncthreads();
    for (int s = 0; s < n_stages; ++s) {
        const int buf = s & 1;
        if (s + 1 < n_stages) stage(s + 1, (s + 1) & 1);
        compute(buf);
        __syncthreads();
    }

#pragma unroll
    for (int mi = 0; mi < MT_PER_WARP; ++mi) {
        const int tok_a = t0 + mt0 + mi * 16 + g;
        const int tok_b = tok_a + 8;
#pragma unroll
        for (int nti = 0; nti < NT_PER_WARP; ++nti) {
            const int nb = (nbase + nti) * 8;
            const int r_a = row0 + nb + 2 * tt;
            const int r_b = r_a + 1;
            const float* a4 = acc[mi * NT_PER_WARP + nti];
            if (tok_a < n_tokens) {
                if (r_a < n_rows) y[(long)tok_a * n_rows + r_a] = __float2half(a4[0]);
                if (r_b < n_rows) y[(long)tok_a * n_rows + r_b] = __float2half(a4[1]);
            }
            if (tok_b < n_tokens) {
                if (r_a < n_rows) y[(long)tok_b * n_rows + r_a] = __float2half(a4[2]);
                if (r_b < n_rows) y[(long)tok_b * n_rows + r_b] = __float2half(a4[3]);
            }
        }
    }
}

}  // namespace

#define FORGE_I8MMA_ENTRY(NAME, BM, BN, NW, MW, FMT)                           \
    extern "C" __global__ void __launch_bounds__(NW * 32) NAME(                \
        __half* y, const uint8_t* w, const int8_t* xq, const float* xd,        \
        const float* xsm, int n_cols, int n_rows, int n_tokens) {              \
        gemm_i8mma_core<BM, BN, NW, MW, FMT>(y, w, xq, xd, xsm, n_cols,        \
                                             n_rows, n_tokens);                \
    }

FORGE_I8MMA_ENTRY(forge_gemm_q4_k_i8mma_cuda, 128, 128, 8, 2, 1)
FORGE_I8MMA_ENTRY(forge_gemm_q4_k_i8mma_cuda_bn64, 128, 64, 8, 4, 1)
FORGE_I8MMA_ENTRY(forge_gemm_q8_0_i8mma_cuda, 128, 128, 8, 2, 0)
FORGE_I8MMA_ENTRY(forge_gemm_q8_0_i8mma_cuda_bn64, 128, 64, 8, 4, 0)
