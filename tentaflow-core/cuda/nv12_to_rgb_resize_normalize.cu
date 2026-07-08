// =============================================================================
// File: cuda/nv12_to_rgb_resize_normalize.cu — fused NV12->RGB + resize + normalize
// =============================================================================
//
// One fused CUDA kernel that turns a batch of NV12 (4:2:0) frames into a single
// contiguous f32 NCHW buffer [n,3,S,S], ready to hand to ONNX Runtime as a
// device-memory input tensor (zero H2D copy on the inference side). Per output
// pixel it performs, in order:
//   (a) YUV->RGB (u8) at the four source corners the bilinear resize needs,
//   (b) the SAME separable Q8 bilinear resize as crop_resize_normalize.cu
//       (shared `axis_plan` / integer blend, so the resize stage is bit-for-bit
//       identical to `vision::resize::resize_rgb`),
//   (c) /255 + per-channel (v-mean)/std normalize, written HWC->CHW.
//
// Parity notes:
//   * The resize is bit-identical to the CPU `resize_rgb`: same half-pixel map
//     `src = (dst+0.5)*scale-0.5`, floor->left, round(frac*256)->Q8 right weight,
//     u8 horizontal intermediate then u8 vertical blend, `+128 >> 8` rounding.
//     Compiled with --fmad=false so `(d+0.5)*scale-0.5` is NOT FMA-contracted
//     (which could flip a boundary Q8 weight). Because the full-frame NV12->RGB
//     conversion is deterministic per source pixel, converting ONLY the four
//     needed corners on the fly yields exactly the u8 RGB the CPU path would have
//     materialized in a full WxH RGB buffer before resizing.
//   * The YUV->RGB stage is NOT integer/bit-exact across CPU vs GPU float units:
//     it runs in f32 and the caller-side CPU reference matches the same formula,
//     but residual float rounding (a few LSB in u8 RGB) is expected and is why
//     the Rust parity test uses a small tolerance rather than exact equality.
//
// Color handling is parameterized (kr, kb luma coefficients + full/limited
// range) so the caller sets BT.601 vs BT.709 and range from the GStreamer caps
// colorimetry in Stage 1. Default (see Rust `ColorCoeffs::bt709_limited`) is
// BT.709 limited-range, the usual decode for H.264.
//
// Chroma is upsampled by nearest 2x2 replication (chroma sample (sx>>1, sy>>1)
// shared by its luma 2x2 block) — the standard simple NV12->RGB siting. Proper
// cited-chroma interpolation is deliberately deferred to Stage 1 (documented as a
// parity risk in the Rust test).

#include <cuda_runtime.h>
#include <math.h>

#define WEIGHT_SHIFT 8
#define WEIGHT_ONE 256

__device__ __forceinline__ long nv12_clamp_l(long v, long lo, long hi) {
    if (v < lo) return lo;
    if (v > hi) return hi;
    return v;
}

// Half-pixel bilinear plan for one output coordinate `d` mapping src_len->dst_len.
// Identical math to `AxisPlan::bilinear` on the CPU and to crop_resize_normalize.cu.
__device__ __forceinline__ void nv12_axis_plan(
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
    *left = (int)nv12_clamp_l(li, 0, max_idx);
    *right = (int)nv12_clamp_l(li + 1, 0, max_idx);
    int wr = (int)round(frac * (double)WEIGHT_ONE);
    if (wr < 0) wr = 0;
    if (wr > WEIGHT_ONE) wr = WEIGHT_ONE;
    *w_right = wr;
}

// YUV->RGB (u8) for one source pixel from raw NV12 sample values (Y, U, V in
// 0..255). Uses luma coefficients (kr, kb) and range so BT.601/BT.709 +
// limited/full are all covered by one formula. The Rust-side CPU reference in
// the parity test replicates this exact f32 formula.
__device__ __forceinline__ void nv12_yuv_to_rgb_u8(
    int yv, int uv_u, int uv_v,
    float kr, float kb, int full_range,
    unsigned char* r_out, unsigned char* g_out, unsigned char* b_out)
{
    float kg = 1.0f - kr - kb;
    float y, cb, cr;
    if (full_range) {
        y = (float)yv / 255.0f;
        cb = ((float)uv_u - 128.0f) / 255.0f;
        cr = ((float)uv_v - 128.0f) / 255.0f;
    } else {
        y = ((float)yv - 16.0f) / 219.0f;
        cb = ((float)uv_u - 128.0f) / 224.0f;
        cr = ((float)uv_v - 128.0f) / 224.0f;
    }
    float r = y + 2.0f * (1.0f - kr) * cr;
    float b = y + 2.0f * (1.0f - kb) * cb;
    float g = y - (2.0f * kr * (1.0f - kr) / kg) * cr - (2.0f * kb * (1.0f - kb) / kg) * cb;
    r = fminf(fmaxf(r, 0.0f), 1.0f);
    g = fminf(fmaxf(g, 0.0f), 1.0f);
    b = fminf(fmaxf(b, 0.0f), 1.0f);
    *r_out = (unsigned char)(int)(r * 255.0f + 0.5f);
    *g_out = (unsigned char)(int)(g * 255.0f + 0.5f);
    *b_out = (unsigned char)(int)(b * 255.0f + 0.5f);
}

// Samples the RGB (u8) of one source pixel (sx, sy) of an NV12 frame. Chroma is
// the nearest 2x2-shared sample (sx>>1, sy>>1) from the interleaved UV plane.
__device__ __forceinline__ void nv12_sample_rgb(
    const unsigned char* yp, int y_stride,
    const unsigned char* uvp, int uv_stride,
    int sx, int sy,
    float kr, float kb, int full_range,
    unsigned char* r, unsigned char* g, unsigned char* b)
{
    int yv = (int)yp[(long)sy * y_stride + sx];
    int cx = sx >> 1;
    int cy = sy >> 1;
    const unsigned char* uv_row = uvp + (long)cy * uv_stride;
    int uu = (int)uv_row[cx * 2 + 0];
    int vv = (int)uv_row[cx * 2 + 1];
    nv12_yuv_to_rgb_u8(yv, uu, vv, kr, kb, full_range, r, g, b);
}

// One thread per (frame, y, x). Output plane layout per frame is row-major NCHW:
// out[frame*3*S*S + c*S*S + y*S + x].
extern "C" __global__ void nv12_to_rgb_resize_normalize_kernel(
    const unsigned char* const* y_ptrs,
    const int* y_strides,
    const unsigned char* const* uv_ptrs,
    const int* uv_strides,
    const int* widths,
    const int* heights,
    int n,
    int s,
    const float* mean,
    const float* stdv,
    float kr,
    float kb,
    int full_range,
    float* out)
{
    long total = (long)n * (long)s * (long)s;
    long idx = (long)blockIdx.x * (long)blockDim.x + (long)threadIdx.x;
    if (idx >= total) return;

    long per = (long)s * (long)s;
    int frame_i = (int)(idx / per);
    long rem = idx % per;
    int y = (int)(rem / (long)s);
    int x = (int)(rem % (long)s);

    const unsigned char* yp = y_ptrs[frame_i];
    const unsigned char* uvp = uv_ptrs[frame_i];
    int y_stride = y_strides[frame_i];
    int uv_stride = uv_strides[frame_i];
    int w = widths[frame_i];
    int h = heights[frame_i];

    int left, right, wr;
    nv12_axis_plan(x, w, s, &left, &right, &wr);
    int wl = WEIGHT_ONE - wr;

    int top, bot, wb;
    nv12_axis_plan(y, h, s, &top, &bot, &wb);
    int wt = WEIGHT_ONE - wb;

    // Convert the four source corners to u8 RGB, then blend with the SAME Q8
    // integer math as resize_rgb (horizontal u8 intermediate, then vertical).
    unsigned char tl[3], tr[3], bl[3], br[3];
    nv12_sample_rgb(yp, y_stride, uvp, uv_stride, left, top, kr, kb, full_range, &tl[0], &tl[1], &tl[2]);
    nv12_sample_rgb(yp, y_stride, uvp, uv_stride, right, top, kr, kb, full_range, &tr[0], &tr[1], &tr[2]);
    nv12_sample_rgb(yp, y_stride, uvp, uv_stride, left, bot, kr, kb, full_range, &bl[0], &bl[1], &bl[2]);
    nv12_sample_rgb(yp, y_stride, uvp, uv_stride, right, bot, kr, kb, full_range, &br[0], &br[1], &br[2]);

    #pragma unroll
    for (int c = 0; c < 3; ++c) {
        int h_top = ((int)tl[c] * wl + (int)tr[c] * wr + (WEIGHT_ONE / 2)) >> WEIGHT_SHIFT;
        int h_bot = ((int)bl[c] * wl + (int)br[c] * wr + (WEIGHT_ONE / 2)) >> WEIGHT_SHIFT;
        int v = (h_top * wt + h_bot * wb + (WEIGHT_ONE / 2)) >> WEIGHT_SHIFT;
        float fv = (float)v / 255.0f;
        float outv = (fv - mean[c]) / stdv[c];
        out[(long)frame_i * 3 * per + (long)c * per + (long)y * (long)s + (long)x] = outv;
    }
}

// Plain-C launcher so the Rust FFI never touches the <<<>>> syntax. Launched on
// the caller-provided stream so concurrent callers (one stream per worker
// thread) do not serialize on the default stream. Returns the CUDA error code
// from the launch (0 == cudaSuccess).
extern "C" int launch_nv12_to_rgb_resize_normalize(
    const unsigned char* const* y_ptrs,
    const int* y_strides,
    const unsigned char* const* uv_ptrs,
    const int* uv_strides,
    const int* widths,
    const int* heights,
    int n,
    int s,
    const float* mean,
    const float* stdv,
    float kr,
    float kb,
    int full_range,
    float* out,
    cudaStream_t stream)
{
    long total = (long)n * (long)s * (long)s;
    int block = 256;
    long grid = (total + block - 1) / block;
    nv12_to_rgb_resize_normalize_kernel<<<(unsigned int)grid, (unsigned int)block, 0, stream>>>(
        y_ptrs, y_strides, uv_ptrs, uv_strides, widths, heights, n, s, mean, stdv,
        kr, kb, full_range, out);
    return (int)cudaGetLastError();
}
