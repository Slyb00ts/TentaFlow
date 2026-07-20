# ===== File: gemm_q6k_i8_multistage.mojo — universal Mojo int8 Q6_K prefill GEMM (AOT) =====
# Portable per-16-block NATIVE-LAYOUT Q6_K int8 multistage tensor-core GEMM (int8
# m16n8k32 mma, sm_80+). The CUDA-free replacement for the f16 `gemm_q6_k_impl`
# on the prefill down-proj (and Q6_K attn_v). The pipeline lives in
# `modular_i8/multistage_i8_q6k_native.mojo` (forked from the Q4_K native GEMM).
#
# TRUE 1× VRAM: the kernel reads the RAW GGUF `block_q6_K` superblock bytes
# in-kernel — the SAME `DevWeight::Q6K.buf` bytes decode reads. Q6_K's 16-element
# scale granularity is honored bit-exactly with a double m16n8k32 mma per 32-region
# (full + upper-16-k-zeroed), so quality == the CPU Q6_K × q8_1 golden by
# construction. Activation q8_1 quant is shared with the Q4_K native path
# (`quantize_act_q8_1`, f32 da; sa unused — Q6_K has no min term). One GEMM PTX per
# (N,K,MPAD); the launcher picks the smallest MPAD bucket ≥ token count T and
# guards real rows in the epilogue.

from layout import TileTensor, Idx, Coord, row_major
from linalg.utils_gpu import MatmulConfig
from std.utils.index import Index
from std.gpu import block_idx, block_dim, thread_idx
from src.modular_i8.multistage_i8_q6k_native import (
    multistage_gemm_q6k_native_kernel,
)


comptime Q6K_I8_CFG = MatmulConfig[
    DType.int8, DType.int8, DType.float32, True
](
    block_tile_shape=Index(128, 128, 64),
    warp_tile_shape=Index(64, 32, 64),
    mma_shape=Index(16, 8, 32),
)


def gemm_q6k_i8_native[N: Int, K: Int, MPAD: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Int8, MutAnyOrigin],
    w: UnsafePointer[UInt8, ImmutAnyOrigin],
    da: UnsafePointer[Float32, ImmutAnyOrigin],
    sa: UnsafePointer[Float32, ImmutAnyOrigin],
    m_real: Int,
):
    """Y[T,N] = Q6_K(B) · q8_1(A): NATIVE GGUF Q6_K weight bytes read in-kernel,
    f16 out. `a` is the int8 activation padded to the static token ceiling MPAD
    (`quantize_act_q8_1`, rows T..MPAD zeroed), `da` its per-32-block f32 scale
    [K/32, MPAD] (`sa` accepted for a shared signature but unused — Q6_K folds the
    −32 offset into the weight code, no min term); `w` the RAW `DevWeight::Q6K.buf`
    (210 bytes / 256 weights, NO repack). `m_real` the real token count T. Grid
    (ceil(N/128), MPAD/128); launcher passes dynamic smem + the >48 KB opt-in."""
    var c_dummy = y.bitcast[Float32]()
    var a_nd = TileTensor(a, row_major(Coord(Idx[MPAD], Idx[K])))
    var c_nd = TileTensor(c_dummy, row_major(Coord(Idx[MPAD], Idx[N])))
    multistage_gemm_q6k_native_kernel[
        CLT = c_nd.LayoutType,
        a_type = DType.int8,
        ALT = a_nd.LayoutType,
        c_linear_idx_type = c_nd.linear_idx_type,
        a_linear_idx_type = a_nd.linear_idx_type,
        config = Q6K_I8_CFG,
    ](c_nd, a_nd, w, da, sa, y, m_real)


# Committed (N, K, MPAD) instances for the dense Mistral-7B family: (4096,14336)
# down-proj and (1024,4096) attn_v (the Q6_K weights in Q4_K_M). MPAD buckets
# (128..4096) cover any prefill token count; the launcher picks the smallest
# bucket ≥ T and zero-pads. Unlisted (N,K) or T beyond 4096 fall back to the f16
# Q6_K path.
comptime gemm_q6k_i8_native_4096_14336_m128 = gemm_q6k_i8_native[4096, 14336, 128]
comptime gemm_q6k_i8_native_4096_14336_m256 = gemm_q6k_i8_native[4096, 14336, 256]
comptime gemm_q6k_i8_native_4096_14336_m512 = gemm_q6k_i8_native[4096, 14336, 512]
comptime gemm_q6k_i8_native_4096_14336_m1024 = gemm_q6k_i8_native[4096, 14336, 1024]
comptime gemm_q6k_i8_native_4096_14336_m2048 = gemm_q6k_i8_native[4096, 14336, 2048]
comptime gemm_q6k_i8_native_4096_14336_m4096 = gemm_q6k_i8_native[4096, 14336, 4096]

comptime gemm_q6k_i8_native_1024_4096_m128 = gemm_q6k_i8_native[1024, 4096, 128]
comptime gemm_q6k_i8_native_1024_4096_m256 = gemm_q6k_i8_native[1024, 4096, 256]
comptime gemm_q6k_i8_native_1024_4096_m512 = gemm_q6k_i8_native[1024, 4096, 512]
comptime gemm_q6k_i8_native_1024_4096_m1024 = gemm_q6k_i8_native[1024, 4096, 1024]
comptime gemm_q6k_i8_native_1024_4096_m2048 = gemm_q6k_i8_native[1024, 4096, 2048]
comptime gemm_q6k_i8_native_1024_4096_m4096 = gemm_q6k_i8_native[1024, 4096, 4096]
