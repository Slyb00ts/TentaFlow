// ===== File: hal_test_kernels.cu — minimal kernels exercising the HAL launch/graph paths in tests =====
// Precompiled to hal_test_kernels.ptx (nvcc --ptx -arch=sm_89); the PTX is the
// committed fixture so tests never depend on a local nvcc.

extern "C" __global__ void saxpy(float a, const float* x, float* y, unsigned int n) {
    unsigned int i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i < n) {
        y[i] = a * x[i] + y[i];
    }
}

extern "C" __global__ void scale2(float* x, unsigned int n) {
    unsigned int i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i < n) {
        x[i] *= 2.0f;
    }
}

extern "C" __global__ void add3(float* x, unsigned int n) {
    unsigned int i = blockIdx.x * blockDim.x + threadIdx.x;
    if (i < n) {
        x[i] += 3.0f;
    }
}
