# ===== File: gemm_fp8_modular.mojo — Modular multistage fp8 prefill GEMM (AOT) =====
# Wraps Modular's `multistage_gemm_kernel` (linalg) — a deep cp.async multi-stage
# e4m3×e4m3→f32 tensor-core GEMM — as a self-contained AOT kernel FORGE launches
# through its cudarc/PTX path (docs/CODEGEN_PROOF.md Finding G). The wrapper takes
# BARE pointers + a runtime token count `m` so every param is one 8-byte slot
# (FORGE's HAL contract), builds the operand `TileTensor`s device-side with a
# DYNAMIC M and STATIC (N,K), and fuses the per-token × per-row scale + f16
# downcast into the GEMM's epilogue (`elementwise_lambda_fn`) — no extra HBM pass.
#
# One PTX per (N,K); the same PTX serves ANY token count T (M read at runtime via
# `c.dim[0]()`). Config: block 128×128×64, warp 64×64×64, 4 stages, dynamic smem
# 65536 B (the >48 KB opt-in the HAL already sets for MMQ). fp8 mma needs PTX ISA
# .version >= 8.4; build_kernels.mojo lifts the committed .ptx via `_finalize_fp8`
# (Mojo's NVPTX emitter caps sm_89 at 8.1; Ada's 4th-gen fp8 tensor cores are
# hardware-valid at 8.4). Non-default prefill GEMM (`FORGE_GEMM=fp8mod`).

from layout import TileTensor, Idx, Coord, row_major
from linalg.matmul.gpu._multistage_gemm_gpu import multistage_gemm_kernel
from linalg.utils_gpu import MatmulConfig
from std.utils.index import Index, IndexList

comptime FP8_CFG = MatmulConfig[
    DType.float8_e4m3fn, DType.float8_e4m3fn, DType.float32, True
](
    block_tile_shape=Index(128, 128, 64),
    warp_tile_shape=Index(64, 64, 64),
)

# 2 * BM * BK * size_of(e4m3) * (stages/2 double-buffer) collapses to 2*128*64*4.
comptime FP8_MOD_SMEM = 2 * 128 * 64 * 4


def gemm_fp8_mod[N: Int, K: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    b: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    ws: UnsafePointer[Float32, MutAnyOrigin],
    m: Int,
):
    """Y[T,N] = diag(xs)·(A[T,K]·B[N,K]^T)·diag(ws), fp8 e4m3 in, f16 out.

    `a` is the per-token e4m3 activation [T,K] (`quantize_act_fp8`), `xs` its
    per-token f32 scale [T]; `b` the e4m3 weight [N,K], `ws` its per-row f32
    scale [N]; `m` the runtime token count T. Grid (ceil(N/128), ceil(T/128));
    block 128; dynamic smem FP8_MOD_SMEM. The scale + f16 cast run in the GEMM
    epilogue, so there is no separate scale/downcast pass over [T,N].
    """
    # c is never dereferenced when an epilogue lambda is set (the kernel reads
    # only its runtime M dim); reuse `y` as a valid f32-typed pointer for it.
    var c_dummy = y.bitcast[Float32]()
    var a_nd = TileTensor(a, row_major(Coord(m, Idx[K])))
    var b_nd = TileTensor(b, row_major(Coord(Idx[N], Idx[K])))
    var c_nd = TileTensor(c_dummy, row_major(Coord(m, Idx[N])))

    @parameter
    @always_inline
    def epi[dtype: DType, width: SIMDSize, *, alignment: Int = 1](
        idx: IndexList[2], val: SIMD[dtype, width]
    ):
        var t = idx[0]
        var col = idx[1]
        var sa = xs[t]
        comptime for j in range(width):
            y[t * N + col + j] = (
                val[j].cast[DType.float32]() * sa * ws[col + j]
            ).cast[DType.float16]()

    multistage_gemm_kernel[
        CLT = c_nd.LayoutType,
        ALT = a_nd.LayoutType,
        BLT = b_nd.LayoutType,
        c_linear_idx_type = c_nd.linear_idx_type,
        a_linear_idx_type = a_nd.linear_idx_type,
        b_linear_idx_type = b_nd.linear_idx_type,
        config=FP8_CFG,
        elementwise_lambda_fn=epi,
    ](c_nd, a_nd, b_nd)


# Committed (N, K) instances for the target dense models (Mistral-7B family):
# (4096,4096) Q/O, (1024,4096) K/V, (14336,4096) gate/up, (4096,14336) down.
comptime gemm_fp8_mod_4096_4096 = gemm_fp8_mod[4096, 4096]
comptime gemm_fp8_mod_1024_4096 = gemm_fp8_mod[1024, 4096]
comptime gemm_fp8_mod_14336_4096 = gemm_fp8_mod[14336, 4096]
comptime gemm_fp8_mod_4096_14336 = gemm_fp8_mod[4096, 14336]
