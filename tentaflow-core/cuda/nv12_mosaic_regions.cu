// =============================================================================
// File: cuda/nv12_mosaic_regions.cu — irreversible in-place NV12 mosaic
// =============================================================================
//
// Pixelates rectangular regions of an NV12 frame that lives in device memory,
// in place: every mosaic block of a region is replaced by the rounded integer
// mean of its luma samples and, on the 2x2-subsampled grid, of its U and V
// samples. The information inside a block collapses to one value per channel,
// so the original pixels cannot be recovered from the output.
//
// Geometry is NOT computed here. The Rust wrapper (`vision/gpu_preprocess.rs`,
// `mosaic_regions`) clamps each rect to the frame, snaps it to even luma
// coordinates, picks the block size and numbers the blocks; the host oracle
// `mosaic_nv12_host` consumes the very same region list. Keeping one geometry
// implementation is what makes the GPU/host parity test a check of the
// arithmetic rather than of two copies of the clamping rules.
//
// Two passes, two launches on the caller's stream (stream order separates them):
//   Pass 1 — one CUDA block per mosaic block: integer sums of Y, U, V over the
//            ORIGINAL frame, rounded mean `(sum + n/2) / n` into `means`.
//   Pass 2 — one CUDA block per mosaic block: write the means back.
// Integer addition is associative, so the shared-memory reduction order does
// not affect the result: the output is deterministic and bit-identical to the
// sequential host oracle.
//
// Overlapping regions: every mean is taken from the ORIGINAL frame (pass 1 ends
// before pass 2 starts), and a pixel covered by several regions is written ONLY
// by the highest-index region covering it (pass 2 skips pixels a later region
// owns). One writer per pixel — no write race, no order dependence between CUDA
// blocks. The host oracle reproduces this by computing all means first and then
// writing regions in index order, later ones overwriting earlier ones.
//
// The region table travels as a kernel PARAMETER (64 * 32 B + 8 B, well under
// the 4 KiB parameter limit): no H2D copy, no host buffer that must outlive an
// async transfer, nothing to synchronize before the launcher returns.

#include <cuda_runtime.h>

#define MOSAIC_MAX_REGIONS 64
#define MOSAIC_THREADS 256

// Mirrors `#[repr(C)] struct MosaicRegion` in gpu_preprocess.rs. All coordinates
// are luma pixels, already clamped and even-snapped: [x0, x1) x [y0, y1).
// `b` is the (even) block side; `nbx * nby` blocks start at global index
// `first_block` in the `means` buffer.
struct MosaicRegion {
    int x0;
    int y0;
    int x1;
    int y1;
    int b;
    int nbx;
    int nby;
    int first_block;
};

// Mirrors `#[repr(C)] struct MosaicRegionTable`.
struct MosaicRegionTable {
    int n;
    int total_blocks;
    MosaicRegion r[MOSAIC_MAX_REGIONS];
};

// Resolves a global mosaic-block index to (region, bx, by). Regions are laid out
// back to back by `first_block`, so the owning region is the last one whose
// first block does not exceed the index.
__device__ __forceinline__ int mosaic_find_region(const MosaicRegionTable& t, int gb) {
    int ri = 0;
    for (int i = 1; i < t.n; ++i) {
        if (t.r[i].first_block <= gb) ri = i;
    }
    return ri;
}

// Luma and chroma extents of block (bx, by) of region `g`. Chroma coordinates are
// the luma ones halved; x0 and b are even, so the chroma block covers exactly the
// chroma samples of its luma block. `(x1 + 1) / 2` keeps an odd frame edge's last
// chroma column inside the region.
__device__ __forceinline__ void mosaic_block_extent(
    const MosaicRegion& g, int bx, int by,
    int* lx0, int* ly0, int* lx1, int* ly1,
    int* cx0, int* cy0, int* cx1, int* cy1)
{
    *lx0 = g.x0 + bx * g.b;
    *ly0 = g.y0 + by * g.b;
    *lx1 = min(*lx0 + g.b, g.x1);
    *ly1 = min(*ly0 + g.b, g.y1);
    int hb = g.b / 2;
    *cx0 = g.x0 / 2 + bx * hb;
    *cy0 = g.y0 / 2 + by * hb;
    *cx1 = min(*cx0 + hb, (g.x1 + 1) / 2);
    *cy1 = min(*cy0 + hb, (g.y1 + 1) / 2);
}

extern "C" __global__ void nv12_mosaic_means_kernel(
    const unsigned char* y_plane, int y_pitch,
    const unsigned char* uv_plane, int uv_pitch,
    MosaicRegionTable t,
    unsigned char* means)
{
    int gb = blockIdx.x;
    int ri = mosaic_find_region(t, gb);
    const MosaicRegion& g = t.r[ri];
    int local = gb - g.first_block;
    int bx = local % g.nbx;
    int by = local / g.nbx;

    int lx0, ly0, lx1, ly1, cx0, cy0, cx1, cy1;
    mosaic_block_extent(g, bx, by, &lx0, &ly0, &lx1, &ly1, &cx0, &cy0, &cx1, &cy1);

    int lw = lx1 - lx0;
    int ln = lw * (ly1 - ly0);
    int cw = cx1 - cx0;
    int cn = cw * (cy1 - cy0);

    unsigned int sy = 0, su = 0, sv = 0;
    for (int i = threadIdx.x; i < ln; i += blockDim.x) {
        int px = lx0 + i % lw;
        int py = ly0 + i / lw;
        sy += y_plane[(long)py * y_pitch + px];
    }
    for (int i = threadIdx.x; i < cn; i += blockDim.x) {
        int px = cx0 + i % cw;
        int py = cy0 + i / cw;
        const unsigned char* p = uv_plane + (long)py * uv_pitch + 2 * px;
        su += p[0];
        sv += p[1];
    }

    __shared__ unsigned int acc[3];
    if (threadIdx.x == 0) {
        acc[0] = 0;
        acc[1] = 0;
        acc[2] = 0;
    }
    __syncthreads();
    // Integer atomics: any accumulation order yields the same sum.
    atomicAdd(&acc[0], sy);
    atomicAdd(&acc[1], su);
    atomicAdd(&acc[2], sv);
    __syncthreads();
    if (threadIdx.x == 0) {
        unsigned int n_l = (unsigned int)ln;
        unsigned int n_c = (unsigned int)cn;
        means[3 * gb + 0] = (unsigned char)((acc[0] + n_l / 2) / n_l);
        means[3 * gb + 1] = (unsigned char)((acc[1] + n_c / 2) / n_c);
        means[3 * gb + 2] = (unsigned char)((acc[2] + n_c / 2) / n_c);
    }
}

// True when a region with index > `ri` covers luma pixel (px, py) — that later
// region is the pixel's only writer.
__device__ __forceinline__ bool mosaic_luma_owned_later(
    const MosaicRegionTable& t, int ri, int px, int py)
{
    for (int j = ri + 1; j < t.n; ++j) {
        const MosaicRegion& o = t.r[j];
        if (px >= o.x0 && px < o.x1 && py >= o.y0 && py < o.y1) return true;
    }
    return false;
}

// Chroma counterpart of `mosaic_luma_owned_later`, in chroma coordinates.
__device__ __forceinline__ bool mosaic_chroma_owned_later(
    const MosaicRegionTable& t, int ri, int cx, int cy)
{
    for (int j = ri + 1; j < t.n; ++j) {
        const MosaicRegion& o = t.r[j];
        if (cx >= o.x0 / 2 && cx < (o.x1 + 1) / 2 && cy >= o.y0 / 2 && cy < (o.y1 + 1) / 2) {
            return true;
        }
    }
    return false;
}

extern "C" __global__ void nv12_mosaic_write_kernel(
    unsigned char* y_plane, int y_pitch,
    unsigned char* uv_plane, int uv_pitch,
    MosaicRegionTable t,
    const unsigned char* means)
{
    int gb = blockIdx.x;
    int ri = mosaic_find_region(t, gb);
    const MosaicRegion& g = t.r[ri];
    int local = gb - g.first_block;
    int bx = local % g.nbx;
    int by = local / g.nbx;

    int lx0, ly0, lx1, ly1, cx0, cy0, cx1, cy1;
    mosaic_block_extent(g, bx, by, &lx0, &ly0, &lx1, &ly1, &cx0, &cy0, &cx1, &cy1);

    unsigned char my = means[3 * gb + 0];
    unsigned char mu = means[3 * gb + 1];
    unsigned char mv = means[3 * gb + 2];

    int lw = lx1 - lx0;
    int ln = lw * (ly1 - ly0);
    for (int i = threadIdx.x; i < ln; i += blockDim.x) {
        int px = lx0 + i % lw;
        int py = ly0 + i / lw;
        if (mosaic_luma_owned_later(t, ri, px, py)) continue;
        y_plane[(long)py * y_pitch + px] = my;
    }
    int cw = cx1 - cx0;
    int cn = cw * (cy1 - cy0);
    for (int i = threadIdx.x; i < cn; i += blockDim.x) {
        int px = cx0 + i % cw;
        int py = cy0 + i / cw;
        if (mosaic_chroma_owned_later(t, ri, px, py)) continue;
        unsigned char* p = uv_plane + (long)py * uv_pitch + 2 * px;
        p[0] = mu;
        p[1] = mv;
    }
}

// Plain-C launcher (the Rust FFI never touches <<<>>>). `table` is a HOST struct
// copied into the launch parameters of both kernels before this returns. `means`
// is device scratch of at least `3 * table->total_blocks` bytes, reused only in
// stream order. Returns the first CUDA launch error (0 == cudaSuccess).
extern "C" int launch_nv12_mosaic_regions(
    unsigned char* y_plane,
    int y_pitch,
    unsigned char* uv_plane,
    int uv_pitch,
    const MosaicRegionTable* table,
    unsigned char* means,
    cudaStream_t stream)
{
    if (table->n <= 0 || table->total_blocks <= 0) return 0;
    nv12_mosaic_means_kernel<<<(unsigned int)table->total_blocks, MOSAIC_THREADS, 0, stream>>>(
        y_plane, y_pitch, uv_plane, uv_pitch, *table, means);
    int rc = (int)cudaGetLastError();
    if (rc != 0) return rc;
    nv12_mosaic_write_kernel<<<(unsigned int)table->total_blocks, MOSAIC_THREADS, 0, stream>>>(
        y_plane, y_pitch, uv_plane, uv_pitch, *table, means);
    return (int)cudaGetLastError();
}
