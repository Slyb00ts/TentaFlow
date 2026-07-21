// Correctness harness: vendored llama.cpp Q4_K MMQ stream-K path (mmq_q4k.cu)
// vs CPU golden AND vs the dense conventional-tiling path. Same synthetic Q4_K
// weights + f16 activation as harness.cu. Proves the stream-K driver + fixup
// reduction match the dense kernel within f16 round-off and the CPU golden within
// q8_1 activation-quant tolerance.
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <cmath>
#include <cstring>
#include <vector>
#include <cuda_fp16.h>
#include <cuda_runtime.h>

extern "C" __global__ void forge_quantize_mmq_q8_1_ds4(
    const __half*, void*, int64_t, int64_t, int64_t, int);
#define DECL_DENSE(MMQX) extern "C" __global__ void forge_mmq_q4k_x##MMQX##_nc( \
    const char*, const int*, __half*, int, int, int, int, int, int);
#define DECL_SK(MMQX) extern "C" __global__ void forge_mmq_sk_q4k_x##MMQX##_nc( \
    const char*, const int*, __half*, float*, int, int, int, int, int, int);
#define DECL_FIX(MMQX) extern "C" __global__ void forge_mmq_fix_q4k_x##MMQX##_nc( \
    __half*, const float*, int, int, int, int);
DECL_DENSE(64) DECL_DENSE(128)
DECL_SK(64) DECL_SK(128)
DECL_FIX(64) DECL_FIX(128)

static inline float half2f(uint16_t h){ __half x; memcpy(&x,&h,2); return __half2float(x); }

static void get_scale_min_k4(int j, const uint8_t* s, int& sc, int& mn){
    if (j < 4){ sc = s[j] & 63; mn = s[4+j] & 63; }
    else { sc = (s[4+j] & 0x0F) | ((s[j-4] >> 6) << 4);
           mn = (s[4+j] >> 4)  | ((s[j]   >> 6) << 4); }
}
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
    const int N = argc>1?atoi(argv[1]):4096;
    const int K = argc>2?atoi(argv[2]):512;
    const int T = argc>3?atoi(argv[3]):512;
    const int MMQX = argc>4?atoi(argv[4]):128;
    if (K%256){ printf("K must be mult 256\n"); return 1; }
    srand(1234);
    const int kblk = K/256;
    std::vector<uint8_t> W((size_t)N*kblk*144);
    for (auto& b : W) b = rand() & 0xFF;
    for (size_t blkidx=0; blkidx*144 < W.size(); blkidx++){
        uint8_t* p = &W[blkidx*144];
        __half d    = __float2half(0.02f + 0.06f*(rand()/(float)RAND_MAX));
        __half dmin = __float2half(0.01f + 0.03f*(rand()/(float)RAND_MAX));
        memcpy(p,   &d,    2);
        memcpy(p+2, &dmin, 2);
    }
    std::vector<__half> Xh((size_t)T*K);
    std::vector<float>  X((size_t)T*K);
    for (size_t i=0;i<X.size();i++){ __half h=__float2half((rand()/(float)RAND_MAX)*2.f-1.f); Xh[i]=h; X[i]=__half2float(h); }

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

    const int Kpad = ((K + 511)/512)*512;
    const int QMMQ = 144;
    const size_t q8n = (size_t)(Kpad/128)*T*QMMQ + 128*QMMQ;
    char *dW; int *dQ; __half *dX; __half *dDstSk; __half *dDstDense; float *dFix;
    CK(cudaMalloc(&dW, W.size()));
    CK(cudaMalloc(&dX, X.size()*2));
    CK(cudaMalloc(&dQ, q8n));
    CK(cudaMalloc(&dDstSk, (size_t)T*N*2));
    CK(cudaMalloc(&dDstDense, (size_t)T*N*2));
    CK(cudaMemcpy(dW, W.data(), W.size(), cudaMemcpyHostToDevice));
    CK(cudaMemcpy(dX, Xh.data(), X.size()*2, cudaMemcpyHostToDevice));
    CK(cudaMemset(dQ, 0, q8n));

    dim3 qg(T, (Kpad + 4*128-1)/(4*128), 1); dim3 qb(128,1,1);
    forge_quantize_mmq_q8_1_ds4<<<qg,qb>>>(dX, dQ, K, K, Kpad, T);
    CK(cudaGetLastError()); CK(cudaDeviceSynchronize());

    auto pad=[&](int v,int a){ return ((v+a-1)/a)*a; };
    int nbs_ids = MMQX*4;
    int nbs_x   = 128*76*4;
    int nbs_y   = pad(MMQX*QMMQ, 8*32*4);
    int smem    = nbs_ids + nbs_x + nbs_y;
    dim3 b(32,8,1);

    // Dense reference.
    dim3 gd((N+127)/128, (T+MMQX-1)/MMQX, 1);
    void* fnd = (MMQX==128)?(void*)forge_mmq_q4k_x128_nc:(void*)forge_mmq_q4k_x64_nc;
    CK(cudaFuncSetAttribute(fnd, cudaFuncAttributeMaxDynamicSharedMemorySize, smem));
    if (MMQX==128) forge_mmq_q4k_x128_nc<<<gd,b,smem>>>(dW,dQ,dDstDense, K,N,T, kblk, T, N);
    else           forge_mmq_q4k_x64_nc <<<gd,b,smem>>>(dW,dQ,dDstDense, K,N,T, kblk, T, N);
    CK(cudaGetLastError()); CK(cudaDeviceSynchronize());

    // Stream-K: grid(nsm,1,1) + fixup pass.
    int nsm=0; CK(cudaDeviceGetAttribute(&nsm, cudaDevAttrMultiProcessorCount, 0));
    CK(cudaMalloc(&dFix, (size_t)nsm*MMQX*128*sizeof(float)));
    dim3 gsk(nsm,1,1);
    void* fnsk = (MMQX==128)?(void*)forge_mmq_sk_q4k_x128_nc:(void*)forge_mmq_sk_q4k_x64_nc;
    CK(cudaFuncSetAttribute(fnsk, cudaFuncAttributeMaxDynamicSharedMemorySize, smem));
    if (MMQX==128){
        forge_mmq_sk_q4k_x128_nc<<<gsk,b,smem>>>(dW,dQ,dDstSk,dFix, K,N,T, kblk, T, N);
        CK(cudaGetLastError());
        forge_mmq_fix_q4k_x128_nc<<<gsk,b>>>(dDstSk,dFix, K,N,T, N);
    } else {
        forge_mmq_sk_q4k_x64_nc<<<gsk,b,smem>>>(dW,dQ,dDstSk,dFix, K,N,T, kblk, T, N);
        CK(cudaGetLastError());
        forge_mmq_fix_q4k_x64_nc<<<gsk,b>>>(dDstSk,dFix, K,N,T, N);
    }
    CK(cudaGetLastError()); CK(cudaDeviceSynchronize());

    std::vector<__half> sk((size_t)T*N), dn((size_t)T*N);
    CK(cudaMemcpy(sk.data(), dDstSk, sk.size()*2, cudaMemcpyDeviceToHost));
    CK(cudaMemcpy(dn.data(), dDstDense, dn.size()*2, cudaMemcpyDeviceToHost));

    // stream-K vs golden
    double num=0, den=0;
    // stream-K vs dense
    double numd=0, dend=0, maxad=0;
    for (size_t i=0;i<sk.size();i++){
        double s = __half2float(sk[i]);
        double d = __half2float(dn[i]);
        double diff = s-golden[i];
        num += diff*diff; den += (double)golden[i]*golden[i];
        double dd = s-d; numd += dd*dd; dend += d*d;
        double ad = fabs(dd); if (ad>maxad) maxad=ad;
    }
    double rl2 = sqrt(num/den);
    double rl2d = sqrt(numd/dend);
    printf("N=%d K=%d T=%d mmq_x=%d nsm=%d : streamK-vs-golden relL2=%.3e | streamK-vs-dense relL2=%.3e maxabs=%.3e\n",
        N,K,T,MMQX,nsm, rl2, rl2d, maxad);
    bool ok = rl2 < 5e-3 && rl2d < 5e-3;
    printf("%s\n", ok ? "PASS" : "FAIL");
    return ok ? 0 : 1;
}
