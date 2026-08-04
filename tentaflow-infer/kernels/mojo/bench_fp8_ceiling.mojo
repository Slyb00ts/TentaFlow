# =============================================================================
# Plik: bench_fp8_ceiling.mojo
# Opis: Mierzy sufit instrukcji MMA FP8 e4m3 na GB10 przy rosnacym ILP.
# Przyklad: pixi run mojo bench_fp8_ceiling.mojo
# =============================================================================
#
# Pytanie rozstrzygajace przed pisaniem wlasnego GEMM FP8: ile w ogole zostalo
# do wziecia. Kafel q/o osiaga dzis 146 TFLOPS, wiec jesli sufit samej
# instrukcji lezy blisko tej wartosci, zaden uklad kafla ani kolejnosc blokow
# tego nie ruszy i praca powinna pojsc gdzie indziej.
#
# `bench_mxfp4_mma.mojo` mierzy CO INNEGO: ma jeden akumulator, wiec kolejna
# instrukcja czeka na poprzednia i wychodzi z tego OPOZNIENIE, nie
# przepustowosc. Tutaj kazdy poziom ILP dostaje osobne akumulatory, wiec przy
# dostatecznej liczbie niezaleznych lancuchow potok tensorowy sie wysyca.

from std.gpu import block_dim, block_idx, grid_dim, thread_idx
from std.gpu.host import DeviceBuffer, DeviceContext
from std.gpu.intrinsics import inlined_assembly
from std.sys import _RegisterPackType
from std.time import perf_counter_ns

comptime ITERS = 512
comptime ROUNDS = 7
comptime LAUNCHES = 10
comptime BLOCKS = 384
comptime THREADS = 256
comptime WARPS_PER_BLOCK = THREADS // 32

# m16n8k32 to 16*8*32*2 operacji zmiennoprzecinkowych na instrukcje warpa.
comptime FLOP_PER_MMA = 8192.0


def mma_fp8_k32(
    a0: UInt32, a1: UInt32, a2: UInt32, a3: UInt32,
    b0: UInt32, b1: UInt32,
    c: SIMD[DType.float32, 4],
) -> _RegisterPackType[Float32, Float32, Float32, Float32]:
    return inlined_assembly[
        (
            "mma.sync.aligned.m16n8k32.row.col.f32.e4m3.e4m3.f32 {$0, $1, $2,"
            " $3}, {$4, $5, $6, $7}, {$8, $9}, {$10, $11, $12, $13};"
        ),
        _RegisterPackType[Float32, Float32, Float32, Float32],
        constraints="=f,=f,=f,=f,r,r,r,r,r,r,f,f,f,f",
        has_side_effect=False,
    ](a0, a1, a2, a3, b0, b1, c[0], c[1], c[2], c[3])


def mma_mxfp4_k64(
    a0: UInt32, a1: UInt32, a2: UInt32, a3: UInt32,
    b0: UInt32, b1: UInt32,
    c: SIMD[DType.float32, 4],
    sa: UInt32, sb: UInt32,
) -> _RegisterPackType[Float32, Float32, Float32, Float32]:
    """Blokowo skalowane FP4 e2m1 ze skalami ue8m0, k64 na instrukcje.

    Przerabia dwa razy wiecej K na instrukcje niz FP8, wiec jesli wykonuje sie
    w tym samym czasie, daje dwukrotnosc. Wariant `nvf4` ponizej jest osobnym
    pomiarem, bo to on — nie ten — jest instrukcja realnej sciezki NVFP4.
    """
    return inlined_assembly[
        (
            "mma.sync.aligned.kind::mxf4.block_scale.m16n8k64.row.col.f32.e2m1.e2m1.f32.ue8m0"
            " {$0, $1, $2, $3}, {$4, $5, $6, $7}, {$8, $9}, {$10, $11, $12,"
            " $13}, {$14}, {$16, $17}, {$15}, {$16, $17};"
        ),
        _RegisterPackType[Float32, Float32, Float32, Float32],
        constraints="=f,=f,=f,=f,r,r,r,r,r,r,f,f,f,f,r,r,h,h",
        has_side_effect=False,
    ](a0, a1, a2, a3, b0, b1, c[0], c[1], c[2], c[3], sa, sb,
      UInt16(0), UInt16(0))


def kern_fp4[ILP: Int](out_ptr: UnsafePointer[Float32, MutAnyOrigin]):
    var a = UInt32(Int(thread_idx.x) | 0x01010101)
    var sc = UInt32(0x7F7F7F7F)
    var acc = InlineArray[SIMD[DType.float32, 4], ILP](
        fill=SIMD[DType.float32, 4](0.0)
    )
    for _ in range(ITERS):
        comptime for i in range(ILP):
            var r = mma_mxfp4_k64(a, a, a, a, a, a, acc[i], sc, sc)
            acc[i] = SIMD[DType.float32, 4](r[0], r[1], r[2], r[3])
    var total = SIMD[DType.float32, 4](0.0)
    comptime for i in range(ILP):
        total += acc[i]
    if Int(thread_idx.x) == 0:
        out_ptr[Int(block_idx.x)] = total[0] + total[1] + total[2] + total[3]


def _report_fp4[
    ILP: Int
](ctx: DeviceContext, mut dst: DeviceBuffer[DType.float32]) raises:
    for _ in range(3):
        ctx.enqueue_function[kern_fp4[ILP]](
            dst.unsafe_ptr(), grid_dim=BLOCKS, block_dim=THREADS
        )
    ctx.synchronize()
    var best = Float64(1.0e30)
    for _ in range(ROUNDS):
        var started = perf_counter_ns()
        for _ in range(LAUNCHES):
            ctx.enqueue_function[kern_fp4[ILP]](
                dst.unsafe_ptr(), grid_dim=BLOCKS, block_dim=THREADS
            )
        ctx.synchronize()
        var el = Float64(perf_counter_ns() - started) / Float64(LAUNCHES)
        if el < best:
            best = el
    var mmas = Float64(BLOCKS * WARPS_PER_BLOCK * ITERS * ILP)
    # m16n8k64 to dwa razy wiecej pracy na instrukcje niz m16n8k32
    print("FP4 ILP", ILP, "  ", best / 1000.0, "us   ",
          mmas * FLOP_PER_MMA * 2.0 / best / 1000.0, "TFLOPS")


def mma_nvfp4_k64(
    a0: UInt32, a1: UInt32, a2: UInt32, a3: UInt32,
    b0: UInt32, b1: UInt32,
    c: SIMD[DType.float32, 4],
    sa: UInt32, sb: UInt32,
) -> _RegisterPackType[Float32, Float32, Float32, Float32]:
    """Natywne NVFP4: skale ue4m3 co 16 wartosci (`scale_vec::4X`).

    To instrukcja, na ktorej stoi `bench_nvfp4_native_gemm.mojo`. Cztery skale
    na operand zamiast jednej to inny odczyt rejestru skali niz w `mxf4`, wiec
    sufit trzeba zmierzyc osobno — inaczej porownywaloby sie GEMM z sufitem
    innej instrukcji.
    """
    return inlined_assembly[
        (
            "mma.sync.aligned.kind::mxf4nvf4.block_scale.scale_vec::4X.m16n8k64.row.col.f32.e2m1.e2m1.f32.ue4m3"
            " {$0, $1, $2, $3}, {$4, $5, $6, $7}, {$8, $9}, {$10, $11, $12,"
            " $13}, {$14}, {$16, $17}, {$15}, {$16, $17};"
        ),
        _RegisterPackType[Float32, Float32, Float32, Float32],
        constraints="=f,=f,=f,=f,r,r,r,r,r,r,f,f,f,f,r,r,h,h",
        has_side_effect=False,
    ](a0, a1, a2, a3, b0, b1, c[0], c[1], c[2], c[3], sa, sb,
      UInt16(0), UInt16(0))


def kern_nvf4[ILP: Int](out_ptr: UnsafePointer[Float32, MutAnyOrigin]):
    var a = UInt32(Int(thread_idx.x) | 0x01010101)
    var sc = UInt32(0x3F3F3F3F)
    var acc = InlineArray[SIMD[DType.float32, 4], ILP](
        fill=SIMD[DType.float32, 4](0.0)
    )
    for _ in range(ITERS):
        comptime for i in range(ILP):
            var r = mma_nvfp4_k64(a, a, a, a, a, a, acc[i], sc, sc)
            acc[i] = SIMD[DType.float32, 4](r[0], r[1], r[2], r[3])
    var total = SIMD[DType.float32, 4](0.0)
    comptime for i in range(ILP):
        total += acc[i]
    if Int(thread_idx.x) == 0:
        out_ptr[Int(block_idx.x)] = total[0] + total[1] + total[2] + total[3]


def _report_nvf4[
    ILP: Int
](ctx: DeviceContext, mut dst: DeviceBuffer[DType.float32]) raises:
    for _ in range(3):
        ctx.enqueue_function[kern_nvf4[ILP]](
            dst.unsafe_ptr(), grid_dim=BLOCKS, block_dim=THREADS
        )
    ctx.synchronize()
    var best = Float64(1.0e30)
    for _ in range(ROUNDS):
        var started = perf_counter_ns()
        for _ in range(LAUNCHES):
            ctx.enqueue_function[kern_nvf4[ILP]](
                dst.unsafe_ptr(), grid_dim=BLOCKS, block_dim=THREADS
            )
        ctx.synchronize()
        var el = Float64(perf_counter_ns() - started) / Float64(LAUNCHES)
        if el < best:
            best = el
    var mmas = Float64(BLOCKS * WARPS_PER_BLOCK * ITERS * ILP)
    print("NVFP4 ILP", ILP, "  ", best / 1000.0, "us   ",
          mmas * FLOP_PER_MMA * 2.0 / best / 1000.0, "TFLOPS")


def kern[ILP: Int](out_ptr: UnsafePointer[Float32, MutAnyOrigin]):
    """`ILP` niezaleznych lancuchow mma pod rzad, bez ruchu pamieci.

    Operandy pochodza z `thread_idx`, wiec kompilator nie zwinie ich do stalej,
    a wynik trafia do pamieci, wiec calosc nie zniknie jako martwa.
    """
    var a = UInt32(Int(thread_idx.x) | 0x01010101)
    var acc = InlineArray[SIMD[DType.float32, 4], ILP](
        fill=SIMD[DType.float32, 4](0.0)
    )
    for _ in range(ITERS):
        comptime for i in range(ILP):
            var r = mma_fp8_k32(a, a, a, a, a, a, acc[i])
            acc[i] = SIMD[DType.float32, 4](r[0], r[1], r[2], r[3])

    var total = SIMD[DType.float32, 4](0.0)
    comptime for i in range(ILP):
        total += acc[i]
    if Int(thread_idx.x) == 0:
        out_ptr[Int(block_idx.x)] = total[0] + total[1] + total[2] + total[3]


def _measure[
    ILP: Int
](ctx: DeviceContext, mut dst: DeviceBuffer[DType.float32]) raises -> Float64:
    for _ in range(3):
        ctx.enqueue_function[kern[ILP]](
            dst.unsafe_ptr(), grid_dim=BLOCKS, block_dim=THREADS
        )
    ctx.synchronize()

    var best = Float64(1.0e30)
    for _ in range(ROUNDS):
        var started = perf_counter_ns()
        for _ in range(LAUNCHES):
            ctx.enqueue_function[kern[ILP]](
                dst.unsafe_ptr(), grid_dim=BLOCKS, block_dim=THREADS
            )
        ctx.synchronize()
        var elapsed = Float64(perf_counter_ns() - started) / Float64(LAUNCHES)
        if elapsed < best:
            best = elapsed
    return best


def _report[
    ILP: Int
](ctx: DeviceContext, mut dst: DeviceBuffer[DType.float32]) raises:
    var ns = _measure[ILP](ctx, dst)
    var mmas = Float64(BLOCKS * WARPS_PER_BLOCK * ITERS * ILP)
    var tflops = mmas * FLOP_PER_MMA / ns / 1000.0
    print("ILP", ILP, "  ", ns / 1000.0, "us   ", tflops, "TFLOPS")


comptime STREAM_MB = 512
comptime STREAM_ELEMS = STREAM_MB * 1024 * 1024 // 16


def kern_stream(
    src: UnsafePointer[Float32, MutAnyOrigin],
    out_ptr: UnsafePointer[Float32, MutAnyOrigin],
    n_vec4: Int,
):
    """Strumieniowy odczyt calego bufora, po 16 bajtow na watek.

    Drugi bok roofline'u. GB10 ma pamiec zunifikowana LPDDR5X, wiec pasmo jest
    tu o rzad wielkosci nizsze niz w kartach z HBM i to ono, a nie sufit
    instrukcji, moze ograniczac GEMM przy ponownym czytaniu wag.
    """
    var stride = Int(grid_dim.x) * Int(block_dim.x)
    var index = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    var total = SIMD[DType.float32, 4](0.0)
    while index < n_vec4:
        total += (src + index * 4).load[width=4, alignment=16]()
        index += stride
    if total[0] == 1.2345e-30:
        out_ptr[Int(block_idx.x)] = total[0] + total[1] + total[2] + total[3]


def _bandwidth(ctx: DeviceContext, mut dst: DeviceBuffer[DType.float32]) raises:
    var src = ctx.enqueue_create_buffer[DType.float32](STREAM_ELEMS * 4)
    for _ in range(3):
        ctx.enqueue_function[kern_stream](
            src.unsafe_ptr(), dst.unsafe_ptr(), STREAM_ELEMS,
            grid_dim=BLOCKS, block_dim=THREADS,
        )
    ctx.synchronize()
    var best = Float64(1.0e30)
    for _ in range(ROUNDS):
        var started = perf_counter_ns()
        for _ in range(LAUNCHES):
            ctx.enqueue_function[kern_stream](
                src.unsafe_ptr(), dst.unsafe_ptr(), STREAM_ELEMS,
                grid_dim=BLOCKS, block_dim=THREADS,
            )
        ctx.synchronize()
        var elapsed = Float64(perf_counter_ns() - started) / Float64(LAUNCHES)
        if elapsed < best:
            best = elapsed
    var gb = Float64(STREAM_MB) / 1024.0
    print("odczyt strumieniowy", STREAM_MB, "MiB:", gb / (best / 1.0e9), "GB/s")


def main() raises:
    var ctx = DeviceContext()
    print("arch:", ctx.arch_name())
    var dst = ctx.enqueue_create_buffer[DType.float32](BLOCKS)
    _bandwidth(ctx, dst)
    _report[1](ctx, dst)
    _report[2](ctx, dst)
    _report[4](ctx, dst)
    _report[8](ctx, dst)
    _report[16](ctx, dst)
    _report_fp4[1](ctx, dst)
    _report_fp4[4](ctx, dst)
    _report_fp4[8](ctx, dst)
    _report_nvf4[1](ctx, dst)
    _report_nvf4[4](ctx, dst)
    _report_nvf4[8](ctx, dst)
    # Nasza petla GEMM trzyma 32 akumulatory (128 rejestrow) na watek. Jesli
    # sufit przy tym ILP spada, to nie uklad kafla jest wina, tylko sama liczba
    # zywych akumulatorow.
    _report_nvf4[16](ctx, dst)
    _report_nvf4[32](ctx, dst)
