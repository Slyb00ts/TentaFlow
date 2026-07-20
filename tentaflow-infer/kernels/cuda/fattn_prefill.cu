// ===== File: fattn_prefill.cu — nvcc tensor-core flash-attention prefill =====
//
// Causal flash-attention prefill over FORGE's paged KV cache, computed with f16
// tensor-core mma (m16n8k16) instead of the scalar/SIMD dot products of the Mojo
// `attn_prefill` (kernels/mojo/src/prefill.mojo). ADR-0001 exception (docs/CODEGEN_PROOF.md): the CUDA cubin loads
// through the existing cuModuleLoadData path and is routed only under
// FORGE_ATTN=fa, so the default scalar path stays bit-exact.
//
// I/O CONTRACT — byte-identical to Mojo `attn_prefill` so it is a drop-in:
//   out       : f16  [T][n_q_heads][head_dim]     row-major
//   q         : f16  [T][n_q_heads][head_dim]     row-major (already rope'd/normed)
//   k_cache   : f16  [n_pages][n_kv_heads][page_size][head_dim]
//   v_cache   : f16  [n_pages][n_kv_heads][page_size][head_dim]
//   page_table: i32  logical-page -> physical-page
//   base_pos  : first absolute position of this chunk
//   n_q_heads/n_kv_heads : GQA group = n_q_heads / n_kv_heads
//   page_size, scale (softmax QK^T scale), n_tokens (=T)
// Query token `tok` attends absolute positions 0..base_pos+tok (causal). Grid:
// (ceil(T/BQ), n_q_heads); one block owns BQ=64 query rows of one head, with 4
// warps each owning a 16-row m-tile. K/V stream through shared-memory tiles of
// BK=32 positions; QK^T and P·V run as f16 mma with an online softmax kept in
// registers (running max/sum, rescaled accumulator).
//
// The tensor-core FA scheme (mma QK^T, online softmax, mma P·V, KV tiling) mirrors
// llama.cpp's fattn-mma-f16.cuh (ggml, MIT, Copyright (c) 2023-2024 The ggml
// authors) — adapted to FORGE's paged cache and I/O contract.

#include <cstdint>
#include <cuda_fp16.h>

namespace {

__device__ __forceinline__ unsigned smem_addr(const void* p) {
    return static_cast<unsigned>(__cvta_generic_to_shared(p));
}

// ldmatrix.x4 (four 8x8 b16 = mma A operand for a 16x16 f16 tile).
__device__ __forceinline__ void ldmatrix_x4(unsigned addr, unsigned& r0, unsigned& r1,
                                            unsigned& r2, unsigned& r3) {
    asm volatile(
        "ldmatrix.sync.aligned.m8n8.x4.shared.b16 {%0,%1,%2,%3}, [%4];\n"
        : "=r"(r0), "=r"(r1), "=r"(r2), "=r"(r3)
        : "r"(addr));
}

// ldmatrix.x2 (two 8x8 b16 = mma B operand for a 16x8 f16 tile).
__device__ __forceinline__ void ldmatrix_x2(unsigned addr, unsigned& r0, unsigned& r1) {
    asm volatile(
        "ldmatrix.sync.aligned.m8n8.x2.shared.b16 {%0,%1}, [%2];\n"
        : "=r"(r0), "=r"(r1)
        : "r"(addr));
}

// D(16x8,f32) += A(16x16,f16) * B(16x8,f16). C accumulates in place.
__device__ __forceinline__ void mma_f16(float (&d)[4], const unsigned (&a)[4],
                                        const unsigned (&b)[2]) {
    asm volatile(
        "mma.sync.aligned.m16n8k16.row.col.f32.f16.f16.f32 "
        "{%0,%1,%2,%3}, {%4,%5,%6,%7}, {%8,%9}, {%0,%1,%2,%3};\n"
        : "+f"(d[0]), "+f"(d[1]), "+f"(d[2]), "+f"(d[3])
        : "r"(a[0]), "r"(a[1]), "r"(a[2]), "r"(a[3]), "r"(b[0]), "r"(b[1]));
}

// Pack two f32 into one b32 register of two f16 (mma A operand element).
__device__ __forceinline__ unsigned pack_h2(float lo, float hi) {
    __half2 h = __floats2half2_rn(lo, hi);
    return *reinterpret_cast<unsigned*>(&h);
}

constexpr float NEG_INF = -1e30f;

// BQ query rows per block (4 warps x 16). BK cached positions per KV tile.
template <int HD, int BQ, int BK>
__device__ __forceinline__ void fattn_prefill_core(
    __half* __restrict__ out, const __half* __restrict__ q,
    const __half* __restrict__ k_cache, const __half* __restrict__ v_cache,
    const int32_t* __restrict__ page_table, int base_pos, int n_q_heads,
    int n_kv_heads, int page_size, float scale, int n_tokens) {
    constexpr int KC = HD / 16;         // QK^T k-chunks (head_dim / 16)
    constexpr int NSUB = BK / 8;        // S n-subtiles (8 keys each)
    constexpr int KCK = BK / 16;        // P·V k-chunks (16 keys each)
    constexpr int HN = HD / 8;          // O n-subtiles (8 head-dims each)

    const int warp = threadIdx.x >> 5;
    const int lane = threadIdx.x & 31;
    const int q0 = blockIdx.x * BQ;     // first query row of the block
    const int qh = blockIdx.y;          // query head
    const int kvh = qh / (n_q_heads / n_kv_heads);
    const int mrow = q0 + warp * 16;    // this warp's first query row

    __shared__ __half qs[BQ * HD];
    __shared__ __half ks[BK * HD];   // [key][head_dim]
    __shared__ __half vs[HD * BK];   // [head_dim][key] (transposed for P·V)

    // Stage the block's Q tile once (rows past n_tokens duplicate the last, then
    // get masked at write-out). 8 halfs per vectorized store.
    for (int e = threadIdx.x * 8; e < BQ * HD; e += blockDim.x * 8) {
        int row = e / HD;
        int col = e % HD;
        int tq = q0 + row;
        if (tq > n_tokens - 1) tq = n_tokens - 1;
        *reinterpret_cast<int4*>(&qs[e]) =
            *reinterpret_cast<const int4*>(&q[(long)(tq * n_q_heads + qh) * HD + col]);
    }
    __syncthreads();

    // Preload this warp's Q fragments (reused across every KV tile). A operand
    // 16x16 per k-chunk: lane l -> group l/8, row (l%8)+(group&1)*8, col group>=2?8:0.
    unsigned qf[KC][4];
    {
        const int grp = lane >> 3;
        const int qrow = warp * 16 + (lane & 7) + ((grp & 1) << 3);
        const int qcol = (grp >> 1) << 3;
#pragma unroll
        for (int kc = 0; kc < KC; ++kc) {
            unsigned a = smem_addr(&qs[qrow * HD + kc * 16 + qcol]);
            ldmatrix_x4(a, qf[kc][0], qf[kc][1], qf[kc][2], qf[kc][3]);
        }
    }

    // Online-softmax state: each lane owns rows ra=lane/4 and rb=lane/4+8.
    float m_a = NEG_INF, m_b = NEG_INF;
    float l_a = 0.0f, l_b = 0.0f;
    float acc[HN][4];
#pragma unroll
    for (int i = 0; i < HN; ++i)
#pragma unroll
        for (int j = 0; j < 4; ++j) acc[i][j] = 0.0f;

    // Highest absolute position any query in this block attends.
    int tok_hi = q0 + BQ;
    if (tok_hi > n_tokens) tok_hi = n_tokens;
    const int max_abs = base_pos + tok_hi - 1;

    for (int pos0 = 0; pos0 <= max_abs; pos0 += BK) {
        int n_valid = max_abs + 1 - pos0;
        if (n_valid > BK) n_valid = BK;
        __syncthreads();
        // Stage K/V tile [BK][HD] from the paged cache (widen to f16 verbatim).
        // K stays [key][head_dim]; V is written transposed to [head_dim][key] so
        // the P·V mma reads it non-transposed (mma .row.col wants Bstored[n][k]).
        for (int e = threadIdx.x * 8; e < BK * HD; e += blockDim.x * 8) {
            int row = e / HD;
            int col = e % HD;
            int pos = pos0 + row;
            if (row < n_valid) {
                int page = page_table[pos / page_size];
                long base = ((long)(page * n_kv_heads + kvh) * page_size + pos % page_size) * HD + col;
                int4 kv = *reinterpret_cast<const int4*>(&k_cache[base]);
                *reinterpret_cast<int4*>(&ks[e]) = kv;
                int4 vv = *reinterpret_cast<const int4*>(&v_cache[base]);
                const __half* vh = reinterpret_cast<const __half*>(&vv);
#pragma unroll
                for (int i = 0; i < 8; ++i) vs[(col + i) * BK + row] = vh[i];
            } else {
                *reinterpret_cast<int4*>(&ks[e]) = make_int4(0, 0, 0, 0);
#pragma unroll
                for (int i = 0; i < 8; ++i) vs[(col + i) * BK + row] = __float2half(0.0f);
            }
        }
        __syncthreads();

        // S = Q * K^T for this tile: NSUB fragments of 8 keys each.
        float s[NSUB][4];
#pragma unroll
        for (int nt = 0; nt < NSUB; ++nt) {
#pragma unroll
            for (int j = 0; j < 4; ++j) s[nt][j] = 0.0f;
#pragma unroll
            for (int kc = 0; kc < KC; ++kc) {
                // B = K as Bstored[key][head_dim] (mma .row.col: C[m][n]=Σ A[m][k]B[n][k]).
                unsigned bf[2];
                const int kkey = nt * 8 + (lane & 7);
                const int kcol = kc * 16 + ((lane >> 3) & 1) * 8;
                unsigned b = smem_addr(&ks[kkey * HD + kcol]);
                ldmatrix_x2(b, bf[0], bf[1]);
                mma_f16(s[nt], qf[kc], bf);
            }
        }

        // Online softmax. Lane owns row ra=lane/4 (s cols d0,d1) and rb=lane/4+8
        // (d2,d3). Reduce max/sum over the tile's keys (in-register over NSUB,
        // then across the 4 lanes of the row-group), rescale acc, exp in place.
        float local_a = NEG_INF, local_b = NEG_INF;
#pragma unroll
        for (int nt = 0; nt < NSUB; ++nt) {
            int key_a = nt * 8 + (lane & 3) * 2;
            // Mask keys beyond n_valid (tile tail) and beyond each query's causal
            // horizon. Causal: query row r (absolute base_pos+q0+..) attends
            // pos0+key <= base_pos+global_row  =>  key <= global_row - pos0.
            float s0 = s[nt][0], s1 = s[nt][1], s2 = s[nt][2], s3 = s[nt][3];
            int ga = q0 + warp * 16 + (lane >> 2);      // global query row (ra)
            int gb = ga + 8;                            // (rb)
            int ha = base_pos + ga - pos0;              // max key idx for ra
            int hb = base_pos + gb - pos0;
            if (key_a >= n_valid || key_a > ha) s0 = NEG_INF;
            if (key_a + 1 >= n_valid || key_a + 1 > ha) s1 = NEG_INF;
            if (key_a >= n_valid || key_a > hb) s2 = NEG_INF;
            if (key_a + 1 >= n_valid || key_a + 1 > hb) s3 = NEG_INF;
            s0 *= scale; s1 *= scale; s2 *= scale; s3 *= scale;
            s[nt][0] = s0; s[nt][1] = s1; s[nt][2] = s2; s[nt][3] = s3;
            local_a = fmaxf(local_a, fmaxf(s0, s1));
            local_b = fmaxf(local_b, fmaxf(s2, s3));
        }
        // Reduce across the 4 lanes sharing a row (lane&3 = 0..3).
        local_a = fmaxf(local_a, __shfl_xor_sync(0xffffffff, local_a, 1));
        local_a = fmaxf(local_a, __shfl_xor_sync(0xffffffff, local_a, 2));
        local_b = fmaxf(local_b, __shfl_xor_sync(0xffffffff, local_b, 1));
        local_b = fmaxf(local_b, __shfl_xor_sync(0xffffffff, local_b, 2));

        float new_m_a = fmaxf(m_a, local_a);
        float new_m_b = fmaxf(m_b, local_b);
        float corr_a = (m_a <= NEG_INF) ? 1.0f : __expf(m_a - new_m_a);
        float corr_b = (m_b <= NEG_INF) ? 1.0f : __expf(m_b - new_m_b);
        m_a = new_m_a;
        m_b = new_m_b;
        l_a *= corr_a;
        l_b *= corr_b;
        // Rescale the O accumulator (rows ra held in acc[*][0,1], rb in [2,3]).
#pragma unroll
        for (int i = 0; i < HN; ++i) {
            acc[i][0] *= corr_a;
            acc[i][1] *= corr_a;
            acc[i][2] *= corr_b;
            acc[i][3] *= corr_b;
        }

        // exp(S - m) in place -> P; accumulate row sums.
        float sum_a = 0.0f, sum_b = 0.0f;
#pragma unroll
        for (int nt = 0; nt < NSUB; ++nt) {
            float p0 = __expf(s[nt][0] - m_a);
            float p1 = __expf(s[nt][1] - m_a);
            float p2 = __expf(s[nt][2] - m_b);
            float p3 = __expf(s[nt][3] - m_b);
            s[nt][0] = p0; s[nt][1] = p1; s[nt][2] = p2; s[nt][3] = p3;
            sum_a += p0 + p1;
            sum_b += p2 + p3;
        }
        sum_a += __shfl_xor_sync(0xffffffff, sum_a, 1);
        sum_a += __shfl_xor_sync(0xffffffff, sum_a, 2);
        sum_b += __shfl_xor_sync(0xffffffff, sum_b, 1);
        sum_b += __shfl_xor_sync(0xffffffff, sum_b, 2);
        l_a += sum_a;
        l_b += sum_b;

        // O += P * V. P (16 query x BK key) reinterpreted as mma A operand: the S
        // accumulator layout equals the A-operand layout, so no repack — pack the
        // f32 probs to f16 pairs. B = V via ldmatrix.trans (keys x head-dim).
#pragma unroll
        for (int kck = 0; kck < KCK; ++kck) {
            unsigned pf[4];
            pf[0] = pack_h2(s[2 * kck][0], s[2 * kck][1]);
            pf[1] = pack_h2(s[2 * kck][2], s[2 * kck][3]);
            pf[2] = pack_h2(s[2 * kck + 1][0], s[2 * kck + 1][1]);
            pf[3] = pack_h2(s[2 * kck + 1][2], s[2 * kck + 1][3]);
#pragma unroll
            for (int hn = 0; hn < HN; ++hn) {
                // B = V as Bstored[head_dim][key] (vs is transposed). Free dim N =
                // 8 head-dims (rows), contraction K = 16 keys (cols).
                unsigned bf[2];
                const int vdim = hn * 8 + (lane & 7);
                const int vkeycol = kck * 16 + ((lane >> 3) & 1) * 8;
                unsigned bptr = smem_addr(&vs[vdim * BK + vkeycol]);
                ldmatrix_x2(bptr, bf[0], bf[1]);
                mma_f16(acc[hn], pf, bf);
            }
        }
        (void)n_valid;
    }

    // Write O = acc / l. Lane owns rows ra=lane/4, rb=lane/4+8; each O n-subtile
    // (8 head-dims) holds cols (lane%4)*2, +1 in d0,d1 (ra) and d2,d3 (rb).
    const int ra = mrow + (lane >> 2);
    const int rb = ra + 8;
    const float inv_a = (l_a > 0.0f) ? 1.0f / l_a : 0.0f;
    const float inv_b = (l_b > 0.0f) ? 1.0f / l_b : 0.0f;
#pragma unroll
    for (int hn = 0; hn < HN; ++hn) {
        int col = hn * 8 + (lane & 3) * 2;
        if (ra < n_tokens) {
            long o = (long)(ra * n_q_heads + qh) * HD + col;
            out[o] = __float2half(acc[hn][0] * inv_a);
            out[o + 1] = __float2half(acc[hn][1] * inv_a);
        }
        if (rb < n_tokens) {
            long o = (long)(rb * n_q_heads + qh) * HD + col;
            out[o] = __float2half(acc[hn][2] * inv_b);
            out[o + 1] = __float2half(acc[hn][3] * inv_b);
        }
    }
}

}  // namespace

#define FORGE_FATTN_ENTRY(NAME, HD, BQ, BK)                                     \
    extern "C" __global__ void __launch_bounds__((BQ / 16) * 32) NAME(          \
        __half* out, const __half* q, const __half* k_cache,                    \
        const __half* v_cache, const int32_t* page_table, int base_pos,        \
        int n_q_heads, int n_kv_heads, int page_size, float scale,             \
        int n_tokens) {                                                         \
        fattn_prefill_core<HD, BQ, BK>(out, q, k_cache, v_cache, page_table,    \
                                       base_pos, n_q_heads, n_kv_heads,         \
                                       page_size, scale, n_tokens);            \
    }

FORGE_FATTN_ENTRY(forge_attn_prefill_fa_f16_hd128, 128, 64, 32)
FORGE_FATTN_ENTRY(forge_attn_prefill_fa_f16_hd64, 64, 64, 32)
