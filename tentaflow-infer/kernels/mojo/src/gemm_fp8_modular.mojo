# ===== File: gemm_fp8_modular.mojo — Modular multistage fp8 prefill GEMM (AOT) =====
# Wraps Modular's `multistage_gemm_kernel` (linalg) — a deep cp.async multi-stage
# e4m3×e4m3→f32 tensor-core GEMM — as a self-contained AOT kernel FORGE launches
# through its cudarc/PTX path (docs/CODEGEN_PROOF.md Finding G). The wrapper takes
# BARE pointers + a runtime token count `m` so every param is one 8-byte slot
# (FORGE's HAL contract), builds the operand `TileTensor`s device-side with a
# DYNAMIC M and STATIC (N,K), and fuses the per-token × per-row scale + f16
# downcast into the GEMM's epilogue (`elementwise_lambda_fn`) — no extra HBM pass.
#
# Jeden PTX na wariant (N,K,BN) obsługuje dowolne T odczytywane w czasie
# wykonania. Kafel bloku ma 128×BN×64, warp 64×64×64 i cztery etapy. FP8 mma
# wymaga PTX ISA
# .version >= 8.4; build_kernels.mojo lifts the committed .ptx via `_finalize_fp8`
# (Mojo's NVPTX emitter caps sm_89 at 8.1; Ada's 4th-gen fp8 tensor cores are
# hardware-valid at 8.4). Non-default prefill GEMM (`FORGE_GEMM=fp8mod`).

from layout import TileTensor, Idx, Coord, row_major
from linalg.matmul.gpu._multistage_gemm_gpu import multistage_gemm_kernel
from linalg.utils_gpu import MatmulConfig
from std.utils.index import Index, IndexList


def gemm_fp8_mod_tile[
    N: Int, K: Int, BN: Int, LDY: Int = N
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    b: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    ws: UnsafePointer[Float32, MutAnyOrigin],
    m: Int,
):
    comptime config = MatmulConfig[
        DType.float8_e4m3fn, DType.float8_e4m3fn, DType.float32, True
    ](
        block_tile_shape=Index(128, BN, 64),
        warp_tile_shape=Index(64, 64, 64),
    )
    """Y[T,N] = diag(xs)·(A[T,K]·B[N,K]^T)·diag(ws), fp8 e4m3 in, f16 out.

    `a` zawiera aktywacje e4m3 [T,K], a `xs` skale F32 per token. `b` zawiera
    wagi e4m3 [N,K], a `ws` skale F32 per wiersz. Siatka ma kształt
    (ceil(N/BN), ceil(T/128)); skala i zapis F16 są częścią epilogu GEMM.
    """
    # c is never dereferenced when an epilogue lambda is set (the kernel reads
    # only its runtime M dim); reuse `y` as a valid f32-typed pointer for it.
    var c_dummy = y.bitcast[Float32]()
    var a_nd = TileTensor(a, row_major(Coord(m, Idx[K])))
    var b_nd = TileTensor(b, row_major(Coord(Idx[N], Idx[K])))
    var c_nd = TileTensor(c_dummy, row_major(Coord(m, Idx[N])))

    @parameter
    @always_inline
    def epi[
        dtype: DType, width: SIMDSize, *, alignment: Int = 1
    ](idx: IndexList[2], val: SIMD[dtype, width]):
        var t = idx[0]
        var col = idx[1]
        var sa = xs[t]
        comptime for j in range(width):
            y[t * LDY + col + j] = (
                val[j].cast[DType.float32]() * sa * ws[col + j]
            ).cast[DType.float16]()

    multistage_gemm_kernel[
        CLT=c_nd.LayoutType,
        ALT=a_nd.LayoutType,
        BLT=b_nd.LayoutType,
        c_linear_idx_type=c_nd.linear_idx_type,
        a_linear_idx_type=a_nd.linear_idx_type,
        b_linear_idx_type=b_nd.linear_idx_type,
        config=config,
        elementwise_lambda_fn=epi,
    ](c_nd, a_nd, b_nd)


# Committed (N, K) instances for the target dense models (Mistral-7B family):
# (4096,4096) Q/O, (1024,4096) K/V, (14336,4096) gate/up, (4096,14336) down.
comptime gemm_fp8_mod_4096_4096 = gemm_fp8_mod_tile[4096, 4096, 128]
comptime gemm_fp8_mod_1024_4096 = gemm_fp8_mod_tile[1024, 4096, 128]
comptime gemm_fp8_mod_14336_4096 = gemm_fp8_mod_tile[14336, 4096, 128]
comptime gemm_fp8_mod_4096_14336 = gemm_fp8_mod_tile[4096, 14336, 128]
comptime gemm_fp8_mod_11264_4096 = gemm_fp8_mod_tile[11264, 4096, 128]
comptime gemm_fp8_mod_4096_11264 = gemm_fp8_mod_tile[4096, 11264, 128]
comptime gemm_fp8_mod_4096_4096_bn256 = gemm_fp8_mod_tile[4096, 4096, 256]
comptime gemm_fp8_mod_11264_4096_bn256 = gemm_fp8_mod_tile[11264, 4096, 256]
# Projekcja `down` (N=4096, K=duże) zyskuje na BN=256 nieporównanie więcej niż
# pozostałe: sweep `bench_fp8_modular_tiles.mojo` na GB10 dla M=1024 daje
# 1471.9 -> 867.0 us (-41%), czyli 64 -> 109 TFLOPS, podczas gdy q/o i gate/up
# zyskują po ~4%. Brak tych wariantów zostawiał najwolniejszy GEMM prefillu na
# kaflu, który jest dla niego najgorszy.
comptime gemm_fp8_mod_4096_11264_bn256 = gemm_fp8_mod_tile[4096, 11264, 256]
comptime gemm_fp8_mod_4096_14336_bn256 = gemm_fp8_mod_tile[4096, 14336, 256]


# Wydajnosc tego GEMM ma maksimum przy N=4096 i zalamuje sie powyzej: przy
# K=4096 i M=1024 zmierzono 142 TFLOPS dla N=4096, 62 dla N=8192 i 47 dla
# N=11264. Te same 94.5 GFLOP policzone jako 4096+4096+3072 zajmuja 661 us
# zamiast 2016 us, czyli 3.05x szybciej. Ponizsze warianty licza WYCINEK kolumn
# i zapisuja go do pelnej macierzy wyjsciowej o kroku wiersza LDY.
comptime gemm_fp8_mod_4096x11264_4096 = gemm_fp8_mod_tile[4096, 4096, 256, 11264]
comptime gemm_fp8_mod_3072x11264_4096 = gemm_fp8_mod_tile[3072, 4096, 256, 11264]
comptime gemm_fp8_mod_4096x14336_4096 = gemm_fp8_mod_tile[4096, 4096, 256, 14336]
comptime gemm_fp8_mod_2048x14336_4096 = gemm_fp8_mod_tile[2048, 4096, 256, 14336]
