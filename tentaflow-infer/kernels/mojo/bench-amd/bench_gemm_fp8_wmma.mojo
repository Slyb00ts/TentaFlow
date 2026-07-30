# =============================================================================
# Plik: bench_gemm_fp8_wmma.mojo
# Opis: Prototyp GEMM-u FP8 (e4m3) na jednostce macierzowej RDNA4 wobec kafla
#       f16 o tej samej geometrii. Jednostka fp8 ma 378 TFLOPS wobec 179 f16, a
#       fragment operandu jest CZTERY RAZY mniejszy (2 VGPR zamiast 8) — to
#       drugie zdejmuje sciane rejestrow, o ktora rozbija sie kafel f16.
# Przyklad: pixi run mojo run -I . bench-amd/bench_gemm_fp8_wmma.mojo
# =============================================================================
from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.host import DeviceContext
from std.gpu.memory import AddressSpace
from std.gpu.sync import barrier
from std.memory import bitcast, stack_allocation
from std.random import random_si64, seed
from std.time import perf_counter_ns

from src.arch_wmma import wmma_acc_row, wmma_fp8_16x16x16

comptime WARMUP = 2
comptime ITERS = 6
comptime TILE = 16
comptime CHUNK = 64
comptime GROUPS = CHUNK // TILE


@always_inline
def _frag8(p: UnsafePointer[UInt8, MutAnyOrigin]) -> SIMD[DType.int32, 2]:
    """Osiem bajtow e4m3 tej linii jako fragment operandu RDNA4."""
    return bitcast[DType.int32, 2](p.load[width=8, alignment=1]())


def gemm_fp8_wmma_impl[
    WAVES_M: Int, WAVES_N: Int, MTILE: Int, NTILE: Int, PAD: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    ws_g: UnsafePointer[Float32, MutAnyOrigin],
    xq_g: UnsafePointer[UInt8, MutAnyOrigin],
    xs_g: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Y[t, r] = s_x[t] * s_w[r] * suma_k Xq[t,k] * Wq[r,k], oba operandy e4m3.

    Skale sa PER WIERSZ i PER TOKEN, czyli stale wzdluz K — dzieki temu w petli
    wewnetrznej nie ma zadnego zrzucania akumulatora, inaczej niz przy formatach
    z blokowa skala co 32 kolumny.

    Fragment RDNA4 to osiem bajtow na linie; polowa fali wybiera, ktore osiem z
    szesnastu kolumn kafla niesie ta linia.
    """
    comptime BM = WAVES_M * MTILE * TILE
    comptime BN = WAVES_N * NTILE * TILE
    comptime ROW = CHUNK + PAD

    var lane = Int(thread_idx.x) % 32
    var half = lane // 16
    var wave = Int(thread_idx.x) // 32
    var base_m = Int(block_idx.y) * BM + (wave // WAVES_N) * MTILE * TILE
    var base_n = Int(block_idx.x) * BN + (wave % WAVES_N) * NTILE * TILE

    var x_base = InlineArray[Int, MTILE](fill=0)
    comptime for mt in range(MTILE):
        var m = base_m + mt * TILE + lane % 16
        if m > n_tokens - 1:
            m = n_tokens - 1
        x_base[mt] = m * n_cols

    var acc = InlineArray[SIMD[DType.float32, 8], MTILE * NTILE](
        fill=SIMD[DType.float32, 8](0.0)
    )
    ws = stack_allocation[BN * ROW, UInt8, address_space = AddressSpace.SHARED]()
    var threads = Int(block_dim.x)
    var tid = Int(thread_idx.x)

    var chunk = 0
    while chunk < n_cols // CHUNK:
        var slot = tid
        while slot < BN * GROUPS:
            local_row = slot // GROUPS
            group = slot % GROUPS
            var source_row = Int(block_idx.x) * BN + local_row
            if source_row > n_rows - 1:
                source_row = n_rows - 1
            (ws + local_row * ROW + group * TILE).store(
                (w + source_row * n_cols + chunk * CHUNK + group * TILE).load[
                    width=16, alignment=1
                ]()
            )
            slot += threads
        barrier()

        comptime for sub in range(GROUPS):
            var column = chunk * CHUNK + sub * TILE + half * 8
            var a = InlineArray[SIMD[DType.int32, 2], MTILE](
                fill=SIMD[DType.int32, 2](0)
            )
            comptime for mt in range(MTILE):
                a[mt] = _frag8(xq_g + x_base[mt] + column)
            comptime for nt in range(NTILE):
                local_n = (wave % WAVES_N) * NTILE * TILE + nt * TILE + lane % 16
                b = bitcast[DType.int32, 2](
                    (ws + local_n * ROW + sub * TILE + half * 8).load[
                        width=8, alignment=1
                    ]()
                )
                comptime for mt in range(MTILE):
                    acc[mt * NTILE + nt] = wmma_fp8_16x16x16(
                        a[mt], b, acc[mt * NTILE + nt]
                    )
        barrier()
        chunk += 1

    comptime for mt in range(MTILE):
        comptime for nt in range(NTILE):
            var n = base_n + nt * TILE + lane % 16
            var scale_w: Float32 = 0.0
            if n < n_rows:
                scale_w = ws_g[n]
            comptime for i in range(8):
                var m = base_m + mt * TILE + wmma_acc_row(lane, i)
                if m < n_tokens and n < n_rows:
                    y[m * n_rows + n] = Float16(
                        acc[mt * NTILE + nt][i] * scale_w * xs_g[m]
                    )


def one[WM: Int, WN: Int, MT: Int, NT: Int, PAD: Int](
    ctx: DeviceContext,
    label: String,
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[UInt8, MutAnyOrigin],
    wsg: UnsafePointer[Float32, MutAnyOrigin],
    xq: UnsafePointer[UInt8, MutAnyOrigin],
    xsg: UnsafePointer[Float32, MutAnyOrigin],
    n_rows: Int,
    n_cols: Int,
    n_tokens: Int,
    flops: Float64,
) raises:
    comptime BM = WM * MT * 16
    comptime BN = WN * NT * 16
    comptime THREADS = WM * WN * 32
    var t0: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t0 = perf_counter_ns()
        ctx.enqueue_function[gemm_fp8_wmma_impl[WM, WN, MT, NT, PAD]](
            y, w, wsg, xq, xsg, n_cols, n_rows, n_tokens,
            grid_dim=((n_rows + BN - 1) // BN, (n_tokens + BM - 1) // BM),
            block_dim=THREADS,
        )
    ctx.synchronize()
    s = Float64(perf_counter_ns() - t0) / 1e9 / Float64(ITERS)
    print("   ", label, "BM", BM, "BN", BN, "|", Int(s * 1e6), "us =",
          Int(flops / s / 1e12), "TFLOPS")


def shape(ctx: DeviceContext, n_rows: Int, n_cols: Int, n_tokens: Int) raises:
    var wh = ctx.enqueue_create_host_buffer[DType.uint8](n_rows * n_cols)
    var xh = ctx.enqueue_create_host_buffer[DType.uint8](n_tokens * n_cols)
    var sh = ctx.enqueue_create_host_buffer[DType.float32](n_rows)
    var th = ctx.enqueue_create_host_buffer[DType.float32](n_tokens)
    ctx.synchronize()
    for i in range(n_rows * n_cols):
        wh[i] = UInt8(Int(random_si64(0, 120)))
    for i in range(n_tokens * n_cols):
        xh[i] = UInt8(Int(random_si64(0, 120)))
    for i in range(n_rows):
        sh[i] = 0.01
    for i in range(n_tokens):
        th[i] = 0.02
    var w = ctx.enqueue_create_buffer[DType.uint8](n_rows * n_cols)
    var x = ctx.enqueue_create_buffer[DType.uint8](n_tokens * n_cols)
    var wsg = ctx.enqueue_create_buffer[DType.float32](n_rows)
    var xsg = ctx.enqueue_create_buffer[DType.float32](n_tokens)
    var y = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(w, wh)
    ctx.enqueue_copy(x, xh)
    ctx.enqueue_copy(wsg, sh)
    ctx.enqueue_copy(xsg, th)
    ctx.synchronize()
    flops = 2.0 * Float64(n_tokens) * Float64(n_rows) * Float64(n_cols)
    yp = rebind[UnsafePointer[Float16, MutAnyOrigin]](y.unsafe_ptr())
    wp = rebind[UnsafePointer[UInt8, MutAnyOrigin]](w.unsafe_ptr())
    xp = rebind[UnsafePointer[UInt8, MutAnyOrigin]](x.unsafe_ptr())
    sp = rebind[UnsafePointer[Float32, MutAnyOrigin]](wsg.unsafe_ptr())
    tp = rebind[UnsafePointer[Float32, MutAnyOrigin]](xsg.unsafe_ptr())
    print("rows=", n_rows, "cols=", n_cols, "T=", n_tokens)
    one[8, 2, 4, 4, 16](ctx, String("M4xN4 BM512/BN128"), yp, wp, sp, xp, tp, n_rows, n_cols, n_tokens, flops)
    one[4, 2, 4, 4, 16](ctx, String("M4xN4 BM256/BN128"), yp, wp, sp, xp, tp, n_rows, n_cols, n_tokens, flops)
    one[4, 4, 4, 4, 16](ctx, String("M4xN4 BM256/BN256"), yp, wp, sp, xp, tp, n_rows, n_cols, n_tokens, flops)
    one[8, 2, 8, 2, 16](ctx, String("M8xN2 BM1024/BN64"), yp, wp, sp, xp, tp, n_rows, n_cols, n_tokens, flops)
    one[4, 2, 8, 4, 16](ctx, String("M8xN4 BM512/BN128"), yp, wp, sp, xp, tp, n_rows, n_cols, n_tokens, flops)


def main() raises:
    seed(20260730)
    var ctx = DeviceContext()
    shape(ctx, 17408, 5120, 1024)
    shape(ctx, 5120, 6144, 1024)
