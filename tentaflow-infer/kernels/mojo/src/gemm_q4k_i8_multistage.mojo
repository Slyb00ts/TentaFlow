# ===== File: gemm_q4k_i8_multistage.mojo — universal Mojo int8 Q4_K prefill GEMM (AOT) =====
# Portable per-32-block NATIVE-LAYOUT Q4_K int8 multistage tensor-core GEMM (int8
# m16n8k32 mma, sm_80+), the universal DEFAULT Q4_K prefill GEMM on ALL arches —
# the CUDA-free replacement for the vendored llama.cpp MMQ cubin
# (docs/CODEGEN_PROOF.md Finding P). The multistage pipeline lives in
# `modular_i8/multistage_i8_q4k_native.mojo` (forked from Modular's Apache-2.0
# linalg; see that file's header). This wrapper exposes it as bare-pointer AOT
# kernels FORGE launches through its cudarc/PTX path.
#
# TRUE 1× VRAM: the kernel reads the RAW GGUF `block_q4_K` superblock bytes
# in-kernel — the SAME `DevWeight::Q4K.buf` bytes the decode dp4a GEMV consumes.
# There is NO separate weight copy and NO pre-repacked scale tensor: the 144-byte
# native blocks are read directly from HBM (32 qs bytes per row per BK=64 k-tile,
# de-interleaved in-kernel into the int8 tile the s8 mma consumes), and the
# per-32-block f16 scales `dsc[kb]=d·sc`, `dm[kb]=dmin·m` are computed in-kernel
# from the native block header (llama.cpp get_scale_min_k4). The per-32-block
# flush `acc += dsc·da·sumi − dm·sa` (f16-staged activation scales da/sa) is
# bit-identical to llama.cpp `vec_dot_q4_K_q8_1_impl_mmq`, so quality == Q4_K MMQ
# (PPL 30.31) by construction. Activation q8_1 quant is shared with the committed
# int8-MMQ path (`quantize_act_q8_1`, f32 da/sa). One GEMM PTX per (N,K,MPAD): the
# same PTX serves any token count T (M read at runtime), zero-padded to the
# compile-time ceiling MPAD (a 128-multiple) because the int8 masked cp.async path
# fails to compile — the launcher picks the smallest bucket ≥ T and guards real
# rows in the epilogue.

from layout import TileTensor, Idx, Coord, row_major
from linalg.utils_gpu import MatmulConfig
from std.utils.index import Index
from std.gpu import block_idx, block_dim, thread_idx
from src.modular_i8.multistage_i8_q4k_native import (
    multistage_gemm_q4k_native_kernel,
)


comptime Q4K_I8_CFG = MatmulConfig[
    DType.int8, DType.int8, DType.float32, True
](
    block_tile_shape=Index(128, 128, 64),
    warp_tile_shape=Index(64, 32, 64),
    mma_shape=Index(16, 8, 32),
)


def gemm_q4k_i8_native[N: Int, K: Int, MPAD: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Int8, MutAnyOrigin],
    w: UnsafePointer[UInt8, ImmutAnyOrigin],
    da: UnsafePointer[Float32, ImmutAnyOrigin],
    sa: UnsafePointer[Float32, ImmutAnyOrigin],
    m_real: Int,
):
    """Y[T,N] = Q4_K(B) · q8_1(A): NATIVE GGUF Q4_K weight bytes read in-kernel,
    f16 out. `a` is the int8 activation padded to the static token ceiling MPAD
    (`quantize_act_q8_1`, rows T..MPAD zeroed), `da`/`sa` its per-32-block f32
    scales [K/32, MPAD]; `w` the RAW `DevWeight::Q4K.buf` (144 bytes / 256 weights,
    NO repack — same bytes decode reads); `m_real` the real token count T (rows
    T..MPAD are computed but never stored). MPAD is a compile-time ceiling (a
    128-multiple) because the int8 masked cp.async path fails to compile — one PTX
    per (N,K,MPAD) bucket; the launcher picks the smallest bucket ≥ T. Grid
    (ceil(N/128), MPAD/128); launcher passes dynamic smem + the >48 KB opt-in."""
    var c_dummy = y.bitcast[Float32]()
    var a_nd = TileTensor(a, row_major(Coord(Idx[MPAD], Idx[K])))
    var c_nd = TileTensor(c_dummy, row_major(Coord(Idx[MPAD], Idx[N])))
    multistage_gemm_q4k_native_kernel[
        CLT = c_nd.LayoutType,
        a_type = DType.int8,
        ALT = a_nd.LayoutType,
        c_linear_idx_type = c_nd.linear_idx_type,
        a_linear_idx_type = a_nd.linear_idx_type,
        config = Q4K_I8_CFG,
    ](c_nd, a_nd, w, da, sa, y, m_real)


# Committed (N, K, MPAD) instances for the dense Mistral-7B family (same (N,K) as
# the fp8-modular kernel): (4096,4096) Q/O, (1024,4096) K/V, (14336,4096) gate/up,
# (4096,14336) down. MPAD buckets (128..4096) cover any prefill token count; the
# launcher picks the smallest bucket ≥ T and zero-pads. Unlisted (N,K) or T beyond
# the largest bucket fall back to the committed portable int8-MMQ kernel.
comptime gemm_q4k_i8_native_4096_4096_m128 = gemm_q4k_i8_native[4096, 4096, 128]
comptime gemm_q4k_i8_native_4096_4096_m256 = gemm_q4k_i8_native[4096, 4096, 256]
comptime gemm_q4k_i8_native_4096_4096_m512 = gemm_q4k_i8_native[4096, 4096, 512]
comptime gemm_q4k_i8_native_4096_4096_m1024 = gemm_q4k_i8_native[4096, 4096, 1024]
comptime gemm_q4k_i8_native_4096_4096_m2048 = gemm_q4k_i8_native[4096, 4096, 2048]
comptime gemm_q4k_i8_native_4096_4096_m4096 = gemm_q4k_i8_native[4096, 4096, 4096]

comptime gemm_q4k_i8_native_1024_4096_m128 = gemm_q4k_i8_native[1024, 4096, 128]
comptime gemm_q4k_i8_native_1024_4096_m256 = gemm_q4k_i8_native[1024, 4096, 256]
comptime gemm_q4k_i8_native_1024_4096_m512 = gemm_q4k_i8_native[1024, 4096, 512]
comptime gemm_q4k_i8_native_1024_4096_m1024 = gemm_q4k_i8_native[1024, 4096, 1024]
comptime gemm_q4k_i8_native_1024_4096_m2048 = gemm_q4k_i8_native[1024, 4096, 2048]
comptime gemm_q4k_i8_native_1024_4096_m4096 = gemm_q4k_i8_native[1024, 4096, 4096]

comptime gemm_q4k_i8_native_14336_4096_m128 = gemm_q4k_i8_native[14336, 4096, 128]
comptime gemm_q4k_i8_native_14336_4096_m256 = gemm_q4k_i8_native[14336, 4096, 256]
comptime gemm_q4k_i8_native_14336_4096_m512 = gemm_q4k_i8_native[14336, 4096, 512]
comptime gemm_q4k_i8_native_14336_4096_m1024 = gemm_q4k_i8_native[14336, 4096, 1024]
comptime gemm_q4k_i8_native_14336_4096_m2048 = gemm_q4k_i8_native[14336, 4096, 2048]
comptime gemm_q4k_i8_native_14336_4096_m4096 = gemm_q4k_i8_native[14336, 4096, 4096]

comptime gemm_q4k_i8_native_4096_14336_m128 = gemm_q4k_i8_native[4096, 14336, 128]
comptime gemm_q4k_i8_native_4096_14336_m256 = gemm_q4k_i8_native[4096, 14336, 256]
comptime gemm_q4k_i8_native_4096_14336_m512 = gemm_q4k_i8_native[4096, 14336, 512]
comptime gemm_q4k_i8_native_4096_14336_m1024 = gemm_q4k_i8_native[4096, 14336, 1024]
comptime gemm_q4k_i8_native_4096_14336_m2048 = gemm_q4k_i8_native[4096, 14336, 2048]
comptime gemm_q4k_i8_native_4096_14336_m4096 = gemm_q4k_i8_native[4096, 14336, 4096]
