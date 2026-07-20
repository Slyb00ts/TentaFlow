# ===== File: gemm_q4k_i8_multistage.mojo — universal Mojo int8 Q4_K prefill GEMM (AOT) =====
# Portable per-32-block PACKED Q4_K int8 multistage tensor-core GEMM (int8
# m16n8k32 mma, sm_80+), the universal DEFAULT Q4_K prefill GEMM on ALL arches —
# the CUDA-free replacement for the vendored llama.cpp MMQ cubin
# (docs/CODEGEN_PROOF.md Finding O). The multistage pipeline lives in
# `modular_i8/multistage_i8_q4k_pack.mojo` (forked from Modular's Apache-2.0
# linalg; see that file's header). This wrapper exposes it as bare-pointer AOT
# kernels FORGE launches through its cudarc/PTX path, plus the one-time weight
# repack kernel that turns native GGUF Q4_K superblocks into the PACKED 4-bit
# weight ([N,K/2] uint8, 1× VRAM — same byte count as the GGUF 4-bit quants) plus
# the per-32-block f16 scale tensors the GEMM consumes.
#
# The packed weight is cp.async'd into smem and unpacked in-kernel (half the HBM
# bandwidth of the unpacked int8 [N,K] path — this is why the packed variant was
# chosen: 1× VRAM, no weight doubling). The per-32-block flush
# `acc += dsc·da·sumi − dm·sa` (f16 weight scales dsc/dm, f16-staged activation
# scales da/sa) is bit-identical to llama.cpp `vec_dot_q4_K_q8_1_impl_mmq`, so
# quality == Q4_K MMQ (PPL 30.31) by construction. Activation q8_1 quant is shared
# with the committed int8-MMQ path (`quantize_act_q8_1`, f32 da/sa). One GEMM PTX
# per (N,K,MPAD): the same PTX serves any token count T (M read at runtime),
# zero-padded to the compile-time ceiling MPAD (a 128-multiple) because the int8
# masked cp.async path fails to compile — the launcher picks the smallest bucket
# ≥ T and guards real rows in the epilogue.

from layout import TileTensor, Idx, Coord, row_major
from linalg.utils_gpu import MatmulConfig
from std.utils.index import Index
from std.gpu import block_idx, block_dim, thread_idx
from std.memory.unsafe import bitcast
from src.gemv2 import _q4k_scale_min
from src.modular_i8.multistage_i8_q4k_pack import multistage_gemm_q4k_pack_kernel


comptime Q4K_I8_CFG = MatmulConfig[
    DType.int8, DType.int8, DType.float32, True
](
    block_tile_shape=Index(128, 128, 64),
    warp_tile_shape=Index(64, 32, 64),
    mma_shape=Index(16, 8, 32),
)


def q4k_repack_pack(
    w: UnsafePointer[UInt8, MutAnyOrigin],
    bp: UnsafePointer[UInt8, MutAnyOrigin],
    dsc: UnsafePointer[Float16, MutAnyOrigin],
    dm: UnsafePointer[Float16, MutAnyOrigin],
    N: Int,
    K: Int,
):
    """One-time weight preprocess: native GGUF Q4_K [N,K] → PACKED 4-bit codes
    `bp` [N,K/2] (plain contiguous nibble order: column k → byte k//2, low nibble
    for even k) plus per-32-block f16 scales `dsc[kb*N+n]=d*sc`, `dm[kb*N+n]=
    dmin*m` (kb-major so the GEMM's cooperative scale load coalesces). One thread
    per (row n, 32-block sidx); bit layout mirrors the committed int8-MMQ Q4_K
    unpack (gemm_i8mma_impl FMT=1) but re-packs the nibbles instead of widening
    them to int8. Runtime N,K — one PTX for any Q4_K shape."""
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
    codes = (w + base).load[width=32, alignment=8]()
    nibbles = (codes >> UInt8(4 * half)) & UInt8(0x0F)
    # Pack the 32 contiguous nibbles into 16 bytes: even columns → low nibble,
    # odd columns → high nibble (plain row-major, matching the kernel's unpack).
    ev, od = nibbles.deinterleave()
    packed = ev | (od << UInt8(4))
    pbase = n * (K // 2) + sidx * 16
    (bp + pbase).store[alignment=8](packed)


def gemm_q4k_i8_mod_pack[N: Int, K: Int, MPAD: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Int8, MutAnyOrigin],
    bp: UnsafePointer[UInt8, MutAnyOrigin],
    dsc: UnsafePointer[Float16, ImmutAnyOrigin],
    dm: UnsafePointer[Float16, ImmutAnyOrigin],
    da: UnsafePointer[Float32, ImmutAnyOrigin],
    sa: UnsafePointer[Float32, ImmutAnyOrigin],
    m_real: Int,
):
    """Y[T,N] = Q4_K(B) · q8_1(A): PACKED int8 tiles, f16 out. `a` is the int8
    activation padded to the static token ceiling MPAD (`quantize_act_q8_1`, rows
    T..MPAD zeroed), `da`/`sa` its per-32-block f32 scales [K/32, MPAD]; `bp`/
    `dsc`/`dm` the PACKED Q4_K weight from `q4k_repack_pack`; `m_real` the real
    token count T (rows T..MPAD are computed but never stored). MPAD is a compile-
    time ceiling (a 128-multiple) because the int8 masked cp.async path fails to
    compile — one PTX per (N,K,MPAD) bucket; the launcher picks the smallest
    bucket ≥ T. Grid (ceil(N/128), MPAD/128); launcher passes dynamic smem + the
    >48 KB opt-in."""
    var c_dummy = y.bitcast[Float32]()
    var a_nd = TileTensor(a, row_major(Coord(Idx[MPAD], Idx[K])))
    var bp_nd = TileTensor(bp, row_major(Coord(Idx[N], Idx[K // 2])))
    var c_nd = TileTensor(c_dummy, row_major(Coord(Idx[MPAD], Idx[N])))
    multistage_gemm_q4k_pack_kernel[
        CLT = c_nd.LayoutType,
        a_type = DType.int8,
        ALT = a_nd.LayoutType,
        BPLT = bp_nd.LayoutType,
        c_linear_idx_type = c_nd.linear_idx_type,
        a_linear_idx_type = a_nd.linear_idx_type,
        bp_linear_idx_type = bp_nd.linear_idx_type,
        config = Q4K_I8_CFG,
    ](c_nd, a_nd, bp_nd, dsc, dm, da, sa, y, m_real)


# Committed (N, K, MPAD) instances for the dense Mistral-7B family (same (N,K) as
# the fp8-modular kernel): (4096,4096) Q/O, (1024,4096) K/V, (14336,4096) gate/up,
# (4096,14336) down. MPAD buckets (128..4096) cover any prefill token count; the
# launcher picks the smallest bucket ≥ T and zero-pads. Unlisted (N,K) or T beyond
# the largest bucket fall back to the committed portable int8-MMQ kernel.
comptime gemm_q4k_i8_pack_4096_4096_m128 = gemm_q4k_i8_mod_pack[4096, 4096, 128]
comptime gemm_q4k_i8_pack_4096_4096_m256 = gemm_q4k_i8_mod_pack[4096, 4096, 256]
comptime gemm_q4k_i8_pack_4096_4096_m512 = gemm_q4k_i8_mod_pack[4096, 4096, 512]
comptime gemm_q4k_i8_pack_4096_4096_m1024 = gemm_q4k_i8_mod_pack[4096, 4096, 1024]
comptime gemm_q4k_i8_pack_4096_4096_m2048 = gemm_q4k_i8_mod_pack[4096, 4096, 2048]
comptime gemm_q4k_i8_pack_4096_4096_m4096 = gemm_q4k_i8_mod_pack[4096, 4096, 4096]

comptime gemm_q4k_i8_pack_1024_4096_m128 = gemm_q4k_i8_mod_pack[1024, 4096, 128]
comptime gemm_q4k_i8_pack_1024_4096_m256 = gemm_q4k_i8_mod_pack[1024, 4096, 256]
comptime gemm_q4k_i8_pack_1024_4096_m512 = gemm_q4k_i8_mod_pack[1024, 4096, 512]
comptime gemm_q4k_i8_pack_1024_4096_m1024 = gemm_q4k_i8_mod_pack[1024, 4096, 1024]
comptime gemm_q4k_i8_pack_1024_4096_m2048 = gemm_q4k_i8_mod_pack[1024, 4096, 2048]
comptime gemm_q4k_i8_pack_1024_4096_m4096 = gemm_q4k_i8_mod_pack[1024, 4096, 4096]

comptime gemm_q4k_i8_pack_14336_4096_m128 = gemm_q4k_i8_mod_pack[14336, 4096, 128]
comptime gemm_q4k_i8_pack_14336_4096_m256 = gemm_q4k_i8_mod_pack[14336, 4096, 256]
comptime gemm_q4k_i8_pack_14336_4096_m512 = gemm_q4k_i8_mod_pack[14336, 4096, 512]
comptime gemm_q4k_i8_pack_14336_4096_m1024 = gemm_q4k_i8_mod_pack[14336, 4096, 1024]
comptime gemm_q4k_i8_pack_14336_4096_m2048 = gemm_q4k_i8_mod_pack[14336, 4096, 2048]
comptime gemm_q4k_i8_pack_14336_4096_m4096 = gemm_q4k_i8_mod_pack[14336, 4096, 4096]

comptime gemm_q4k_i8_pack_4096_14336_m128 = gemm_q4k_i8_mod_pack[4096, 14336, 128]
comptime gemm_q4k_i8_pack_4096_14336_m256 = gemm_q4k_i8_mod_pack[4096, 14336, 256]
comptime gemm_q4k_i8_pack_4096_14336_m512 = gemm_q4k_i8_mod_pack[4096, 14336, 512]
comptime gemm_q4k_i8_pack_4096_14336_m1024 = gemm_q4k_i8_mod_pack[4096, 14336, 1024]
comptime gemm_q4k_i8_pack_4096_14336_m2048 = gemm_q4k_i8_mod_pack[4096, 14336, 2048]
comptime gemm_q4k_i8_pack_4096_14336_m4096 = gemm_q4k_i8_mod_pack[4096, 14336, 4096]
