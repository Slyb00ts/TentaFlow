// Standalone W4A8 correctness + perf harness (QServe w4a8_per_group dense_kernel0).
// De-risks the QServe weight interleave + per-token int8 activation quant on
// small shapes BEFORE any FORGE engine wiring. Reproduces the exact 8-D weight
// permute from omniserve/.../w4a8_linear.py in host C++, runs the kernel, and
// compares against a CPU int4xint8 golden. Nothing here is committed to FORGE.
//
// Build: nvcc -arch=sm_89 -O3 -o harness harness.cu
// Run:   ./harness            (correctness on small shapes + Phase-A bench)

#include <cuda_fp16.h>
#include <cuda_pipeline_primitives.h>
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <cmath>
#include <vector>
#include <random>
#include <algorithm>

#include "qserve_device.inc"

// Non-torch launcher — mirrors gemm_forward_cuda's dispatch (prefill branch:
// num_out_feats > 128 -> CTA_M=128,CTA_N=64,CTA_K=64,WARP 64/32/64,STAGES=4).
static void w4a8_launch(int8_t* in_feats, int8_t* kernel, int8_t* zeros,
                        int8_t* scales_i8, half2* wscales, half* ascales,
                        half* out_feats, int num_in_feats, int num_out_channels,
                        int num_in_channels) {
  constexpr int G = 128;
  const int num_out_feats = num_in_feats; // KERNEL_LAUNCH_CODE alias (M = tokens)
  if (num_in_feats > 128) {
    constexpr int CTA_M = 128, CTA_N = 64, CTA_K = 64;
    constexpr int WARP_M = 64, WARP_N = 32, WARP_K = 64, STAGES = 4;
    KERNEL_LAUNCH_CODE
  } else if (num_in_feats >= 128) {
    if (num_in_channels <= 4096) {
      constexpr int CTA_M = 64, CTA_N = 64, CTA_K = 64;
      constexpr int WARP_M = 32, WARP_N = 32, WARP_K = 64, STAGES = 4;
      KERNEL_LAUNCH_CODE
    } else {
      constexpr int CTA_M = 64, CTA_N = 64, CTA_K = 128;
      constexpr int WARP_M = 32, WARP_N = 32, WARP_K = 64, STAGES = 3;
      KERNEL_LAUNCH_CODE
    }
  } else {
    constexpr int CTA_M = 32, CTA_N = 64, CTA_K = 128;
    constexpr int WARP_M = 32, WARP_N = 32, WARP_K = 64, STAGES = 3;
    KERNEL_LAUNCH_CODE
  }
}

#define CK(x) do { cudaError_t e=(x); if(e){printf("CUDA %s:%d %s\n",__FILE__,__LINE__,cudaGetErrorString(e));exit(1);} } while(0)

// ---- Host-side QServe quant + pack (reproduces w4a8_linear.py from_linear) ----
// W is [N=out_ch][K=in_ch] fp32. group_size G=128.
struct Packed {
  std::vector<int8_t> qweight;   // [N * K/2] interleaved int4 (2 per byte)
  std::vector<int8_t> s2_scales; // [K/G][N] reordered int8
  std::vector<int8_t> s2_zeros;  // [K/G][N] reordered int8 = (-zero)*s2
  std::vector<half>   s1_scales; // [N] fp16 per channel
  // reference values for CPU golden (un-reordered, natural layout):
  std::vector<int>    q4;        // [N][K] int4 codes in [0,15]
  std::vector<int>    s2;        // [N][K/G] int8 scale
  std::vector<int>    zero;      // [N][K/G] int zero point (s2_zero)
  std::vector<float>  s1;        // [N] fp16-rounded per-channel scale
};

static Packed qserve_quant_pack(const std::vector<float>& W, int N, int K, int G) {
  Packed p;
  int KG = K / G;
  p.q4.assign((size_t)N * K, 0);
  p.s2.assign((size_t)N * KG, 0);
  p.zero.assign((size_t)N * KG, 0);
  p.s1.assign(N, 0);
  p.s1_scales.assign(N, __float2half(0.f));
  std::vector<int> wi8((size_t)N * K, 0);

  for (int n = 0; n < N; ++n) {
    // Stage 1: per-channel fp16 scale -> int8
    float amax = 0.f;
    for (int k = 0; k < K; ++k) amax = std::max(amax, std::fabs(W[(size_t)n * K + k]));
    float s1f = amax > 0 ? amax / 127.f : 1.f;
    s1f = __half2float(__float2half(s1f)); // s1 is stored fp16
    p.s1[n] = s1f;
    p.s1_scales[n] = __float2half(s1f);
    for (int k = 0; k < K; ++k) {
      int v = (int)std::lround(W[(size_t)n * K + k] / s1f);
      v = std::max(-127, std::min(127, v));
      wi8[(size_t)n * K + k] = v;
    }
    // Stage 2: per-group int8 scale + int zero -> int4
    for (int gi = 0; gi < KG; ++gi) {
      int lo = 1 << 30, hi = -(1 << 30);
      for (int j = 0; j < G; ++j) {
        int v = wi8[(size_t)n * K + gi * G + j];
        lo = std::min(lo, v); hi = std::max(hi, v);
      }
      int s2 = (hi - lo + 14) / 15; if (s2 < 1) s2 = 1;
      if (s2 > 127) s2 = 127;
      int zero = (int)std::lround(-(double)lo / s2);
      zero = std::max(0, std::min(15, zero));
      p.s2[(size_t)n * KG + gi] = s2;
      p.zero[(size_t)n * KG + gi] = zero;
      for (int j = 0; j < G; ++j) {
        int v = wi8[(size_t)n * K + gi * G + j];
        int q = (int)std::lround((double)v / s2) + zero;
        q = std::max(0, std::min(15, q));
        p.q4[(size_t)n * K + gi * G + j] = q;
      }
    }
  }

  // ---- Weight repack: reshape [N/32,2,2,8, K/32,2,4,4], permute to
  // [d0,d4,d3,d6,d5,d2,d7,d1], byte = (q[d1=1]<<4)|q[d1=0]. Flat index over
  // the first 7 dims is the contiguous qweight byte index (N*K/2 bytes).
  p.qweight.assign((size_t)N * K / 2, 0);
  int N32 = N / 32, K32 = K / 32;
  for (int d0 = 0; d0 < N32; ++d0)
   for (int d4 = 0; d4 < K32; ++d4)
    for (int d3 = 0; d3 < 8; ++d3)
     for (int d6 = 0; d6 < 4; ++d6)
      for (int d5 = 0; d5 < 2; ++d5)
       for (int d2 = 0; d2 < 2; ++d2)
        for (int d7 = 0; d7 < 4; ++d7) {
          size_t flat7 = ((((((size_t)d0 * K32 + d4) * 8 + d3) * 4 + d6) * 2 + d5) * 2 + d2) * 4 + d7;
          int nibs[2];
          for (int d1 = 0; d1 < 2; ++d1) {
            int oc = d0 * 32 + d1 * 16 + d2 * 8 + d3;
            int ic = d4 * 32 + d5 * 16 + d6 * 4 + d7;
            nibs[d1] = p.q4[(size_t)oc * K + ic] & 0xF;
          }
          p.qweight[flat7] = (int8_t)((nibs[1] << 4) | nibs[0]);
        }

  // ---- Scale/zero repack: [N,KG] -> transpose [KG,N] -> within each 32 block,
  // reorder j -> (j%8)*4 + (j//8). zeros stored as (-zero)*s2 (2's complement).
  p.s2_scales.assign((size_t)KG * N, 0);
  p.s2_zeros.assign((size_t)KG * N, 0);
  for (int gi = 0; gi < KG; ++gi)
    for (int nb = 0; nb < N32; ++nb)
      for (int j = 0; j < 32; ++j) {
        int oc = nb * 32 + j;
        int newj = (j % 8) * 4 + (j / 8);
        int s2 = p.s2[(size_t)oc * KG + gi];
        int zero = p.zero[(size_t)oc * KG + gi];
        p.s2_scales[(size_t)gi * N + nb * 32 + newj] = (int8_t)s2;
        p.s2_zeros[(size_t)gi * N + nb * 32 + newj] = (int8_t)((-zero) * s2);
      }
  return p;
}

// Per-token int8 activation quant. A[M][K] fp32 -> a_i8[M][K], ascale[M] fp16.
static void quant_act(const std::vector<float>& A, int M, int K,
                      std::vector<int8_t>& ai8, std::vector<half>& ascale,
                      std::vector<int>& ai8_ref) {
  ai8.assign((size_t)M * K, 0);
  ascale.assign(M, __float2half(0.f));
  ai8_ref.assign((size_t)M * K, 0);
  for (int m = 0; m < M; ++m) {
    float amax = 0.f;
    for (int k = 0; k < K; ++k) amax = std::max(amax, std::fabs(A[(size_t)m * K + k]));
    float s = amax > 0 ? amax / 127.f : 1.f;
    s = __half2float(__float2half(s));
    ascale[m] = __float2half(s);
    for (int k = 0; k < K; ++k) {
      int v = (int)std::lround(A[(size_t)m * K + k] / s);
      v = std::max(-127, std::min(127, v));
      ai8[(size_t)m * K + k] = (int8_t)v;
      ai8_ref[(size_t)m * K + k] = v;
    }
  }
}

// CPU golden replicating the kernel's math exactly:
// C[m][n] = ascale[m]*s1[n] * sum_k a_i8[m][k] * s2[n][k/G] * (q4[n][k]-zero[n][k/G])
static void cpu_golden(const Packed& p, const std::vector<int>& ai8,
                       const std::vector<half>& ascale, int M, int N, int K,
                       int G, std::vector<float>& C) {
  int KG = K / G;
  C.assign((size_t)M * N, 0.f);
  #pragma omp parallel for schedule(dynamic)
  for (int m = 0; m < M; ++m) {
    float as = __half2float(ascale[m]);
    for (int n = 0; n < N; ++n) {
      long acc = 0;
      for (int gi = 0; gi < KG; ++gi) {
        int s2 = p.s2[(size_t)n * KG + gi];
        int z = p.zero[(size_t)n * KG + gi];
        for (int j = 0; j < G; ++j) {
          int k = gi * G + j;
          // Kernel reconstructs w_i8 bytewise: (q4*s2 + (-z*s2)) mod 256 as int8.
          int8_t wrec = (int8_t)(s2 * (p.q4[(size_t)n * K + k] - z));
          acc += (long)ai8[(size_t)m * K + k] * (long)wrec;
        }
      }
      C[(size_t)m * N + n] = as * p.s1[n] * (float)acc;
    }
  }
}

static void run_case(int M, int N, int K, bool bench, bool check) {
  const int G = 128;
  std::mt19937 rng(1234 + M * 131 + N * 17 + K);
  std::normal_distribution<float> nd(0.f, 1.f);
  std::vector<float> W((size_t)N * K), A((size_t)M * K);
  for (auto& x : W) x = nd(rng) * 0.3f;
  for (auto& x : A) x = nd(rng) * 0.5f;

  Packed p = qserve_quant_pack(W, N, K, G);
  std::vector<int8_t> ai8; std::vector<half> ascale; std::vector<int> ai8_ref;
  quant_act(A, M, K, ai8, ascale, ai8_ref);

  std::vector<float> Cgold;
  if (check) cpu_golden(p, ai8_ref, ascale, M, N, K, G, Cgold);

  // device buffers
  int8_t *d_A, *d_W, *d_z, *d_s2; half *d_ws, *d_as, *d_C;
  int KG = K / G;
  CK(cudaMalloc(&d_A, (size_t)M * K));
  CK(cudaMalloc(&d_W, (size_t)N * K / 2));
  CK(cudaMalloc(&d_z, (size_t)KG * N));
  CK(cudaMalloc(&d_s2, (size_t)KG * N));
  CK(cudaMalloc(&d_ws, (size_t)N * sizeof(half)));
  CK(cudaMalloc(&d_as, (size_t)M * sizeof(half)));
  CK(cudaMalloc(&d_C, (size_t)M * N * sizeof(half)));
  CK(cudaMemcpy(d_A, ai8.data(), (size_t)M * K, cudaMemcpyHostToDevice));
  CK(cudaMemcpy(d_W, p.qweight.data(), (size_t)N * K / 2, cudaMemcpyHostToDevice));
  CK(cudaMemcpy(d_z, p.s2_zeros.data(), (size_t)KG * N, cudaMemcpyHostToDevice));
  CK(cudaMemcpy(d_s2, p.s2_scales.data(), (size_t)KG * N, cudaMemcpyHostToDevice));
  CK(cudaMemcpy(d_ws, p.s1_scales.data(), (size_t)N * sizeof(half), cudaMemcpyHostToDevice));
  CK(cudaMemcpy(d_as, ascale.data(), (size_t)M * sizeof(half), cudaMemcpyHostToDevice));

  w4a8_launch(d_A, d_W, d_z, d_s2, (half2*)d_ws, d_as, d_C, M, N, K);
  CK(cudaGetLastError());
  CK(cudaDeviceSynchronize());

  std::vector<half> Ch((size_t)M * N);
  CK(cudaMemcpy(Ch.data(), d_C, (size_t)M * N * sizeof(half), cudaMemcpyDeviceToHost));

  // correctness: relL2 + max abs, vs CPU golden
  if (check) {
    double se = 0, sref = 0, maxabs = 0, maxrel = 0;
    for (size_t i = 0; i < (size_t)M * N; ++i) {
      double g = Cgold[i], v = __half2float(Ch[i]);
      double d = g - v; se += d * d; sref += g * g;
      maxabs = std::max(maxabs, std::fabs(d));
      double denom = std::max(1e-3, std::fabs(g));
      maxrel = std::max(maxrel, std::fabs(d) / denom);
    }
    double relL2 = std::sqrt(se / std::max(1e-30, sref));
    printf("  M=%-5d N=%-6d K=%-6d  relL2=%.2e  maxabs=%.3e  maxrel=%.2e  %s\n",
           M, N, K, relL2, maxabs, maxrel, relL2 < 2e-2 ? "PASS" : "FAIL");
  }

  if (bench) {
    cudaEvent_t ev_s, ev_e; CK(cudaEventCreate(&ev_s)); CK(cudaEventCreate(&ev_e));
    for (int i = 0; i < 30; ++i) w4a8_launch(d_A, d_W, d_z, d_s2, (half2*)d_ws, d_as, d_C, M, N, K);
    CK(cudaDeviceSynchronize());
    // sustained warmup to reach boost clock
    for (int w = 0; w < 200; ++w) w4a8_launch(d_A, d_W, d_z, d_s2, (half2*)d_ws, d_as, d_C, M, N, K);
    CK(cudaDeviceSynchronize());
    float best = 1e30f;
    for (int rep = 0; rep < 20; ++rep) {
      CK(cudaEventRecord(ev_s));
      for (int i = 0; i < 30; ++i) w4a8_launch(d_A, d_W, d_z, d_s2, (half2*)d_ws, d_as, d_C, M, N, K);
      CK(cudaEventRecord(ev_e)); CK(cudaEventSynchronize(ev_e));
      float ms; CK(cudaEventElapsedTime(&ms, ev_s, ev_e));
      best = std::min(best, ms / 30.f);
    }
    double flop = 2.0 * M * N * K;
    printf("  BENCH M=%-5d N=%-6d K=%-6d  %.1f us   %.1f TFLOP-eq\n",
           M, N, K, best * 1e3, flop / (best * 1e-3) / 1e12);
    cudaEventDestroy(ev_s); cudaEventDestroy(ev_e);
  }

  cudaFree(d_A); cudaFree(d_W); cudaFree(d_z); cudaFree(d_s2);
  cudaFree(d_ws); cudaFree(d_as); cudaFree(d_C);
}

int main() {
  printf("== W4A8 correctness (GPU QServe vs CPU int4xint8 golden), tol relL2<2e-2 ==\n");
  run_case(256, 128, 256, false, true);
  run_case(256, 256, 512, false, true);
  run_case(384, 512, 1024, false, true);
  run_case(129, 256, 512, false, true);    // small-M branch
  run_case(160, 4096, 4096, false, true);  // q/o-like, K==4096 branch
  run_case(256, 4096, 4096, false, true);  // Mistral q/o shape
  run_case(512, 2048, 4096, false, true);  // gate/up-like (trimmed N for CPU golden)
  run_case(512, 4096, 2048, false, true);  // down-like (trimmed K)

  printf("\n== Phase-A perf reconfirm (Mistral FFN shapes, boost-clock sustained) ==\n");
  run_case(2048, 14336, 4096, true, false); // gate/up
  run_case(2048, 4096, 14336, true, false); // down
  run_case(4096, 14336, 4096, true, false);
  run_case(4096, 4096, 14336, true, false);
  run_case(512, 14336, 4096, true, false);
  run_case(512, 4096, 14336, true, false);
  return 0;
}
