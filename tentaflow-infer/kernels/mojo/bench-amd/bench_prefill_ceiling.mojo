# =============================================================================
# Plik: bench_prefill_ceiling.mojo
# Opis: Ile z kafla prefillowego zjada ROZPAKOWANIE wag, a ile sama jednostka
#       macierzowa. Ten sam kafel BM512/BN128 liczony na wagach juz w f16 (bez
#       dekwantyzacji) jest sufitem dla wariantu Q4_K.
# Przyklad: pixi run mojo run -I . bench-amd/bench_prefill_ceiling.mojo
# =============================================================================
from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.host import DeviceContext
from std.gpu.memory import AddressSpace
from std.gpu.sync import barrier
from std.memory import stack_allocation
from std.random import random_si64, seed
from std.time import perf_counter_ns

from src.arch_wmma import wmma_acc_row, wmma_f16_16x16x16

comptime WARMUP = 2
comptime ITERS = 6
comptime TILE = 16
comptime CHUNK = 64
comptime GROUPS = CHUNK // TILE
comptime LDS_PAD = 16


def gemm_f16_ceiling_impl[
    WAVES_M: Int, WAVES_N: Int, MTILE: Int, NTILE: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Kafel identyczny z `gemm_q4_k_wmma_impl`, ale wagi sa JUZ f16.

    Rozni sie wylacznie etapem stagowania: zamiast rozpakowac superblok Q4_K,
    kopiuje szesnascie gotowych wartosci. Wszystko inne — geometria, LDS,
    bariery, kolejnosc WMMA — jest to samo, wiec roznica czasu to koszt
    dekwantyzacji.
    """
    comptime BM = WAVES_M * MTILE * TILE
    comptime BN = WAVES_N * NTILE * TILE
    comptime ROW = CHUNK + LDS_PAD

    var lane = Int(thread_idx.x) % 32
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
    ws = stack_allocation[BN * ROW, Float16, address_space = AddressSpace.SHARED]()
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
            frag = (
                weights + source_row * n_cols + chunk * CHUNK + group * TILE
            ).load[width=16, alignment=2]()
            (ws + local_row * ROW + group * TILE).store(frag)
            slot += threads
        barrier()

        comptime for sub in range(GROUPS):
            var column = chunk * CHUNK + sub * TILE
            var a = InlineArray[SIMD[DType.float16, 16], MTILE](
                fill=SIMD[DType.float16, 16](0.0)
            )
            comptime for mt in range(MTILE):
                a[mt] = (x + x_base[mt] + column).load[width=16, alignment=2]()
            comptime for nt in range(NTILE):
                local_n = (wave % WAVES_N) * NTILE * TILE + nt * TILE + lane % 16
                b = (ws + local_n * ROW + sub * TILE).load[width=16, alignment=2]()
                comptime for mt in range(MTILE):
                    acc[mt * NTILE + nt] = wmma_f16_16x16x16(
                        a[mt], b, acc[mt * NTILE + nt]
                    )
        barrier()
        chunk += 1

    comptime for mt in range(MTILE):
        comptime for nt in range(NTILE):
            comptime for i in range(8):
                var m = base_m + mt * TILE + wmma_acc_row(lane, i)
                var n = base_n + nt * TILE + lane % 16
                if m < n_tokens and n < n_rows:
                    y[m * n_rows + n] = Float16(acc[mt * NTILE + nt][i])


def gemm_f16_lds_a_impl[
    WAVES_M: Int, WAVES_N: Int, MTILE: Int, NTILE: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Ten sam kafel, ale AKTYWACJE tez ida przez LDS.

    Fragment `a` WMMA wymaga, zeby linia niosla 16 kolejnych kolumn SWOJEGO
    wiersza — czyli przy czytaniu wprost z pamieci globalnej szesnascie linii
    siega adresow odleglych o `n_cols * 2` bajtow (10 KB dla 27B). To jest 32
    osobne transakcje na fale zamiast osmiu. Tutaj kafel aktywacji wchodzi do
    LDS odczytem SKOALESCOWANYM (sasiednie watki czytaja sasiednie 32 B), a
    fragmenty biora sie juz z LDS.
    """
    comptime BM = WAVES_M * MTILE * TILE
    comptime BN = WAVES_N * NTILE * TILE
    comptime ROW = CHUNK + LDS_PAD

    var lane = Int(thread_idx.x) % 32
    var wave = Int(thread_idx.x) // 32
    var base_m = Int(block_idx.y) * BM
    var base_n = Int(block_idx.x) * BN
    var wave_m = base_m + (wave // WAVES_N) * MTILE * TILE
    var wave_n = base_n + (wave % WAVES_N) * NTILE * TILE

    var acc = InlineArray[SIMD[DType.float32, 8], MTILE * NTILE](
        fill=SIMD[DType.float32, 8](0.0)
    )
    xs = stack_allocation[BM * ROW, Float16, address_space = AddressSpace.SHARED]()
    ws = stack_allocation[BN * ROW, Float16, address_space = AddressSpace.SHARED]()
    var threads = Int(block_dim.x)
    var tid = Int(thread_idx.x)

    var chunk = 0
    while chunk < n_cols // CHUNK:
        var slot = tid
        while slot < BN * GROUPS:
            local_row = slot // GROUPS
            group = slot % GROUPS
            var source_row = base_n + local_row
            if source_row > n_rows - 1:
                source_row = n_rows - 1
            (ws + local_row * ROW + group * TILE).store(
                (weights + source_row * n_cols + chunk * CHUNK + group * TILE).load[
                    width=16, alignment=2
                ]()
            )
            slot += threads
        slot = tid
        while slot < BM * GROUPS:
            local_row = slot // GROUPS
            group = slot % GROUPS
            var source_row = base_m + local_row
            if source_row > n_tokens - 1:
                source_row = n_tokens - 1
            (xs + local_row * ROW + group * TILE).store(
                (x + source_row * n_cols + chunk * CHUNK + group * TILE).load[
                    width=16, alignment=2
                ]()
            )
            slot += threads
        barrier()

        comptime for sub in range(GROUPS):
            var a = InlineArray[SIMD[DType.float16, 16], MTILE](
                fill=SIMD[DType.float16, 16](0.0)
            )
            comptime for mt in range(MTILE):
                local_m = (wave // WAVES_N) * MTILE * TILE + mt * TILE + lane % 16
                a[mt] = (xs + local_m * ROW + sub * TILE).load[width=16, alignment=2]()
            comptime for nt in range(NTILE):
                local_n = (wave % WAVES_N) * NTILE * TILE + nt * TILE + lane % 16
                b = (ws + local_n * ROW + sub * TILE).load[width=16, alignment=2]()
                comptime for mt in range(MTILE):
                    acc[mt * NTILE + nt] = wmma_f16_16x16x16(
                        a[mt], b, acc[mt * NTILE + nt]
                    )
        barrier()
        chunk += 1

    comptime for mt in range(MTILE):
        comptime for nt in range(NTILE):
            comptime for i in range(8):
                var m = wave_m + mt * TILE + wmma_acc_row(lane, i)
                var n = wave_n + nt * TILE + lane % 16
                if m < n_tokens and n < n_rows:
                    y[m * n_rows + n] = Float16(acc[mt * NTILE + nt][i])


def one_lds[WM: Int, WN: Int, MT: Int, NT: Int](
    ctx: DeviceContext,
    label: String,
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
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
        ctx.enqueue_function[gemm_f16_lds_a_impl[WM, WN, MT, NT]](
            y, w, x, n_cols, n_rows, n_tokens,
            grid_dim=((n_rows + BN - 1) // BN, (n_tokens + BM - 1) // BM),
            block_dim=THREADS,
        )
    ctx.synchronize()
    s = Float64(perf_counter_ns() - t0) / 1e9 / Float64(ITERS)
    print("   ", label, "BM", BM, "BN", BN, "|", Int(s * 1e6), "us =",
          Int(flops / s / 1e12), "TFLOPS")


def gemm_f16_pipelined_impl[
    WAVES_M: Int, WAVES_N: Int, MTILE: Int, NTILE: Int
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Kafel z POTOKOWANIEM: fragmenty aktywacji kroku `sub + 1` sa ladowane
    ZANIM policza sie mnozenia kroku `sub`.

    W wariancie podstawowym kazdy podkrok najpierw laduje `MTILE` fragmentow z
    pamieci globalnej, a dopiero potem liczy — czyli opoznienie odczytu lezy na
    sciezce krytycznej za kazdym razem. Tutaj odczyt nastepnego podkroku jest
    wystawiony przed mnozeniami biezacego i chowa sie za nimi.
    """
    comptime BM = WAVES_M * MTILE * TILE
    comptime BN = WAVES_N * NTILE * TILE
    comptime ROW = CHUNK + LDS_PAD

    var lane = Int(thread_idx.x) % 32
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
    ws = stack_allocation[BN * ROW, Float16, address_space = AddressSpace.SHARED]()
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
                (weights + source_row * n_cols + chunk * CHUNK + group * TILE).load[
                    width=16, alignment=2
                ]()
            )
            slot += threads
        barrier()

        var cur = InlineArray[SIMD[DType.float16, 16], MTILE](
            fill=SIMD[DType.float16, 16](0.0)
        )
        comptime for mt in range(MTILE):
            cur[mt] = (x + x_base[mt] + chunk * CHUNK).load[width=16, alignment=2]()

        comptime for sub in range(GROUPS):
            var nxt = InlineArray[SIMD[DType.float16, 16], MTILE](
                fill=SIMD[DType.float16, 16](0.0)
            )
            comptime if sub + 1 < GROUPS:
                comptime for mt in range(MTILE):
                    nxt[mt] = (
                        x + x_base[mt] + chunk * CHUNK + (sub + 1) * TILE
                    ).load[width=16, alignment=2]()
            comptime for nt in range(NTILE):
                local_n = (wave % WAVES_N) * NTILE * TILE + nt * TILE + lane % 16
                b = (ws + local_n * ROW + sub * TILE).load[width=16, alignment=2]()
                comptime for mt in range(MTILE):
                    acc[mt * NTILE + nt] = wmma_f16_16x16x16(
                        cur[mt], b, acc[mt * NTILE + nt]
                    )
            comptime if sub + 1 < GROUPS:
                comptime for mt in range(MTILE):
                    cur[mt] = nxt[mt]
        barrier()
        chunk += 1

    comptime for mt in range(MTILE):
        comptime for nt in range(NTILE):
            comptime for i in range(8):
                var m = base_m + mt * TILE + wmma_acc_row(lane, i)
                var n = base_n + nt * TILE + lane % 16
                if m < n_tokens and n < n_rows:
                    y[m * n_rows + n] = Float16(acc[mt * NTILE + nt][i])


def one_pipe[WM: Int, WN: Int, MT: Int, NT: Int](
    ctx: DeviceContext,
    label: String,
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
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
        ctx.enqueue_function[gemm_f16_pipelined_impl[WM, WN, MT, NT]](
            y, w, x, n_cols, n_rows, n_tokens,
            grid_dim=((n_rows + BN - 1) // BN, (n_tokens + BM - 1) // BM),
            block_dim=THREADS,
        )
    ctx.synchronize()
    s = Float64(perf_counter_ns() - t0) / 1e9 / Float64(ITERS)
    print("   ", label, "BM", BM, "BN", BN, "|", Int(s * 1e6), "us =",
          Int(flops / s / 1e12), "TFLOPS")


def one[WM: Int, WN: Int, MT: Int, NT: Int](
    ctx: DeviceContext,
    label: String,
    y: UnsafePointer[Float16, MutAnyOrigin],
    w: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
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
        ctx.enqueue_function[gemm_f16_ceiling_impl[WM, WN, MT, NT]](
            y,
            w,
            x,
            n_cols,
            n_rows,
            n_tokens,
            grid_dim=((n_rows + BN - 1) // BN, (n_tokens + BM - 1) // BM),
            block_dim=THREADS,
        )
    ctx.synchronize()
    s = Float64(perf_counter_ns() - t0) / 1e9 / Float64(ITERS)
    print("   ", label, "BM", BM, "BN", BN, "|", Int(s * 1e6), "us =",
          Int(flops / s / 1e12), "TFLOPS")


def shape(ctx: DeviceContext, n_rows: Int, n_cols: Int, n_tokens: Int) raises:
    var wb = ctx.enqueue_create_host_buffer[DType.float16](n_rows * n_cols)
    var xb = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_cols)
    ctx.synchronize()
    for i in range(n_rows * n_cols):
        wb[i] = Float16(Float64(Int(random_si64(-4, 4))) * 0.25)
    for i in range(n_tokens * n_cols):
        xb[i] = Float16(Float64(Int(random_si64(-4, 4))) * 0.25)
    var w = ctx.enqueue_create_buffer[DType.float16](n_rows * n_cols)
    var x = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_cols)
    var y = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(w, wb)
    ctx.enqueue_copy(x, xb)
    ctx.synchronize()
    flops = 2.0 * Float64(n_tokens) * Float64(n_rows) * Float64(n_cols)
    yp = rebind[UnsafePointer[Float16, MutAnyOrigin]](y.unsafe_ptr())
    wp = rebind[UnsafePointer[Float16, MutAnyOrigin]](w.unsafe_ptr())
    xp = rebind[UnsafePointer[Float16, MutAnyOrigin]](x.unsafe_ptr())
    print("rows=", n_rows, "cols=", n_cols, "T=", n_tokens)
    one[8, 2, 4, 4](ctx, String("bazowy M4xN4 BM512"), yp, wp, xp, n_rows, n_cols, n_tokens, flops)
    one_pipe[8, 2, 4, 4](ctx, String("potok  M4xN4 BM512"), yp, wp, xp, n_rows, n_cols, n_tokens, flops)
    one_pipe[4, 2, 4, 4](ctx, String("potok  M4xN4 BM256"), yp, wp, xp, n_rows, n_cols, n_tokens, flops)
    one_pipe[8, 4, 4, 2](ctx, String("potok  M4xN2 BM512"), yp, wp, xp, n_rows, n_cols, n_tokens, flops)

def main() raises:
    seed(20260730)
    var ctx = DeviceContext()
    shape(ctx, 17408, 5120, 1024)
    shape(ctx, 5120, 6144, 1024)
