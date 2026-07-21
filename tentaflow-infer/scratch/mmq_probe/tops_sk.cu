// Isolated in-engine GEMM TOPS: dense conventional-tiling MMQ vs stream-K MMQ
// (mmq_q4k.cu). Times each kernel path alone (values irrelevant; buffers sized).
// Stream-K path = mul_mat_q stream-K driver (grid = nsm) + fixup reduction.
// TOPS = 2*rows*cols*tokens/t.
#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <cuda_fp16.h>
#include <cuda_runtime.h>

extern "C" __global__ void forge_mmq_q4k_x128_nc(const char*,const int*,__half*,int,int,int,int,int,int);
extern "C" __global__ void forge_mmq_sk_q4k_x128_nc(const char*,const int*,__half*,float*,int,int,int,int,int,int);
extern "C" __global__ void forge_mmq_fix_q4k_x128_nc(__half*,const float*,int,int,int,int);

#define CK(x) do{cudaError_t e=(x); if(e){printf("err %s:%d %s\n",__FILE__,__LINE__,cudaGetErrorString(e));exit(1);} }while(0)

static double time_launch(void(*launch)(void*,cudaStream_t),void* ctx,int iters){
    cudaStream_t s; CK(cudaStreamCreate(&s));
    for(int i=0;i<5;i++) launch(ctx,s);
    CK(cudaStreamSynchronize(s));
    cudaEvent_t a,b; CK(cudaEventCreate(&a)); CK(cudaEventCreate(&b));
    CK(cudaEventRecord(a,s));
    for(int i=0;i<iters;i++) launch(ctx,s);
    CK(cudaEventRecord(b,s)); CK(cudaEventSynchronize(b));
    float ms=0; CK(cudaEventElapsedTime(&ms,a,b));
    CK(cudaStreamDestroy(s));
    return ms/iters/1e3;
}

struct Ctx{ const char*W; const int*Q; __half*D; float*F; int K,N,T,smem,nsm; };
static void launch_dense(void* p,cudaStream_t s){
    Ctx*c=(Ctx*)p; dim3 g((c->N+127)/128,(c->T+127)/128,1); dim3 b(32,8,1);
    forge_mmq_q4k_x128_nc<<<g,b,c->smem,s>>>(c->W,c->Q,c->D,c->K,c->N,c->T,c->K/256,c->T,c->N);
}
static void launch_sk(void* p,cudaStream_t s){
    Ctx*c=(Ctx*)p; dim3 g(c->nsm,1,1); dim3 b(32,8,1);
    forge_mmq_sk_q4k_x128_nc<<<g,b,c->smem,s>>>(c->W,c->Q,c->D,c->F,c->K,c->N,c->T,c->K/256,c->T,c->N);
    forge_mmq_fix_q4k_x128_nc<<<g,b,0,s>>>(c->D,c->F,c->K,c->N,c->T,c->N);
}

int main(int argc,char**argv){
    int N=argc>1?atoi(argv[1]):14336, K=argc>2?atoi(argv[2]):4096, T=argc>3?atoi(argv[3]):4096;
    int kblk=K/256, Kpad=((K+511)/512)*512;
    char*W; int*Q; __half*D; CK(cudaMalloc(&W,(size_t)N*kblk*144)); CK(cudaMemset(W,1,(size_t)N*kblk*144));
    size_t q8n=(size_t)(Kpad/128)*T*144+128*144; CK(cudaMalloc(&Q,q8n)); CK(cudaMemset(Q,1,q8n));
    CK(cudaMalloc(&D,(size_t)T*N*2));
    int nsm=0; CK(cudaDeviceGetAttribute(&nsm,cudaDevAttrMultiProcessorCount,0));
    float*F; CK(cudaMalloc(&F,(size_t)nsm*128*128*sizeof(float)));
    int smem = 128*4 + 128*76*4 + (((128*144)+ (8*32*4)-1)/(8*32*4))*(8*32*4);
    CK(cudaFuncSetAttribute(forge_mmq_q4k_x128_nc,cudaFuncAttributeMaxDynamicSharedMemorySize,smem));
    CK(cudaFuncSetAttribute(forge_mmq_sk_q4k_x128_nc,cudaFuncAttributeMaxDynamicSharedMemorySize,smem));

    Ctx c{W,Q,D,F,K,N,T,smem,nsm};
    int iters=50;
    double t_d=time_launch(launch_dense,&c,iters);
    double t_s=time_launch(launch_sk,&c,iters);
    double work=2.0*N*K*T;
    printf("N=%d K=%d T=%d nsm=%d : DENSE %.1f us %.1f TOPS | STREAMK %.1f us %.1f TOPS | speedup %.3fx\n",
        N,K,T,nsm, t_d*1e6, work/t_d/1e12, t_s*1e6, work/t_s/1e12, t_d/t_s);
    return 0;
}
