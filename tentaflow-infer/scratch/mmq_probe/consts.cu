#include "common.cuh"
#include "mmq.cuh"
#include <cstdio>
int main() {
    const int cc = 890; // RTX 4090 Ada
    const int warp_size = 32;
    const int nwarps = mmq_get_nwarps_host(cc, warp_size);
    const int mmq_y = get_mmq_y_host(cc);
    const int mmq_x_max = get_mmq_x_max_host(cc);
    printf("sizeof(block_q8_1_mmq)=%zu\n", sizeof(block_q8_1_mmq));
    printf("sizeof(block_q8_1)=%zu\n", sizeof(block_q8_1));
    printf("MMQ_TILE_Y_K=%d\n", MMQ_TILE_Y_K);
    printf("MMQ_MMA_TILE_X_K_Q8_1=%d\n", (int)mmq_get_mma_tile_x_k(GGML_TYPE_Q4_K));
    printf("mmq_y=%d nwarps=%d mmq_x_max=%d warp_size=%d\n", mmq_y, nwarps, mmq_x_max, warp_size);
    printf("turing_mma_available=%d\n", (int)turing_mma_available(cc));
    printf("ds_layout(Q4_K)=%d (DS4=%d)\n", (int)mmq_get_q8_1_ds_layout(GGML_TYPE_Q4_K), (int)MMQ_Q8_1_DS_LAYOUT_DS4);
    printf("QK8_1=%d QI8_0=%d QI4_K=%d qk(Q4_K)=%d\n", QK8_1, QI8_0, QI4_K, ggml_cuda_type_traits<GGML_TYPE_Q4_K>::qk);
    for (int mmq_x = 8; mmq_x <= mmq_x_max; mmq_x += 8) {
        const int gran = mmq_get_granularity_host(mmq_x, cc);
        const size_t nbs = mmq_get_nbytes_shared<GGML_TYPE_Q4_K>(mmq_x, mmq_y, cc, warp_size, nwarps);
        printf("mmq_x=%3d gran=%d nbytes_shared=%zu\n", mmq_x, gran, nbs);
    }
    return 0;
}
