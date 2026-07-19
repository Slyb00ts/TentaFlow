// Correctness harness: vendored llama.cpp Q4_K MMQ (mmq_q4k.cu) vs CPU golden.
// Synthesizes random-but-valid Q4_K weight blocks + f32 activation, dequantizes
// the SAME weight bytes on CPU (canonical ggml Q4_K dequant), computes the golden
// GEMM, then runs the GPU quantize+MMQ path and compares. Weight quant cancels
// (same bytes both sides); residual error is the q8_1 activation quant (~few e-3).
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <cmath>
#include <vector>
#include <cuda_fp16.h>
#include <cuda_runtime.h>

// Entry points from mmq_q4k.cu (compiled into this binary).
extern "C" __global__ void forge_quantize_mmq_q8_1_ds4(
    const __half*, void*, int64_t, int64_t, int64_t, int);
#define DECL(MMQX) extern "C" __global__ void forge_mmq_q4k_x##MMQX##_nc( \
    const char*, const int*, float*, int, int, int, int, int, int);
DECL(64) DECL(128)

static inline float half2f(uint16_t h){ __half x; memcpy(&x,&h,2); return __half2float(x); }

static void get_scale_min_k4(int j, const uint8_t* s, int& sc, int& mn){
    if (j < 4){ sc = s[j] & 63; mn = s[4+j] & 63; }
    else { sc = (s[4+j] & 0x0F) | ((s[j-4] >> 6) << 4);
           mn = (s[4+j] >> 4)  | ((s[j]   >> 6) << 4); }
}
// Canonical ggml dequant of one 256-elem Q4_K block into out[256].
static void dequant_q4k(const uint8_t* blk, float* out){
    const float d    = half2f(*(const uint16_t*)(blk));
    const float dmin = half2f(*(const uint16_t*)(blk+2));
    const uint8_t* scales = blk + 4;
    const uint8_t* q = blk + 16;
    int is = 0; float* y = out;
    for (int j = 0; j < 256; j += 64){
        int sc, mn; get_scale_min_k4(is+0, scales, sc, mn); float d1=d*sc, m1=dmin*mn;
        get_scale_min_k4(is+1, scales, sc, mn); float d2=d*sc, m2=dmin*mn;
        for (int l=0;l<32;l++) y[l]    = d1*(q[l] & 0xF) - m1;
        for (int l=0;l<32;l++) y[l+32] = d2*(q[l] >> 4)  - m2;
        q += 32; is += 2; y += 64;
    }
}

#define CK(x) do{ cudaError_t e=(x); if(e){printf("CUDA err %s @%d: %s\n",#x,__LINE__,cudaGetErrorString(e));exit(1);} }while(0)

int main(int argc, char** argv){
    const int N = argc>1?atoi(argv[1]):4096;   // rows (output features)
    const int K = argc>2?atoi(argv[2]):512;    // cols (reduction), mult of 256
    const int T = argc>3?atoi(argv[3]):512;    // tokens
    const int MMQX = argc>4?atoi(argv[4]):128;
    if (K%256){ printf("K must be mult 256\n"); return 1; }
    srand(1234);
    const int kblk = K/256;
    std::vector<uint8_t> W((size_t)N*kblk*144);
    for (auto& b : W) b = rand() & 0xFF;
    // Overwrite the f16 d/dmin of each block with realistic small scales;
    // random bytes there produce inf/nan half values. scales[12]+qs[128] stay random.
    for (size_t blkidx=0; blkidx*144 < W.size(); blkidx++){
        uint8_t* p = &W[blkidx*144];
        __half d    = __float2half(0.02f + 0.06f*(rand()/(float)RAND_MAX));
        __half dmin = __float2half(0.01f + 0.03f*(rand()/(float)RAND_MAX));
        memcpy(p,   &d,    2);
        memcpy(p+2, &dmin, 2);
    }
    // f16 activation (matches engine prefill dtype); golden uses the f16-rounded
    // values so only the q8_1 quant contributes error.
    std::vector<__half> Xh((size_t)T*K);
    std::vector<float>  X((size_t)T*K);
    for (size_t i=0;i<X.size();i++){ __half h=__float2half((rand()/(float)RAND_MAX)*2.f-1.f); Xh[i]=h; X[i]=__half2float(h); }

    // CPU golden: Wref[n][k], golden[t*N+n] = sum_k X[t][k]*Wref[n][k]
    std::vector<float> Wref((size_t)N*K);
    std::vector<float> blk(256);
    for (int n=0;n<N;n++) for (int kb=0;kb<kblk;kb++){
        dequant_q4k(&W[((size_t)n*kblk+kb)*144], blk.data());
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
    char *dW; int *dQ; __half *dX; float *dDst;
    CK(cudaMalloc(&dW, W.size()));
    CK(cudaMalloc(&dX, X.size()*2));
    CK(cudaMalloc(&dQ, q8n));
    CK(cudaMalloc(&dDst, (size_t)T*N*4));
    CK(cudaMemcpy(dW, W.data(), W.size(), cudaMemcpyHostToDevice));
    CK(cudaMemcpy(dX, Xh.data(), X.size()*2, cudaMemcpyHostToDevice));
    CK(cudaMemset(dQ, 0, q8n));

    // Quantize: grid(ne1=T, block_num_y=ceil(Kpad/512), 1), block 128
    dim3 qg(T, (Kpad + 4*128-1)/(4*128), 1); dim3 qb(128,1,1);
    forge_quantize_mmq_q8_1_ds4<<<qg,qb>>>(dX, dQ, K, K, Kpad, T);
    CK(cudaGetLastError()); CK(cudaDeviceSynchronize());

    // MMQ: grid(nty=ceil(N/128), ntx=ceil(T/mmqx), 1), block(32,8,1)
    dim3 g((N+127)/128, (T+MMQX-1)/MMQX, 1); dim3 b(32,8,1);
    // dynamic smem: nbs_ids + nbs_x + PAD(nbs_y, nwarps*warp*4)
    auto pad=[&](int v,int a){ return ((v+a-1)/a)*a; };
    int nbs_ids = MMQX*4;
    int nbs_x   = 128*76*4;              // mmq_y * MMQ_MMA_TILE_X_K_Q8_1 * 4
    int nbs_y   = pad(MMQX*QMMQ, 8*32*4);
    int smem    = nbs_ids + nbs_x + nbs_y;
    void* fn = (MMQX==128)?(void*)forge_mmq_q4k_x128_nc:(void*)forge_mmq_q4k_x64_nc;
    CK(cudaFuncSetAttribute(fn, cudaFuncAttributeMaxDynamicSharedMemorySize, smem));
    if (MMQX==128)
        forge_mmq_q4k_x128_nc<<<g,b,smem>>>(dW,dQ,dDst, K,N,T, kblk, T, N);
    else
        forge_mmq_q4k_x64_nc<<<g,b,smem>>>(dW,dQ,dDst, K,N,T, kblk, T, N);
    CK(cudaGetLastError()); CK(cudaDeviceSynchronize());

    std::vector<float> dst((size_t)T*N);
    CK(cudaMemcpy(dst.data(), dDst, dst.size()*4, cudaMemcpyDeviceToHost));

    // Compare
    double num=0, den=0, maxrel=0; int worst=-1;
    for (size_t i=0;i<dst.size();i++){
        double diff = (double)dst[i]-golden[i];
        num += diff*diff; den += (double)golden[i]*golden[i];
        double rel = fabs(diff)/(fabs(golden[i])+1e-3);
        if (rel>maxrel){maxrel=rel;worst=(int)i;}
    }
    double rl2 = sqrt(num/den);
    printf("N=%d K=%d T=%d mmq_x=%d smem=%d : relL2=%.3e maxrel=%.3e (golden[%d]=%.4f gpu=%.4f)\n",
        N,K,T,MMQX,smem, rl2, maxrel, worst, worst>=0?golden[worst]:0, worst>=0?dst[worst]:0);
    printf("%s\n", rl2 < 5e-3 ? "PASS" : "FAIL");
    return rl2 < 5e-3 ? 0 : 1;
}
