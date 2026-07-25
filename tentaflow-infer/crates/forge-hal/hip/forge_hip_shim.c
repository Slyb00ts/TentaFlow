// =============================================================================
// Plik: forge_hip_shim.c
// Opis: Płaski widok właściwości urządzenia HIP dla backendu forge-hal. Shim
//       kompiluje się nagłówkami ROCm, więc układ `hipDeviceProp_t` i numeracja
//       enumów są rozstrzygane przez kompilator, a nie zgadywane w Ruście.
// =============================================================================
#include <hip/hip_runtime_api.h>
#include <stdio.h>
#include <string.h>

typedef struct {
    char name[256];
    char arch[64];
    unsigned long long total_mem;
    int warp_size;
    int cu_count;
    int max_threads_per_block;
    int max_shared_mem_per_block;
} ForgeHipProps;

int forge_hip_props(int device, ForgeHipProps *out) {
    hipDeviceProp_t props;
    hipError_t status = hipGetDeviceProperties(&props, device);
    if (status != hipSuccess) {
        return (int)status;
    }
    memset(out, 0, sizeof(*out));
    snprintf(out->name, sizeof(out->name), "%s", props.name);
    snprintf(out->arch, sizeof(out->arch), "%s", props.gcnArchName);
    out->total_mem = (unsigned long long)props.totalGlobalMem;
    out->warp_size = props.warpSize;
    out->cu_count = props.multiProcessorCount;
    out->max_threads_per_block = props.maxThreadsPerBlock;
    out->max_shared_mem_per_block = (int)props.sharedMemPerBlock;
    return (int)hipSuccess;
}
