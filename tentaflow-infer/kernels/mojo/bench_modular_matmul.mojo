# ===== bench_modular_matmul.mojo — Modular ready-made bf16 GEMM TOPS on Ada =====
# Calls linalg _matmul_gpu (multistage cp.async pipeline + TensorCore + swizzle)
# at the Mistral FFN shapes and reports achieved bf16 TFLOPS on the RTX 4090.
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from layout import TileTensor, Idx, Coord, row_major
from linalg.matmul.gpu import _matmul_gpu


def _bench[M: Int, N: Int, K: Int](ctx: DeviceContext) raises:
    # C[M,N] = A[M,K] * B[N,K] (transpose_b), bf16 in, f32 out.
    var a_buf = ctx.enqueue_create_buffer[DType.bfloat16](M * K)
    var b_buf = ctx.enqueue_create_buffer[DType.bfloat16](N * K)
    var c_buf = ctx.enqueue_create_buffer[DType.float32](M * N)

    var a_nd = TileTensor(a_buf, row_major(Coord(Idx[M], Idx[K])))
    var b_nd = TileTensor(b_buf, row_major(Coord(Idx[N], Idx[K])))
    var c_nd = TileTensor(c_buf, row_major(Coord(Idx[M], Idx[N])))

    comptime ITERS = 40
    ops = 2.0 * Float64(M) * Float64(N) * Float64(K)

    for _ in range(200):
        _matmul_gpu[use_tensor_core=True, transpose_b=True](c_nd, a_nd, b_nd, ctx)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        _matmul_gpu[use_tensor_core=True, transpose_b=True](c_nd, a_nd, b_nd, ctx)
    ctx.synchronize()
    ms = Float64(perf_counter_ns() - t0) / 1e6 / ITERS

    print(
        "BF16 M(T)=", M, "N=", N, "K=", K,
        " :", ops / (ms / 1e3) / 1e12, "TFLOPS (", ms, "ms)",
    )


def main() raises:
    var ctx = DeviceContext()
    # down-proj: N=4096, K=14336 ; gate/up: N=14336, K=4096
    _bench[512, 4096, 14336](ctx)
    _bench[2048, 4096, 14336](ctx)
    _bench[512, 14336, 4096](ctx)
    _bench[2048, 14336, 4096](ctx)
    _bench[512, 4096, 4096](ctx)
