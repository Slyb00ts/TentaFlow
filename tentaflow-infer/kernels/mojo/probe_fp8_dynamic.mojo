# =============================================================================
# Plik: probe_fp8_dynamic.mojo
# Opis: Ustala, KTORY z wymiarow runtime psuje wynik wieloetapowego GEMM FP8.
# Przyklad: pixi run mojo probe_fp8_dynamic.mojo
# =============================================================================
#
# `bench_fp8_dynamic.mojo` pokazal, ze wersja z runtime'owym N liczy z ta sama
# predkoscia, ale wypisuje inne wartosci: poprawne sa tylko wiersze bedace
# wielokrotnoscia osmiu, a wzorzec nie zalezy od N. To wyglada na przestawienie
# wierszy, nie na uszkodzone liczby. Ten probe izoluje pojedyncze zmienne:
# osobno runtime'owy LDY, osobno runtime'owy N, osobno runtime'owe K.

from layout import TileTensor, Idx, Coord, row_major
from linalg.matmul.gpu._multistage_gemm_gpu import multistage_gemm_kernel
from linalg.utils_gpu import MatmulConfig
from std.gpu.host import DeviceBuffer, DeviceContext
from std.utils.index import Index, IndexList

comptime M = 256
comptime N = 512
comptime K = 256
comptime BK = 64
comptime BN = 256
comptime BM = 128
comptime THREADS = 256
comptime SMEM = 98304

comptime config = MatmulConfig[
    DType.float8_e4m3fn, DType.float8_e4m3fn, DType.float32, True
](block_tile_shape=Index(BM, BN, BK), warp_tile_shape=Index(64, 64, BK))


def gemm_all_static(
    y: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    b: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    m: Int,
):
    var c_dummy = y.bitcast[Float32]()
    var a_nd = TileTensor(a, row_major(Coord(m, Idx[K])))
    var b_nd = TileTensor(b, row_major(Coord(Idx[N], Idx[K])))
    var c_nd = TileTensor(c_dummy, row_major(Coord(m, Idx[N])))

    @parameter
    @always_inline
    def epi[
        dtype: DType, width: SIMDSize, *, alignment: Int = 1
    ](idx: IndexList[2], val: SIMD[dtype, width]):
        (y + idx[0] * N + idx[1]).store(val.cast[DType.float16]())

    multistage_gemm_kernel[
        CLT=c_nd.LayoutType, ALT=a_nd.LayoutType, BLT=b_nd.LayoutType,
        c_linear_idx_type=c_nd.linear_idx_type,
        a_linear_idx_type=a_nd.linear_idx_type,
        b_linear_idx_type=b_nd.linear_idx_type,
        config=config, elementwise_lambda_fn=epi,
    ](c_nd, a_nd, b_nd)


def gemm_dyn_ldy(
    y: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    b: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    m: Int,
    ldy: Int,
):
    """Ksztalt jak w statycznym; z runtime'u przychodzi tylko krok zapisu."""
    var c_dummy = y.bitcast[Float32]()
    var a_nd = TileTensor(a, row_major(Coord(m, Idx[K])))
    var b_nd = TileTensor(b, row_major(Coord(Idx[N], Idx[K])))
    var c_nd = TileTensor(c_dummy, row_major(Coord(m, Idx[N])))

    @parameter
    @always_inline
    def epi[
        dtype: DType, width: SIMDSize, *, alignment: Int = 1
    ](idx: IndexList[2], val: SIMD[dtype, width]):
        (y + idx[0] * ldy + idx[1]).store(val.cast[DType.float16]())

    multistage_gemm_kernel[
        CLT=c_nd.LayoutType, ALT=a_nd.LayoutType, BLT=b_nd.LayoutType,
        c_linear_idx_type=c_nd.linear_idx_type,
        a_linear_idx_type=a_nd.linear_idx_type,
        b_linear_idx_type=b_nd.linear_idx_type,
        config=config, elementwise_lambda_fn=epi,
    ](c_nd, a_nd, b_nd)


def gemm_dyn_n(
    y: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    b: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    m: Int,
    n: Int,
):
    """Szerokosc wyniku z runtime'u, K nadal statyczne."""
    var c_dummy = y.bitcast[Float32]()
    var a_nd = TileTensor(a, row_major(Coord(m, Idx[K])))
    var b_nd = TileTensor(b, row_major(Coord(n, Idx[K])))
    var c_nd = TileTensor(c_dummy, row_major(Coord(m, n)))

    @parameter
    @always_inline
    def epi[
        dtype: DType, width: SIMDSize, *, alignment: Int = 1
    ](idx: IndexList[2], val: SIMD[dtype, width]):
        (y + idx[0] * n + idx[1]).store(val.cast[DType.float16]())

    multistage_gemm_kernel[
        CLT=c_nd.LayoutType, ALT=a_nd.LayoutType, BLT=b_nd.LayoutType,
        c_linear_idx_type=c_nd.linear_idx_type,
        a_linear_idx_type=a_nd.linear_idx_type,
        b_linear_idx_type=b_nd.linear_idx_type,
        config=config, elementwise_lambda_fn=epi,
    ](c_nd, a_nd, b_nd)


def gemm_dyn_k(
    y: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    b: UnsafePointer[Float8_e4m3fn, MutAnyOrigin],
    m: Int,
    k: Int,
):
    """Glebokosc kontrakcji z runtime'u, N statyczne."""
    var c_dummy = y.bitcast[Float32]()
    var a_nd = TileTensor(a, row_major(Coord(m, k)))
    var b_nd = TileTensor(b, row_major(Coord(Idx[N], k)))
    var c_nd = TileTensor(c_dummy, row_major(Coord(m, Idx[N])))

    @parameter
    @always_inline
    def epi[
        dtype: DType, width: SIMDSize, *, alignment: Int = 1
    ](idx: IndexList[2], val: SIMD[dtype, width]):
        (y + idx[0] * N + idx[1]).store(val.cast[DType.float16]())

    multistage_gemm_kernel[
        CLT=c_nd.LayoutType, ALT=a_nd.LayoutType, BLT=b_nd.LayoutType,
        c_linear_idx_type=c_nd.linear_idx_type,
        a_linear_idx_type=a_nd.linear_idx_type,
        b_linear_idx_type=b_nd.linear_idx_type,
        config=config, elementwise_lambda_fn=epi,
    ](c_nd, a_nd, b_nd)


def _compare(
    ctx: DeviceContext,
    want: DeviceBuffer[DType.float16],
    got: DeviceBuffer[DType.float16],
    name: String,
) raises:
    var hr = ctx.enqueue_create_host_buffer[DType.float16](M * N)
    var hg = ctx.enqueue_create_host_buffer[DType.float16](M * N)
    ctx.enqueue_copy(hr, want)
    ctx.enqueue_copy(hg, got)
    ctx.synchronize()
    var diff = 0
    for i in range(M * N):
        if hr[i] != hg[i]:
            diff += 1
    # Gdy wiersz wyladowal gdzie indziej, znajdzmy gdzie: szukamy dla wiersza 1
    # wyniku referencyjnego wsrod wszystkich wierszy otrzymanych.
    var moved_to = -1
    for t in range(M):
        var ok = True
        for c in range(16):
            if hg[t * N + c] != hr[1 * N + c]:
                ok = False
                break
        if ok:
            moved_to = t
            break
    print(name, "roznice:", diff, "z", M * N, "| wiersz 1 znaleziony w:", moved_to)


def main() raises:
    var ctx = DeviceContext()
    var a = ctx.enqueue_create_buffer[DType.float8_e4m3fn](M * K)
    var b = ctx.enqueue_create_buffer[DType.float8_e4m3fn](N * K)
    with a.map_to_host() as v:
        for i in range(len(v)):
            v[i] = Scalar[DType.float8_e4m3fn](Float32((i % 13) - 6) * 0.03125)
    with b.map_to_host() as v:
        for i in range(len(v)):
            v[i] = Scalar[DType.float8_e4m3fn](Float32((i % 7) - 3) * 0.015625)

    var y_ref = ctx.enqueue_create_buffer[DType.float16](M * N)
    var y_ldy = ctx.enqueue_create_buffer[DType.float16](M * N)
    var y_n = ctx.enqueue_create_buffer[DType.float16](M * N)
    var y_k = ctx.enqueue_create_buffer[DType.float16](M * N)
    ctx.enqueue_memset(y_ldy, 0)
    ctx.enqueue_memset(y_n, 0)
    ctx.enqueue_memset(y_k, 0)

    comptime grid = ((N + BN - 1) // BN, (M + BM - 1) // BM)
    ctx.enqueue_function[gemm_all_static](
        y_ref.unsafe_ptr(), a.unsafe_ptr(), b.unsafe_ptr(), M,
        grid_dim=grid, block_dim=THREADS, shared_mem_bytes=SMEM,
    )
    ctx.enqueue_function[gemm_dyn_ldy](
        y_ldy.unsafe_ptr(), a.unsafe_ptr(), b.unsafe_ptr(), M, N,
        grid_dim=grid, block_dim=THREADS, shared_mem_bytes=SMEM,
    )
    ctx.enqueue_function[gemm_dyn_n](
        y_n.unsafe_ptr(), a.unsafe_ptr(), b.unsafe_ptr(), M, N,
        grid_dim=grid, block_dim=THREADS, shared_mem_bytes=SMEM,
    )
    ctx.enqueue_function[gemm_dyn_k](
        y_k.unsafe_ptr(), a.unsafe_ptr(), b.unsafe_ptr(), M, K,
        grid_dim=grid, block_dim=THREADS, shared_mem_bytes=SMEM,
    )
    ctx.synchronize()

    _compare(ctx, y_ref, y_ldy, "runtime LDY  ")
    _compare(ctx, y_ref, y_n, "runtime N    ")
    _compare(ctx, y_ref, y_k, "runtime K    ")
