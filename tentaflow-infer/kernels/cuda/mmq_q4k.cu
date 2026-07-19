// ===== File: mmq_q4k.cu — vendored llama.cpp Q4_K + Q6_K MMQ tensor-core prefill GEMM =====
//
// The 208-TOPS reference GEMM (docs/CODEGEN_PROOF.md Exp 2). Unlike gemm_i8mma.cu
// (FORGE's hand int8-MMQ kernel, ~107 TOPS), this file runs llama.cpp's ACTUAL
// compiled `mul_mat_q` device code — the exact tensor-core inner loop nvcc/ptxas
// schedules past 200 TOPS on the RTX 4090. The perf-critical compute is not
// re-implemented: the per-type tile loaders and MMA dot products
// (`load_tiles_q4_K`/`load_tiles_q6_K`, `vec_dot_q4_K_q8_1_mma`/
// `vec_dot_q6_K_q8_1_mma`, reached through `mmq_type_traits`) come verbatim from
// the vendored ggml-cuda headers (vendor/llama-cpp/, MIT, (c) 2023-2026 The ggml
// authors). This TU adds the `extern "C"` entry points, replicates ggml's dense
// conventional-tiling loop (the non-perf orchestration around those two device
// functions), and folds the f32→f16 output conversion straight into the epilogue
// so the GEMM writes FORGE's f16 activation dtype with no separate convert pass.
//
// I/O CONTRACT — the GEMM consumes NATIVE GGUF weight bytes (no requant) plus
// llama.cpp's own q8_1 activation quant (`block_q8_1_mmq`), and writes f16
// [token][row] directly:
//   x     : char   GGUF Q4_K (144 B / 256-col) or Q6_K (210 B / 256-col) blocks
//   y     : int    block_q8_1_mmq activation (144 B / 128-col block), laid out
//                  [k_block][token] exactly as ggml's quantize writes it. Q4_K
//                  uses the DS4 ds layout (d + partial sum), Q6_K uses D4 (d only)
//   dst   : __half [ncols_dst=tokens][nrows_x=rows], stride_col_dst = rows
// Grid / dynamic-smem sizing and the per-batch mmq_x selection are replicated
// host-side in the Rust launcher, mirroring ggml's `mul_mat_q_case` /
// `launch_mul_mat_q` (which pull ggml_backend runtime state this cubin has not).

#include "common.cuh"
#include "mmq.cuh"
#include "quantize.cuh"

// ---------------------------------------------------------------------------
// f16 write-back epilogue: a copy of ggml's `mmq_write_back_mma` (mmq.cuh) with
// the destination retyped to __half so the f32 register accumulators are stored
// as FORGE's f16 activation dtype in one pass (no separate f32→f16 kernel). The
// tile geometry (tile_C, ntx, i0) is byte-identical to the ggml original; only
// the final store converts. sm_89 = TURING_MMA path.
// ---------------------------------------------------------------------------
template <int mmq_x, int mmq_y, bool need_check>
static __device__ __forceinline__ void forge_mmq_write_back_f16(
        const float * __restrict__ sum, const int * __restrict__ ids_dst, __half * __restrict__ dst,
        const int stride, const int i_max, const int j_max) {
    constexpr int granularity = mmq_get_granularity_device(mmq_x);
    typedef tile<16, 8, int> tile_C;
    constexpr int rows_per_warp = 2 * granularity;
    constexpr int ntx = rows_per_warp/tile_C::I; // Number of x minitiles per warp.

    const int i0 = (threadIdx.y / ntx) * (ntx*tile_C::I);

#pragma unroll
    for (int j0 = 0; j0 < mmq_x; j0 += ntx*tile_C::J) {
#pragma unroll
        for (int n = 0; n < ntx; ++n) {
#pragma unroll
            for (int l = 0; l < tile_C::ne; ++l) {
                const int j = j0 + (threadIdx.y % ntx) * tile_C::J + tile_C::get_j(l);

                if (j > j_max) {
                    continue;
                }

                const int i = i0 + n*tile_C::I + tile_C::get_i(l);

                if (need_check && i > i_max) {
                    continue;
                }

                dst[ids_dst[j]*stride + i] = __float2half(sum[(j0/tile_C::J + n)*tile_C::ne + l]);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Dense conventional-tiling GEMM body, generic over the weight type. Mirrors the
// `__CUDA_ARCH__ < VOLTA` conventional-tiling branch of ggml's `mul_mat_q` plus
// `mul_mat_q_process_tile` (mmq.cuh), specialized to the dense, single-
// channel/sample, non-MoE case (ids_dst = identity, tmp_fixup = nullptr). The
// K-reduction loop calls the per-type `load_tiles` + `vec_dot` device functions
// unchanged (verbatim ggml compute); the epilogue is the f16 write-back above.
// ---------------------------------------------------------------------------
template <ggml_type type, int mmq_x, bool need_check>
__device__ __forceinline__ void forge_mmq_body(
        const char * __restrict__ x, const int * __restrict__ y, __half * __restrict__ dst,
        const int ncols_x, const int nrows_x, const int ncols_dst,
        const int stride_row_x, const int ncols_y, const int stride_col_dst) {
    constexpr int              warp_size  = ggml_cuda_get_physical_warp_size();          // 32
    constexpr int              nwarps     = mmq_get_nwarps_device();                     // 8 (Ada)
    constexpr int              qk         = ggml_cuda_type_traits<type>::qk;             // 256
    constexpr int              mmq_y      = get_mmq_y_device();                          // 128 (Ada)
    constexpr load_tiles_mmq_t load_tiles = mmq_type_traits<mmq_x, mmq_y, need_check, type>::load_tiles;
    constexpr vec_dot_mmq_t    vec_dot    = mmq_type_traits<mmq_x, mmq_y, need_check, type>::vec_dot_mma;

    // Shared-memory layout identical to ggml's process_tile: the first mmq_x ints
    // are the write-back ids (identity for a dense matmul), then the y tile, then
    // the x tile.
    extern __shared__ int data_mul_mat_q[];
    int * tile_y = data_mul_mat_q + mmq_x;
    int * tile_x = tile_y + GGML_PAD(mmq_x*MMQ_TILE_Y_K, nwarps*warp_size);

#pragma unroll
    for (int j0 = 0; j0 < mmq_x; j0 += nwarps*warp_size) {
        const int j = j0 + threadIdx.y*warp_size + threadIdx.x;
        if (j0 + nwarps*warp_size > mmq_x && j >= mmq_x) {
            break;
        }
        data_mul_mat_q[j] = j;
    }
    __syncthreads();

    const int jt = blockIdx.y; // token tile
    const int it = blockIdx.x; // row tile

    const int offset_y   = jt*mmq_x * (sizeof(block_q8_1_mmq)/sizeof(int));
    const int offset_dst = jt*mmq_x*stride_col_dst + it*mmq_y;

    const int tile_x_max_i = nrows_x   - it*mmq_y - 1;
    const int tile_y_max_j = ncols_dst - jt*mmq_x - 1;

    const int offset_x = it*mmq_y*stride_row_x;

    const int * yq = y + offset_y;

    constexpr int ne_block        = 4 * QK8_1;
    constexpr int sz              = sizeof(block_q8_1_mmq) / sizeof(int);
    constexpr int ITER_K          = get_iter_k(type);
    constexpr int blocks_per_iter = ITER_K / qk;

    float sum[mmq_x*mmq_y / (nwarps*warp_size)] = {0.0f};

    const int kb0_stop = ncols_x / qk;
    for (int kb0 = 0; kb0 < kb0_stop; kb0 += blocks_per_iter) {
        load_tiles(x, tile_x, offset_x + kb0, tile_x_max_i, stride_row_x);
        {
            const int * by0 = yq + ncols_y * (kb0 * qk / ne_block) * sz;
#pragma unroll
            for (int l0 = 0; l0 < mmq_x * MMQ_TILE_Y_K; l0 += nwarps * warp_size) {
                int l = l0 + threadIdx.y*warp_size + threadIdx.x;
                tile_y[l] = by0[l];
            }
        }
        __syncthreads();
        vec_dot(tile_x, tile_y, sum, 0);
        __syncthreads();
        {
            const int * by0 = yq + ncols_y * ((kb0 * qk / ne_block) * sz + sz);
#pragma unroll
            for (int l0 = 0; l0 < mmq_x * MMQ_TILE_Y_K; l0 += nwarps * warp_size) {
                int l = l0 + threadIdx.y*warp_size + threadIdx.x;
                tile_y[l] = by0[l];
            }
        }
        __syncthreads();
        vec_dot(tile_x, tile_y, sum, MMQ_TILE_NE_K);
        __syncthreads();
    }

    forge_mmq_write_back_f16<mmq_x, mmq_y, need_check>(
        sum, data_mul_mat_q, dst + offset_dst, stride_col_dst, tile_x_max_i, tile_y_max_j);
}

#define FORGE_MMQ_ENTRY(NAME, TYPE, MMQX, NC)                                          \
    extern "C" __global__ void __launch_bounds__(MMQ_NWARPS*32, 1) NAME(               \
            const char * x, const int * y, __half * dst,                               \
            const int ncols_x, const int nrows_x, const int ncols_dst,                 \
            const int stride_row_x, const int ncols_y, const int stride_col_dst) {     \
        forge_mmq_body<TYPE, MMQX, NC>(x, y, dst, ncols_x, nrows_x, ncols_dst,         \
                                       stride_row_x, ncols_y, stride_col_dst);         \
    }

// One entry per (weight type, mmq_x, need_check). ggml's `mul_mat_q_case` picks
// the smallest mmq_x (multiple of its granularity, smem <= smpbo) minimizing
// ceil(tokens/mmq_x); the Rust launcher replicates that choice (identical smem
// for Q4_K and Q6_K — both use MMQ_MMA_TILE_X_K == 76). need_check=true only when
// nrows_x is not a multiple of mmq_y (128); both are provided.
#define FORGE_MMQ_Q4K_PAIR(MMQX)                                              \
    FORGE_MMQ_ENTRY(forge_mmq_q4k_x##MMQX##_nc, GGML_TYPE_Q4_K, MMQX, false)  \
    FORGE_MMQ_ENTRY(forge_mmq_q4k_x##MMQX##_c,  GGML_TYPE_Q4_K, MMQX, true)
#define FORGE_MMQ_Q6K_PAIR(MMQX)                                              \
    FORGE_MMQ_ENTRY(forge_mmq_q6k_x##MMQX##_nc, GGML_TYPE_Q6_K, MMQX, false)  \
    FORGE_MMQ_ENTRY(forge_mmq_q6k_x##MMQX##_c,  GGML_TYPE_Q6_K, MMQX, true)

FORGE_MMQ_Q4K_PAIR(8)
FORGE_MMQ_Q4K_PAIR(16)
FORGE_MMQ_Q4K_PAIR(24)
FORGE_MMQ_Q4K_PAIR(32)
FORGE_MMQ_Q4K_PAIR(40)
FORGE_MMQ_Q4K_PAIR(48)
FORGE_MMQ_Q4K_PAIR(56)
FORGE_MMQ_Q4K_PAIR(64)
FORGE_MMQ_Q4K_PAIR(72)
FORGE_MMQ_Q4K_PAIR(80)
FORGE_MMQ_Q4K_PAIR(88)
FORGE_MMQ_Q4K_PAIR(96)
FORGE_MMQ_Q4K_PAIR(104)
FORGE_MMQ_Q4K_PAIR(112)
FORGE_MMQ_Q4K_PAIR(120)
FORGE_MMQ_Q4K_PAIR(128)

FORGE_MMQ_Q6K_PAIR(8)
FORGE_MMQ_Q6K_PAIR(16)
FORGE_MMQ_Q6K_PAIR(24)
FORGE_MMQ_Q6K_PAIR(32)
FORGE_MMQ_Q6K_PAIR(40)
FORGE_MMQ_Q6K_PAIR(48)
FORGE_MMQ_Q6K_PAIR(56)
FORGE_MMQ_Q6K_PAIR(64)
FORGE_MMQ_Q6K_PAIR(72)
FORGE_MMQ_Q6K_PAIR(80)
FORGE_MMQ_Q6K_PAIR(88)
FORGE_MMQ_Q6K_PAIR(96)
FORGE_MMQ_Q6K_PAIR(104)
FORGE_MMQ_Q6K_PAIR(112)
FORGE_MMQ_Q6K_PAIR(120)
FORGE_MMQ_Q6K_PAIR(128)

// ---------------------------------------------------------------------------
// Activation quant: f16 X [token][K] -> block_q8_1_mmq. Faithful copy of ggml's
// `quantize_mmq_q8_1<ds_layout>` (quantize.cu); the DS4 (Q4_K/Q5_K) and D4
// (Q6_K and the IQ family) branches are provided, the D2S6 branch is dropped. The
// one deviation: the input is read as f16 (FORGE's prefill activation dtype) and
// widened to float4, instead of ggml's f32 float4 load — the quant math
// downstream is byte-identical. Grid mirrors `quantize_mmq_q8_1_cuda`
// (single-channel, contiguous rows: s01 = ne00).
// ---------------------------------------------------------------------------
template <bool with_sum>
static __device__ __forceinline__ void forge_quantize_mmq_q8_1_body(
        const __half * __restrict__ x, void * __restrict__ vy,
        const int64_t ne00, const int64_t s01,
        const int64_t ne0, const int ne1) {
    constexpr int vals_per_scale = 32;
    constexpr int vals_per_sum   = 32;

    const int64_t i0 = ((int64_t)blockDim.x*blockIdx.y + threadIdx.x)*4;
    if (i0 >= ne0) {
        return;
    }

    const int64_t i1 = blockIdx.x;
    const int64_t i00 = i0;
    const int64_t i01 = i1;

    block_q8_1_mmq * y = (block_q8_1_mmq *) vy;

    const int64_t ib0 = blockIdx.z*((int64_t)gridDim.x*gridDim.y*blockDim.x/QK8_1);
    const int64_t ib  = ib0 + (i0 / (4*QK8_1))*ne1 + blockIdx.x;
    const int64_t iqs = i0 % (4*QK8_1);

    float4 xi = make_float4(0.0f, 0.0f, 0.0f, 0.0f);
    if (i0 < ne00) {
        const __half2 * xh2 = (const __half2 *) (x + (i01*s01 + i00));
        const float2 a = __half22float2(xh2[0]);
        const float2 b = __half22float2(xh2[1]);
        xi = make_float4(a.x, a.y, b.x, b.y);
    }
    float amax = fabsf(xi.x);
    amax = fmaxf(amax, fabsf(xi.y));
    amax = fmaxf(amax, fabsf(xi.z));
    amax = fmaxf(amax, fabsf(xi.w));

#pragma unroll
    for (int offset = vals_per_scale/8; offset > 0; offset >>= 1) {
        amax = fmaxf(amax, __shfl_xor_sync(0xFFFFFFFF, amax, offset, WARP_SIZE));
    }

    float sum = 0.0f;
    if (with_sum) {
        sum = xi.x + xi.y + xi.z + xi.w;
#pragma unroll
        for (int offset = vals_per_sum/8; offset > 0; offset >>= 1) {
            sum += __shfl_xor_sync(0xFFFFFFFF, sum, offset, WARP_SIZE);
        }
    }

    const float d_inv = 127.0f / amax;
    char4 q;
    q.x = roundf(xi.x*d_inv);
    q.y = roundf(xi.y*d_inv);
    q.z = roundf(xi.z*d_inv);
    q.w = roundf(xi.w*d_inv);

    char4 * yqs4 = (char4 *) y[ib].qs;
    yqs4[iqs/4] = q;

    if (iqs % 32 != 0) {
        return;
    }

    const float d = 1.0f / d_inv;
    if (with_sum) {
        y[ib].ds4[iqs/32] = make_half2(d, sum);
    } else {
        y[ib].d4[iqs/32] = d;
    }
}

// DS4 layout (Q4_K / Q5_K): d + partial sum per 32 values.
extern "C" __global__ void __launch_bounds__(CUDA_QUANTIZE_BLOCK_SIZE_MMQ, 1)
forge_quantize_mmq_q8_1_ds4(
        const __half * __restrict__ x, void * __restrict__ vy,
        const int64_t ne00, const int64_t s01,
        const int64_t ne0, const int ne1) {
    forge_quantize_mmq_q8_1_body<true>(x, vy, ne00, s01, ne0, ne1);
}

// D4 layout (Q6_K and the IQ family): d only per 32 values (symmetric quant, no
// partial sum needed downstream).
extern "C" __global__ void __launch_bounds__(CUDA_QUANTIZE_BLOCK_SIZE_MMQ, 1)
forge_quantize_mmq_q8_1_d4(
        const __half * __restrict__ x, void * __restrict__ vy,
        const int64_t ne00, const int64_t s01,
        const int64_t ne0, const int ne1) {
    forge_quantize_mmq_q8_1_body<false>(x, vy, ne00, s01, ne0, ne1);
}
