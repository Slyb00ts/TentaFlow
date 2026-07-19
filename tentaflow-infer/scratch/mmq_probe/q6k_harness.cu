// Correctness harness: vendored llama.cpp Q6_K MMQ (mmq_q4k.cu) vs CPU golden.
// Synthesizes random-but-valid Q6_K weight blocks (210 B) + f16 activation,
// dequantizes the SAME weight bytes on CPU (canonical ggml Q6_K dequant), computes
// the golden GEMM, then runs the GPU quantize(D4)+MMQ path (which now writes f16
// directly) and compares. Weight quant cancels (same bytes both sides); residual
// error is the q8_1 activation quant + f16 output rounding (~few e-3).
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <cmath>
#include <vector>
#include <cuda_fp16.h>
#include <cuda_runtime.h>

// Entry points from mmq_q4k.cu (compiled into this binary). GEMM writes __half.
extern "C" __global__ void forge_quantize_mmq_q8_1_d4(
    const __half*, void*, int64_t, int64_t, int64_t, int);
#define DECL(MMQX) extern "C" __global__ void forge_mmq_q6k_x##MMQX##_nc( \
    const char*, const int*, __half*, int, int, int, int, int, int);
DECL(64) DECL(128)

static inline float half2f(uint16_t h){ __half x; memcpy(&x,&h,2); return __half2float(x); }

// Canonical ggml dequant of one 256-elem Q6_K block (210 B) into out[256].
// Layout: ql[128], qh[64], scales[16] (int8), d (half).
static void dequant_q6k(const uint8_t* blk, float* out){
    const float d = half2f(*(const uint16_t*)(blk + 208));
    const uint8_t* ql = blk;
    const uint8_t* qh = blk + 128;
    const int8_t*  sc = (const int8_t*)(blk + 192);
    float* y = out;
    for (int n = 0; n < 256; n += 128){
        for (int l = 0; l < 32; ++l){
            int is = l/16;
            const int8_t q1 = (int8_t)((ql[l +  0] & 0xF) | (((qh[l] >> 0) & 3) << 4)) - 32;
            const int8_t q2 = (int8_t)((ql[l + 32] & 0xF) | (((qh[l] >> 2) & 3) << 4)) - 32;
            const int8_t q3 = (int8_t)((ql[l +  0]  >> 4) | (((qh[l] >> 4) & 3) << 4)) - 32;
            const int8_t q4 = (int8_t)((ql[l + 32]  >> 4) | (((qh[l] >> 6) & 3) << 4)) - 32;
            y[l +  0] = d * sc[is + 0] * q1;
            y[l + 32] = d * sc[is + 2] * q2;
            y[l + 64] = d * sc[is + 4] * q3;
            y[l + 96] = d * sc[is + 6] * q4;
        }
        y  += 128; ql += 64; qh += 32; sc += 8;
    }
}

#define CK(x) do{ cudaError_t e=(x); if(e){printf("CUDA err %s @%d: %s\n",#x,__LINE__,cudaGetErrorString(e));exit(1);} }while(0)

int main(int argc, char** argv){
    const int N = argc>1?atoi(argv[1]):4096;   // rows (output features)
    const int K = argc>2?atoi(argv[2]):512;    // cols (reduction), mult of 256
    const int T = argc>3?atoi(argv[3]):512;    // tokens
    const int MMQX = argc>4?atoi(argv[4]):128;
    if (K%256){ printf("K must be mult 256\n"); return 1; }
    srand(4321);
    const int kblk = K/256;
    const int BB = 210; // sizeof(block_q6_K)
    std::vector<uint8_t> W((size_t)N*kblk*BB);
    for (auto& b : W) b = rand() & 0xFF;
    // Overwrite d (f16) with a realistic small scale; random bytes give inf/nan.
    // scales (int8), ql, qh stay random. int8 scales are bounded so no overflow.
    for (size_t blkidx=0; blkidx*BB < W.size(); blkidx++){
        uint8_t* p = &W[blkidx*BB];
        __half d = __float2half(0.005f + 0.02f*(rand()/(float)RAND_MAX));
        memcpy(p+208, &d, 2);
    }
    // f16 activation (matches engine prefill dtype); golden uses f16-rounded values.
    std::vector<__half> Xh((size_t)T*K);
    std::vector<float>  X((size_t)T*K);
    for (size_t i=0;i<X.size();i++){ __half h=__float2half((rand()/(float)RAND_MAX)*2.f-1.f); Xh[i]=h; X[i]=__half2float(h); }

    // CPU golden: Wref[n][k], golden[t*N+n] = sum_k X[t][k]*Wref[n][k]
    std::vector<float> Wref((size_t)N*K);
    std::vector<float> blk(256);
    for (int n=0;n<N;n++) for (int kb=0;kb<kblk;kb++){
        dequant_q6k(&W[((size_t)n*kblk+kb)*BB], blk.data());
        for (int j=0;j<256;j++) Wref[(size_t)n*K + kb*256 + j] = blk[j];
    }
    std::vector<float> golden((size_t)T*N, 0.f);
    for (int t=0;t<T;t++) for (int n=0;n<N;n++){
        double acc=0; const float* xr=&X[(size_t)t*K]; const float* wr=&Wref[(size_t)n*K];
        for (int k=0;k<K;k++) acc += (double)xr[k]*wr[k];
        golden[(size_t)t*N+n]=(float)acc;
    }

    // GPU buffers
    const int Kpad = ((K + 511)/512)*512;
    const int QMMQ = 144; // sizeof(block_q8_1_mmq)
    const size_t q8n = (size_t)(Kpad/128)*T*QMMQ + 128*QMMQ;
    char *dW; int *dQ; __half *dX; __half *dDst;
    CK(cudaMalloc(&dW, W.size()));
    CK(cudaMalloc(&dX, X.size()*2));
    CK(cudaMalloc(&dQ, q8n));
    CK(cudaMalloc(&dDst, (size_t)T*N*2));
    CK(cudaMemcpy(dW, W.data(), W.size(), cudaMemcpyHostToDevice));
    CK(cudaMemcpy(dX, Xh.data(), X.size()*2, cudaMemcpyHostToDevice));
    CK(cudaMemset(dQ, 0, q8n));

    // Quantize (D4): grid(ne1=T, block_num_y=ceil(Kpad/512), 1), block 128
    dim3 qg(T, (Kpad + 4*128-1)/(4*128), 1); dim3 qb(128,1,1);
    forge_quantize_mmq_q8_1_d4<<<qg,qb>>>(dX, dQ, K, K, Kpad, T);
    CK(cudaGetLastError()); CK(cudaDeviceSynchronize());

    // MMQ: grid(nty=ceil(N/128), ntx=ceil(T/mmqx), 1), block(32,8,1)
    dim3 g((N+127)/128, (T+MMQX-1)/MMQX, 1); dim3 b(32,8,1);
    auto pad=[&](int v,int a){ return ((v+a-1)/a)*a; };
    int nbs_ids = MMQX*4;
    int nbs_x   = 128*76*4;              // mmq_y * MMQ_MMA_TILE_X_K_Q6_K(=76) * 4
    int nbs_y   = pad(MMQX*QMMQ, 8*32*4);
    int smem    = nbs_ids + nbs_x + nbs_y;
    void* fn = (MMQX==128)?(void*)forge_mmq_q6k_x128_nc:(void*)forge_mmq_q6k_x64_nc;
    CK(cudaFuncSetAttribute(fn, cudaFuncAttributeMaxDynamicSharedMemorySize, smem));
    if (MMQX==128)
        forge_mmq_q6k_x128_nc<<<g,b,smem>>>(dW,dQ,dDst, K,N,T, kblk, T, N);
    else
        forge_mmq_q6k_x64_nc<<<g,b,smem>>>(dW,dQ,dDst, K,N,T, kblk, T, N);
    CK(cudaGetLastError()); CK(cudaDeviceSynchronize());

    std::vector<__half> dsth((size_t)T*N);
    CK(cudaMemcpy(dsth.data(), dDst, dsth.size()*2, cudaMemcpyDeviceToHost));

    // Compare
    double num=0, den=0, maxrel=0; int worst=-1;
    for (size_t i=0;i<dsth.size();i++){
        double gpu = __half2float(dsth[i]);
        double diff = gpu-golden[i];
        num += diff*diff; den += (double)golden[i]*golden[i];
        double rel = fabs(diff)/(fabs(golden[i])+1e-3);
        if (rel>maxrel){maxrel=rel;worst=(int)i;}
    }
    double rl2 = sqrt(num/den);
    printf("Q6_K N=%d K=%d T=%d mmq_x=%d smem=%d : relL2=%.3e maxrel=%.3e (golden[%d]=%.4f gpu=%.4f)\n",
        N,K,T,MMQX,smem, rl2, maxrel, worst, worst>=0?golden[worst]:0, worst>=0?__half2float(dsth[worst]):0);
    printf("%s\n", rl2 < 5e-3 ? "PASS" : "FAIL");
    return rl2 < 5e-3 ? 0 : 1;
}
