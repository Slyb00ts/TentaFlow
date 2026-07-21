// Isolated in-engine GEMM TOPS: vendored llama.cpp Q4_K MMQ (mmq_q4k.cu) vs the
// committed hand int8-MMQ kernel (gemm_i8mma.cu). Times each kernel alone (values
// irrelevant to throughput; buffers are correctly SIZED). TOPS = 2*rows*cols*tokens/t.
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <vector>
#include <cuda_fp16.h>
#include <cuda_runtime.h>

// vendored MMQ
extern "C" __global__ void forge_mmq_q4k_x128_nc(const char*,const int*,float*,int,int,int,int,int,int);
extern "C" __global__ void forge_quantize_mmq_q8_1_ds4(const __half*,void*,int64_t,int64_t,int64_t,int);
// committed hand kernel
extern "C" __global__ void forge_gemm_q4_k_i8mma_cuda(__half*,const uint8_t*,const int8_t*,const float*,const float*,int,int,int);

#define CK(x) do{cudaError_t e=(x); if(e){printf("err %s:%d %s\n",__FILE__,__LINE__,cudaGetErrorString(e));exit(1);} }while(0)

static double time_kernel(void(*launch)(void*,cudaStream_t),void* ctx,int iters){
    cudaStream_t s; CK(cudaStreamCreate(&s));
    for(int i=0;i<5;i++) launch(ctx,s);              // warmup (steady clocks)
    CK(cudaStreamSynchronize(s));
    cudaEvent_t a,b; CK(cudaEventCreate(&a)); CK(cudaEventCreate(&b));
    CK(cudaEventRecord(a,s));
    for(int i=0;i<iters;i++) launch(ctx,s);
    CK(cudaEventRecord(b,s)); CK(cudaEventSynchronize(b));
    float ms=0; CK(cudaEventElapsedTime(&ms,a,b));
    CK(cudaStreamDestroy(s));
    return ms/iters/1e3; // seconds/iter
}

struct MmqCtx{ const char*W; const int*Q; float*D; int K,N,T,smem; };
static void launch_mmq(void* p,cudaStream_t s){
    MmqCtx*c=(MmqCtx*)p; dim3 g((c->N+127)/128,(c->T+127)/128,1); dim3 b(32,8,1);
    forge_mmq_q4k_x128_nc<<<g,b,c->smem,s>>>(c->W,c->Q,c->D,c->K,c->N,c->T,c->K/256,c->T,c->N);
}
struct HandCtx{ __half*Y; const uint8_t*W; const int8_t*XQ; const float*XD; const float*XSM; int K,N,T; };
static void launch_hand(void* p,cudaStream_t s){
    HandCtx*c=(HandCtx*)p; dim3 g((c->N+127)/128,(c->T+127)/128,1); dim3 b(256,1,1);
    forge_gemm_q4_k_i8mma_cuda<<<g,b,0,s>>>(c->Y,c->W,c->XQ,c->XD,c->XSM,c->K,c->N,c->T);
}

int main(int argc,char**argv){
    int N=argc>1?atoi(argv[1]):4096, K=argc>2?atoi(argv[2]):14336, T=argc>3?atoi(argv[3]):2048;
    int kblk=K/256, Kpad=((K+511)/512)*512;
    // buffers
    char*W; int*Q; float*D; CK(cudaMalloc(&W,(size_t)N*kblk*144)); CK(cudaMemset(W,1,(size_t)N*kblk*144));
    size_t q8n=(size_t)(Kpad/128)*T*144+128*144; CK(cudaMalloc(&Q,q8n)); CK(cudaMemset(Q,1,q8n));
    CK(cudaMalloc(&D,(size_t)T*N*4));
    int smem = 128*4 + 128*76*4 + (((128*144)+ (8*32*4)-1)/(8*32*4))*(8*32*4);
    CK(cudaFuncSetAttribute(forge_mmq_q4k_x128_nc,cudaFuncAttributeMaxDynamicSharedMemorySize,smem));
    // hand-kernel buffers
    __half*Y; int8_t*XQ; float*XD,*XSM;
    CK(cudaMalloc(&Y,(size_t)T*N*2)); CK(cudaMalloc(&XQ,(size_t)T*K)); CK(cudaMemset(XQ,1,(size_t)T*K));
    CK(cudaMalloc(&XD,(size_t)T*(K/32)*4)); CK(cudaMalloc(&XSM,(size_t)T*(K/32)*4));
    CK(cudaMemset(XD,0,(size_t)T*(K/32)*4)); CK(cudaMemset(XSM,0,(size_t)T*(K/32)*4));

    MmqCtx mc{W,Q,D,K,N,T,smem}; HandCtx hc{Y,(const uint8_t*)W,XQ,XD,XSM,K,N,T};
    int iters=50;
    double t_mmq=time_kernel(launch_mmq,&mc,iters);
    double t_hand=time_kernel(launch_hand,&hc,iters);
    double work=2.0*N*K*T;
    printf("N=%d K=%d T=%d : MMQ %.1f us %.1f TOPS | HAND %.1f us %.1f TOPS | speedup %.2fx\n",
        N,K,T, t_mmq*1e6, work/t_mmq/1e12, t_hand*1e6, work/t_hand/1e12, t_hand/t_mmq);
    return 0;
}
