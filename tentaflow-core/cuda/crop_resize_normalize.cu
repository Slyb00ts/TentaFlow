// =============================================================================
// File: cuda/crop_resize_normalize.cu — fused GPU crop resize + normalize
// =============================================================================
//
// One fused CUDA kernel that turns a batch of raw RGB24 u8 crops (each a
// different cw x ch) into a single contiguous f32 NCHW buffer [n,3,S,S], ready
// to hand to ONNX Runtime as a device-memory input tensor (zero H2D copy on the
// inference side). Per output pixel it reproduces, BIT-FOR-BIT, the CPU path in
// `vision::resize::resize_rgb` (separable Q8 bilinear with a u8 intermediate)
// followed by `classifier_stan`'s /255 + per-channel (v-mean)/std normalize.
//
// Parity notes (why this matches the CPU exactly):
//   * Sampling geometry is the half-pixel map `src = (dst+0.5)*scale - 0.5`,
//     clamped at 0, floor -> left index, round(frac*256) -> Q8 right weight,
//     both indices clamped to [0, len-1] — identical to `AxisPlan::bilinear`.
//   * The horizontal pass is recomputed on the fly for exactly the two source
//     rows the vertical pass needs; each (row,dst_col) horizontal result is
//     deterministic and rounded to u8, so on-the-fly recomputation yields the
//     same u8 intermediate the CPU stores in its `tmp` buffer, then the same
//     vertical blend.
//   * Weight math runs in f64 and the normalize in f32, matching the CPU types.
//     Compiled with --fmad=false so `(d+0.5)*scale-0.5` is NOT contracted into
//     an FMA (which rounds differently and could flip a boundary Q8 weight).

#include <cuda_runtime.h>
#include <math.h>

#define WEIGHT_SHIFT 8
#define WEIGHT_ONE 256

__device__ __forceinline__ long clamp_l(long v, long lo, long hi) {
    if (v < lo) return lo;
    if (v > hi) return hi;
    return v;
}

// Half-pixel bilinear plan for one output coordinate `d` mapping src_len->dst_len.
// Fills left/right source indices and the Q8 right-neighbour weight, exactly like
// `AxisPlan::bilinear` on the CPU.
__device__ __forceinline__ void axis_plan(
    int d, int src_len, int dst_len,
    int* left, int* right, int* w_right)
{
    double scale = (double)src_len / (double)dst_len;
    double src_pos = ((double)d + 0.5) * scale - 0.5;
    if (src_pos < 0.0) src_pos = 0.0;
    double lf = floor(src_pos);
    double frac = src_pos - lf;
    long li = (long)lf;
    long max_idx = (long)src_len - 1;
    *left = (int)clamp_l(li, 0, max_idx);
    *right = (int)clamp_l(li + 1, 0, max_idx);
    int wr = (int)round(frac * (double)WEIGHT_ONE);
    if (wr < 0) wr = 0;
    if (wr > WEIGHT_ONE) wr = WEIGHT_ONE;
    *w_right = wr;
}

// One thread per (crop, y, x); all three channels handled inside so the axis
// plans are computed once per output pixel. Output plane layout per crop is
// row-major NCHW: out[crop*3*S*S + c*S*S + y*S + x].
extern "C" __global__ void crop_resize_normalize_kernel(
    const unsigned char* const* crop_ptrs,
    const int* crop_ws,
    const int* crop_hs,
    int n,
    int s,
    const float* mean,
    const float* stdv,
    float* out)
{
    long total = (long)n * (long)s * (long)s;
    long idx = (long)blockIdx.x * (long)blockDim.x + (long)threadIdx.x;
    if (idx >= total) return;

    long per = (long)s * (long)s;
    int crop_i = (int)(idx / per);
    long rem = idx % per;
    int y = (int)(rem / (long)s);
    int x = (int)(rem % (long)s);

    const unsigned char* src = crop_ptrs[crop_i];
    int cw = crop_ws[crop_i];
    int ch = crop_hs[crop_i];

    int left, right, wr;
    axis_plan(x, cw, s, &left, &right, &wr);
    int wl = WEIGHT_ONE - wr;

    int top, bot, wb;
    axis_plan(y, ch, s, &top, &bot, &wb);
    int wt = WEIGHT_ONE - wb;

    int src_stride = cw * 3;
    #pragma unroll
    for (int c = 0; c < 3; ++c) {
        int t_l = (int)src[top * src_stride + left * 3 + c];
        int t_r = (int)src[top * src_stride + right * 3 + c];
        int b_l = (int)src[bot * src_stride + left * 3 + c];
        int b_r = (int)src[bot * src_stride + right * 3 + c];
        // Horizontal pass on the two needed source rows (u8 intermediate).
        int h_top = (t_l * wl + t_r * wr + (WEIGHT_ONE / 2)) >> WEIGHT_SHIFT;
        int h_bot = (b_l * wl + b_r * wr + (WEIGHT_ONE / 2)) >> WEIGHT_SHIFT;
        // Vertical pass -> u8 value in [0,255].
        int v = (h_top * wt + h_bot * wb + (WEIGHT_ONE / 2)) >> WEIGHT_SHIFT;
        float fv = (float)v / 255.0f;
        float outv = (fv - mean[c]) / stdv[c];
        out[(long)crop_i * 3 * per + (long)c * per + (long)y * (long)s + (long)x] = outv;
    }
}

// Plain-C launcher so the Rust FFI never has to touch the <<<>>> launch syntax.
// The kernel is launched on the caller-provided stream so concurrent callers
// (one stream per worker thread) do not serialize on the default stream.
// Returns the CUDA error code from the launch (0 == cudaSuccess).
extern "C" int launch_crop_resize_normalize(
    const unsigned char* const* crop_ptrs,
    const int* crop_ws,
    const int* crop_hs,
    int n,
    int s,
    const float* mean,
    const float* stdv,
    float* out,
    cudaStream_t stream)
{
    long total = (long)n * (long)s * (long)s;
    int block = 256;
    long grid = (total + block - 1) / block;
    crop_resize_normalize_kernel<<<(unsigned int)grid, (unsigned int)block, 0, stream>>>(
        crop_ptrs, crop_ws, crop_hs, n, s, mean, stdv, out);
    return (int)cudaGetLastError();
}
