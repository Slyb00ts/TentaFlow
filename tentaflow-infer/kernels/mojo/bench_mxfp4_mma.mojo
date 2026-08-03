# =============================================================================
# Plik: bench_mxfp4_mma.mojo
# Opis: Porownuje przepustowosc instrukcji MMA FP8 (k32) i MXFP4 (k64) na GB10.
# Przyklad: pixi run mojo bench_mxfp4_mma.mojo
# =============================================================================
#
# Pytanie rozstrzygajace przed budowa calej sciezki MXFP4: czy blokowo skalowana
# instrukcja FP4 z k64 wykonuje sie w tym samym czasie co FP8 z k32. Jesli tak,
# daje dwukrotnosc na te sama liczbe instrukcji; jesli nie, konwersja formatu i
# nowy kernel nie maja sensu.
#
# `ptxas` przyjmuje na sm_121a `kind::mxf4.block_scale.m16n8k64` ze skalami
# ue8m0, a odrzuca warianty ze skalami e4m3 (czyli natywne NVFP4). Mierzymy
# wiec to, co jest osiagalne.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.host import DeviceBuffer, DeviceContext
from std.gpu.intrinsics import inlined_assembly
from std.sys import _RegisterPackType
from std.time import perf_counter_ns

comptime ITERS = 200
comptime ROUNDS = 5
comptime BLOCKS = 1024
comptime THREADS = 256


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
    sa: UInt32, sb: UInt32, bid: UInt16, tid: UInt16,
) -> _RegisterPackType[Float32, Float32, Float32, Float32]:
    return inlined_assembly[
        (
            "mma.sync.aligned.kind::mxf4.block_scale.m16n8k64.row.col.f32.e2m1.e2m1.f32.ue8m0"
            " {$0, $1, $2, $3}, {$4, $5, $6, $7}, {$8, $9}, {$10, $11, $12,"
            " $13}, {$14}, {$16, $17}, {$15}, {$16, $17};"
        ),
        _RegisterPackType[Float32, Float32, Float32, Float32],
        constraints="=f,=f,=f,=f,r,r,r,r,r,r,f,f,f,f,r,r,h,h",
        has_side_effect=False,
    ](a0, a1, a2, a3, b0, b1, c[0], c[1], c[2], c[3], sa, sb, bid, tid)


def kern_fp8(out_ptr: UnsafePointer[Float32, MutAnyOrigin]):
    var acc = SIMD[DType.float32, 4](0.0)
    var a = UInt32(Int(thread_idx.x) | 0x01010101)
    for _ in range(ITERS):
        var r = mma_fp8_k32(a, a, a, a, a, a, acc)
        acc = SIMD[DType.float32, 4](r[0], r[1], r[2], r[3])
    if Int(thread_idx.x) == 0:
        out_ptr[Int(block_idx.x)] = acc[0]


def kern_mxfp4(out_ptr: UnsafePointer[Float32, MutAnyOrigin]):
    var acc = SIMD[DType.float32, 4](0.0)
    var a = UInt32(Int(thread_idx.x) | 0x01010101)
    var s = UInt32(0x7F7F7F7F)
    for _ in range(ITERS):
        var r = mma_mxfp4_k64(a, a, a, a, a, a, acc, s, s, UInt16(0), UInt16(0))
        acc = SIMD[DType.float32, 4](r[0], r[1], r[2], r[3])
    if Int(thread_idx.x) == 0:
        out_ptr[Int(block_idx.x)] = acc[0]


def main() raises:
    var ctx = DeviceContext()
    var dst = ctx.enqueue_create_buffer[DType.float32](BLOCKS)

    # rozgrzewka
    for _ in range(2):
        ctx.enqueue_function[kern_fp8](
            dst.unsafe_ptr(), grid_dim=BLOCKS, block_dim=THREADS
        )
        ctx.enqueue_function[kern_mxfp4](
            dst.unsafe_ptr(), grid_dim=BLOCKS, block_dim=THREADS
        )
    ctx.synchronize()

    var best_fp8 = Float64(1.0e30)
    var best_fp4 = Float64(1.0e30)
    for _ in range(ROUNDS):
        var t0 = perf_counter_ns()
        for _ in range(10):
            ctx.enqueue_function[kern_fp8](
                dst.unsafe_ptr(), grid_dim=BLOCKS, block_dim=THREADS
            )
        ctx.synchronize()
        var dt = Float64(perf_counter_ns() - t0) / 1.0e6
        if dt < best_fp8:
            best_fp8 = dt

        t0 = perf_counter_ns()
        for _ in range(10):
            ctx.enqueue_function[kern_mxfp4](
                dst.unsafe_ptr(), grid_dim=BLOCKS, block_dim=THREADS
            )
        ctx.synchronize()
        dt = Float64(perf_counter_ns() - t0) / 1.0e6
        if dt < best_fp4:
            best_fp4 = dt

    # Ta sama liczba instrukcji; FP4 przerabia dwa razy wiecej K na instrukcje.
    print("FP8  k32:", best_fp8, "ms")
    print("MXFP4 k64:", best_fp4, "ms")
    print("przyspieszenie na te sama prace:", (best_fp8 * 2.0) / best_fp4)
