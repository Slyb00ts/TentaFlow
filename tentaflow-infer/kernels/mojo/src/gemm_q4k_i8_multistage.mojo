# ===== File: gemm_q4k_i8_multistage.mojo — universal Mojo int8 Q4_K prefill GEMM (AOT) =====
# Portable per-32-block Q4_K int8 multistage tensor-core GEMM (int8 m16n8k32 mma,
# sm_80+), the universal DEFAULT Q4_K prefill GEMM on ALL arches — the CUDA-free
# replacement for the vendored llama.cpp MMQ cubin (docs/CODEGEN_PROOF.md Finding
# O). The multistage pipeline itself lives in `modular_i8/` (forked from Modular's
# Apache-2.0 linalg; see those files' headers). This wrapper exposes it as bare-
# pointer AOT kernels FORGE launches through its cudarc/PTX path, plus the one-time
# weight-unpack kernel that turns native GGUF Q4_K superblocks into the unpacked
# int8 weight + per-32-block f16 scale tensors the GEMM consumes.
#
# The per-32-block flush `acc += dsc·da·sumi − dm·sa` (f16 weight scales dsc/dm,
# f16-staged activation scales da/sa) is bit-identical to llama.cpp
# `vec_dot_q4_K_q8_1_impl_mmq`, so quality == Q4_K MMQ (PPL 30.31) by construction.
# Activation q8_1 quant is shared with the committed int8-MMQ path (`quantize_act_q8_1`,
# f32 da/sa). One GEMM PTX per (N,K); the same PTX serves any token count T (M read
# at runtime). Dynamic-M is handled by zero-padding M to a 128-multiple in the
# launcher and guarding real rows in the epilogue.

from layout import TileTensor, Idx, Coord, row_major
from linalg.utils_gpu import MatmulConfig
from std.utils.index import Index
from std.gpu import block_idx, block_dim, thread_idx
from std.memory.unsafe import bitcast
from src.gemv2 import _q4k_scale_min
from src.modular_i8.multistage_i8_q4k_f16 import multistage_gemm_q4k_f16_kernel


comptime Q4K_I8_CFG = MatmulConfig[
    DType.int8, DType.int8, DType.float32, True
](
    block_tile_shape=Index(128, 128, 64),
    warp_tile_shape=Index(64, 32, 64),
    mma_shape=Index(16, 8, 32),
)


def q4k_unpack_i8(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    b_i8: UnsafePointer[Int8, MutAnyOrigin],
    dsc: UnsafePointer[Float16, MutAnyOrigin],
    dm: UnsafePointer[Float16, MutAnyOrigin],
    N: Int,
    K: Int,
):
    """One-time weight preprocess: native GGUF Q4_K [N,K] → unpacked int8 codes
    `b_i8` [N,K] plus per-32-block f16 scales `dsc[kb*N+n]=d*sc`, `dm[kb*N+n]=
    dmin*m` (kb-major so the GEMM's cooperative scale load coalesces). One thread
    per (row n, 32-block sidx); bit layout mirrors the committed int8-MMQ Q4_K
    unpack (gemm_i8mma_impl FMT=1). Runtime N,K — one PTX for any Q4_K shape."""
    var nkb = K // 32
    var bpr = K // 256  # Q4_K superblocks per row
    idx = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    if idx >= N * nkb:
        return
    n = idx // nkb
    sidx = idx % nkb  # global 32-block within the row
    nsb = sidx // 8  # superblock
    nsub = sidx % 8  # sub-block within superblock
    nchunk = nsub // 2
    half = nsub % 2
    row_base = n * bpr * 144
    hdr = (w + row_base + nsb * 144).load[width=16, alignment=16]()
    ds = bitcast[DType.float16, 8](hdr)
    sc, mn = _q4k_scale_min(hdr, nsub)
    dsc[sidx * N + n] = Float16(Float32(ds[0]) * sc)
    dm[sidx * N + n] = Float16(Float32(ds[1]) * mn)
    base = row_base + nsb * 144 + 16 + nchunk * 32
    obase = n * K + sidx * 32
    codes = (w + base).load[width=32, alignment=8]()
    unpacked = ((codes >> UInt8(4 * half)) & UInt8(0x0F)).cast[DType.int8]()
    (b_i8 + obase).store[alignment=8](unpacked)


def gemm_q4k_i8_mod[N: Int, K: Int, MPAD: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Int8, MutAnyOrigin],
    b: UnsafePointer[Int8, MutAnyOrigin],
    dsc: UnsafePointer[Float16, ImmutAnyOrigin],
    dm: UnsafePointer[Float16, ImmutAnyOrigin],
    da: UnsafePointer[Float32, ImmutAnyOrigin],
    sa: UnsafePointer[Float32, ImmutAnyOrigin],
    m_real: Int,
):
    """Y[T,N] = Q4_K(B) · q8_1(A): int8 tiles, f16 out. `a` is the int8 activation
    padded to the static token ceiling MPAD (`quantize_act_q8_1`, rows T..MPAD
    zeroed), `da`/`sa` its per-32-block f32 scales [K/32, MPAD]; `b`/`dsc`/`dm` the
    unpacked Q4_K weight from `q4k_unpack_i8`; `m_real` the real token count T
    (rows T..MPAD are computed but never stored). MPAD is a compile-time ceiling
    (a 128-multiple) because the int8 masked cp.async path fails to compile — one
    PTX per (N,K,MPAD) bucket; the launcher picks the smallest bucket ≥ T. Grid
    (ceil(N/128), MPAD/128); launcher passes dynamic smem + the >48 KB opt-in."""
    var c_dummy = y.bitcast[Float32]()
    var a_nd = TileTensor(a, row_major(Coord(Idx[MPAD], Idx[K])))
    var b_nd = TileTensor(b, row_major(Coord(Idx[N], Idx[K])))
    var c_nd = TileTensor(c_dummy, row_major(Coord(Idx[MPAD], Idx[N])))
    multistage_gemm_q4k_f16_kernel[
        CLT = c_nd.LayoutType,
        a_type = DType.int8,
        ALT = a_nd.LayoutType,
        BLT = b_nd.LayoutType,
        c_linear_idx_type = c_nd.linear_idx_type,
        a_linear_idx_type = a_nd.linear_idx_type,
        b_linear_idx_type = b_nd.linear_idx_type,
        config = Q4K_I8_CFG,
    ](c_nd, a_nd, b_nd, dsc, dm, da, sa, y, m_real)


# Committed (N, K, MPAD) instances for the dense Mistral-7B family (same (N,K) as
# the fp8-modular kernel): (4096,4096) Q/O, (1024,4096) K/V, (14336,4096) gate/up,
# (4096,14336) down. MPAD buckets (128..) cover any prefill token count; the
# launcher picks the smallest bucket ≥ T and zero-pads. Unlisted (N,K) or T beyond
# the largest bucket fall back to the committed portable int8-MMQ kernel.
comptime gemm_q4k_i8_mod_4096_4096_m512 = gemm_q4k_i8_mod[4096, 4096, 512]
comptime gemm_q4k_i8_mod_1024_4096_m512 = gemm_q4k_i8_mod[1024, 4096, 512]
comptime gemm_q4k_i8_mod_14336_4096_m512 = gemm_q4k_i8_mod[14336, 4096, 512]
comptime gemm_q4k_i8_mod_4096_14336_m512 = gemm_q4k_i8_mod[4096, 14336, 512]
