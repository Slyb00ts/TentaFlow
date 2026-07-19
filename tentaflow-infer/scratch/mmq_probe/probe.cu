#include "common.cuh"
#include "mmq.cuh"
#include "quantize.cuh"

// Try to force instantiation of the Q4_K MMA kernel and the quantize kernel.
template __global__ void mul_mat_q<GGML_TYPE_Q4_K, 64, false>(
    const char*, const int*, const int32_t*, const int32_t*, float*, float*,
    int,int,int,int,int,int,int,int,int,int,int,int,int,int,int,int,int);
