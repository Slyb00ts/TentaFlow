// ===== File: mmq_q4k.cu — vendored llama.cpp Q4_K MMQ tensor-core prefill GEMM =====
//
// The 208-TOPS reference GEMM (docs/CODEGEN_PROOF.md Exp 2). Unlike gemm_i8mma.cu
// (FORGE's hand int8-MMQ kernel, ~107 TOPS), this file runs llama.cpp's ACTUAL
// compiled `mul_mat_q` device code — the exact tensor-core inner loop nvcc/ptxas
// schedules past 200 TOPS on the RTX 4090. Nothing is re-implemented: the compute
// (`load_tiles_q4_K`, `vec_dot_q4_K_q8_1_mma`, `mmq_write_back_mma`, all reached
// through `mul_mat_q_process_tile`) comes verbatim from the vendored ggml-cuda
// headers (vendor/llama-cpp/, MIT, (c) 2023-2026 The ggml authors). This TU only
// adds the `extern "C"` entry points and replicates the dense conventional-tiling
// grid wrapper so the kernel loads as a cubin through the same cuModuleLoadData
// path (registry.rs) as the other raw-CUDA kernels (ADR-0001 exception).
//
// I/O CONTRACT — the GEMM consumes NATIVE GGUF Q4_K weight bytes (no requant) plus
// llama.cpp's own q8_1 activation quant (`block_q8_1_mmq`, produced by the
// `forge_quantize_mmq_q8_1_ds4` entry below, a faithful copy of ggml's
// `quantize_mmq_q8_1<DS4>`), and writes f32 [token][row]:
//   x     : char   GGUF Q4_K weight blocks (144 B / 256-col superblock)
//   y     : int    block_q8_1_mmq activation (144 B / 128-col block), laid out
//                  [k_block][token] exactly as ggml's quantize writes it
//   dst   : f32    [ncols_dst=tokens][nrows_x=rows], stride_col_dst = rows
// Grid / dynamic-smem sizing and the per-batch mmq_x selection are replicated
// host-side in the Rust launcher, mirroring ggml's `mul_mat_q_case` /
// `launch_mul_mat_q` (which pull ggml_backend runtime state this cubin has not).
//
// The `forge_f32_to_f16` epilogue converts dst to FORGE's f16 activation tensor.

#include "common.cuh"
#include "mmq.cuh"
#include "quantize.cuh"

// ---------------------------------------------------------------------------
// Q4_K GEMM: dense conventional-tiling wrapper around ggml's process_tile.
// Mirrors the `__CUDA_ARCH__ < VOLTA` conventional-tiling branch of ggml's
// `mul_mat_q` (mmq.cuh), specialized to the dense, single-channel/sample,
// non-MoE case (ids_dst = expert_bounds = tmp_fixup = nullptr). The perf-
// critical compute inside process_tile is 100% ggml.
// ---------------------------------------------------------------------------
template <int mmq_x, bool need_check>
__device__ __forceinline__ void forge_mmq_q4k_body(
        const char * __restrict__ x, const int * __restrict__ y, float * __restrict__ dst,
        const int ncols_x, const int nrows_x, const int ncols_dst,
        const int stride_row_x, const int ncols_y, const int stride_col_dst) {
    constexpr ggml_type type      = GGML_TYPE_Q4_K;
    constexpr int       nwarps    = MMQ_NWARPS;                          // 8 (Ada)
    constexpr int       warp_size = 32;
    constexpr int       qk        = ggml_cuda_type_traits<type>::qk;     // 256
    constexpr int       mmq_y     = 128;                                 // get_mmq_y_device() on Ada

    // Shared-memory ids region (identity for a dense matmul), same base pointer
    // process_tile reads as `data_mul_mat_q`.
    extern __shared__ int ids_dst_shared[];
#pragma unroll
    for (int j0 = 0; j0 < mmq_x; j0 += nwarps*warp_size) {
        const int j = j0 + threadIdx.y*warp_size + threadIdx.x;
        if (j0 + nwarps*warp_size > mmq_x && j >= mmq_x) {
            break;
        }
        ids_dst_shared[j] = j;
    }
    __syncthreads();

    const int jt = blockIdx.y; // token tile
    const int it = blockIdx.x; // row tile

    const int offset_y   = jt*mmq_x * (sizeof(block_q8_1_mmq)/sizeof(int));
    const int offset_dst = jt*mmq_x*stride_col_dst + it*mmq_y;

    const int tile_x_max_i = nrows_x   - it*mmq_y - 1;
    const int tile_y_max_j = ncols_dst - jt*mmq_x - 1;

    const int offset_x = it*mmq_y*stride_row_x;

    mul_mat_q_process_tile<type, mmq_x, need_check, false>(
        x, offset_x, y + offset_y, ids_dst_shared, dst + offset_dst, nullptr,
        stride_row_x, ncols_y, stride_col_dst, tile_x_max_i, tile_y_max_j, 0, ncols_x/qk);
}

#define FORGE_MMQ_Q4K_ENTRY(NAME, MMQX, NC)                                            \
    extern "C" __global__ void __launch_bounds__(MMQ_NWARPS*32, 1) NAME(               \
            const char * x, const int * y, float * dst,                                \
            const int ncols_x, const int nrows_x, const int ncols_dst,                 \
            const int stride_row_x, const int ncols_y, const int stride_col_dst) {     \
        forge_mmq_q4k_body<MMQX, NC>(x, y, dst, ncols_x, nrows_x, ncols_dst,           \
                                     stride_row_x, ncols_y, stride_col_dst);           \
    }

// One entry per (mmq_x, need_check). ggml's `mul_mat_q_case` picks the smallest
// mmq_x (multiple of its granularity, smem <= smpbo) minimizing ceil(tokens/mmq_x);
// the Rust launcher replicates that choice. need_check=true only when nrows_x is
// not a multiple of mmq_y (128); FFN rows here are, but both are provided.
#define FORGE_MMQ_Q4K_PAIR(MMQX)                                              \
    FORGE_MMQ_Q4K_ENTRY(forge_mmq_q4k_x##MMQX##_nc, MMQX, false)             \
    FORGE_MMQ_Q4K_ENTRY(forge_mmq_q4k_x##MMQX##_c,  MMQX, true)

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

// ---------------------------------------------------------------------------
// Activation quant: f16 X [token][K] -> block_q8_1_mmq, DS4 layout (the layout
// ggml selects for Q4_K). Faithful copy of ggml's `quantize_mmq_q8_1<DS4>`
// (quantize.cu) — the DS4-specialized branch only; the non-DS4 code paths of the
// original template are dropped. The one deviation: the input is read as f16
// (FORGE's prefill activation dtype) and widened to float4, instead of ggml's
// f32 float4 load — the quant math downstream is byte-identical. Kept faithful so
// the GEMM's tile loads see exactly the layout it expects. Grid mirrors
// `quantize_mmq_q8_1_cuda` (single-channel, contiguous rows: s01 = ne00).
// ---------------------------------------------------------------------------
extern "C" __global__ void __launch_bounds__(CUDA_QUANTIZE_BLOCK_SIZE_MMQ, 1)
forge_quantize_mmq_q8_1_ds4(
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

    float sum = xi.x + xi.y + xi.z + xi.w;
#pragma unroll
    for (int offset = vals_per_sum/8; offset > 0; offset >>= 1) {
        sum += __shfl_xor_sync(0xFFFFFFFF, sum, offset, WARP_SIZE);
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
    y[ib].ds4[iqs/32] = make_half2(d, sum);
}

// f32 [n]-> f16 [n] elementwise convert (MMQ dst is f32; FORGE wants f16 Y).
extern "C" __global__ void forge_f32_to_f16(
        __half * __restrict__ y, const float * __restrict__ x, const int n) {
    const int i = blockIdx.x*blockDim.x + threadIdx.x;
    if (i < n) {
        y[i] = __float2half(x[i]);
    }
}
