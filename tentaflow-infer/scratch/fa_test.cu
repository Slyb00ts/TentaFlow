// Standalone correctness harness for fattn_prefill.cu vs a CPU reference.
#include <cstdio>
#include <cstdlib>
#include <cmath>
#include <vector>
#include <cuda_fp16.h>

#include "fattn_prefill.cu"

static float frand() { return (float)rand() / RAND_MAX * 2.0f - 1.0f; }

int main(int argc, char** argv) {
    const int HD = 128;
    int T = (argc > 1) ? atoi(argv[1]) : 100;
    int n_q_heads = (argc > 2) ? atoi(argv[2]) : 4;
    int n_kv_heads = (argc > 3) ? atoi(argv[3]) : 1;
    int base_pos = (argc > 4) ? atoi(argv[4]) : 0;
    int page_size = 32;
    float scale = 1.0f / sqrtf((float)HD);

    int total_pos = base_pos + T;
    int n_pages = (total_pos + page_size - 1) / page_size;

    srand(1234);
    std::vector<__half> q(T * n_q_heads * HD);
    std::vector<__half> kc(n_pages * n_kv_heads * page_size * HD);
    std::vector<__half> vc(n_pages * n_kv_heads * page_size * HD);
    std::vector<int32_t> pt(n_pages);
    for (auto& x : q) x = __float2half(frand());
    for (auto& x : kc) x = __float2half(frand());
    for (auto& x : vc) x = __float2half(frand());
    for (int i = 0; i < n_pages; ++i) pt[i] = i;  // identity

    // CPU reference: causal, GQA, paged.
    std::vector<float> ref(T * n_q_heads * HD, 0.0f);
    for (int t = 0; t < T; ++t) {
        for (int h = 0; h < n_q_heads; ++h) {
            int kvh = h / (n_q_heads / n_kv_heads);
            int hi = base_pos + t;  // inclusive last key
            std::vector<float> sc(hi + 1);
            float mx = -1e30f;
            for (int p = 0; p <= hi; ++p) {
                int page = pt[p / page_size];
                long kb = ((long)(page * n_kv_heads + kvh) * page_size + p % page_size) * HD;
                float d = 0;
                for (int e = 0; e < HD; ++e)
                    d += __half2float(q[(long)(t * n_q_heads + h) * HD + e]) * __half2float(kc[kb + e]);
                d *= scale;
                sc[p] = d;
                if (d > mx) mx = d;
            }
            float sum = 0;
            for (int p = 0; p <= hi; ++p) { sc[p] = expf(sc[p] - mx); sum += sc[p]; }
            for (int e = 0; e < HD; ++e) {
                float o = 0;
                for (int p = 0; p <= hi; ++p) {
                    int page = pt[p / page_size];
                    long vb = ((long)(page * n_kv_heads + kvh) * page_size + p % page_size) * HD;
                    o += sc[p] * __half2float(vc[vb + e]);
                }
                ref[(long)(t * n_q_heads + h) * HD + e] = o / sum;
            }
        }
    }

    __half *dq, *dkc, *dvc, *dout;
    int32_t* dpt;
    cudaMalloc(&dq, q.size() * 2);
    cudaMalloc(&dkc, kc.size() * 2);
    cudaMalloc(&dvc, vc.size() * 2);
    cudaMalloc(&dout, ref.size() * 2);
    cudaMalloc(&dpt, pt.size() * 4);
    cudaMemcpy(dq, q.data(), q.size() * 2, cudaMemcpyHostToDevice);
    cudaMemcpy(dkc, kc.data(), kc.size() * 2, cudaMemcpyHostToDevice);
    cudaMemcpy(dvc, vc.data(), vc.size() * 2, cudaMemcpyHostToDevice);
    cudaMemcpy(dpt, pt.data(), pt.size() * 4, cudaMemcpyHostToDevice);
    cudaMemset(dout, 0, ref.size() * 2);

    const int BQ = 64;
    dim3 grid((T + BQ - 1) / BQ, n_q_heads, 1);
    dim3 block((BQ / 16) * 32, 1, 1);
    forge_attn_prefill_fa_f16_hd128<<<grid, block>>>(
        dout, dq, dkc, dvc, dpt, base_pos, n_q_heads, n_kv_heads, page_size, scale, T);
    cudaError_t err = cudaDeviceSynchronize();
    if (err != cudaSuccess) { printf("CUDA ERR: %s\n", cudaGetErrorString(err)); return 1; }

    std::vector<__half> hout(ref.size());
    cudaMemcpy(hout.data(), dout, ref.size() * 2, cudaMemcpyDeviceToHost);

    double max_abs = 0, max_rel = 0, sum_abs = 0;
    int nbad = 0;
    for (size_t i = 0; i < ref.size(); ++i) {
        float g = __half2float(hout[i]);
        float r = ref[i];
        double a = fabs(g - r);
        double rel = a / (fabs(r) + 1e-4);
        max_abs = fmax(max_abs, a);
        max_rel = fmax(max_rel, rel);
        sum_abs += a;
        if (rel > 2e-2 && a > 2e-3) {
            if (nbad < 8) printf("  bad [%zu] gpu=%.5f ref=%.5f\n", i, g, r);
            nbad++;
        }
    }
    printf("T=%d heads=%d/%d base=%d  max_abs=%.5f max_rel=%.5f mean_abs=%.6f nbad=%d/%zu\n",
           T, n_q_heads, n_kv_heads, base_pos, max_abs, max_rel, sum_abs / ref.size(), nbad, ref.size());
    return nbad > 0 ? 2 : 0;
}
