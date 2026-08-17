# =============================================================================
# Plik: bench_wmma_gfx11.mojo
# Opis: Sprawdza poprawność i mierzy pułap jednostek macierzowych WMMA na RDNA3
#       (gfx11) — int8 i f16. To jest odpowiednik roofline'u dot4/dot2 dla kart
#       z jednostką macierzową i punkt odniesienia dla kafli GEMM.
# Przykład: pixi run mojo run -I . bench-amd/bench_wmma_gfx11.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.gpu import block_dim, block_idx, thread_idx
from std.time import perf_counter_ns

from src.arch_wmma import (
    wmma_f16_16x16x16,
    wmma_i8_16x16x16,
    wmma_fp8_16x16x16,
    wmma_iu4_16x16x32,
)

comptime ITERS = 20
# Ta sama reguła co dla dot4/dot2 na RDNA: poniżej ośmiu niezależnych łańcuchów
# pomiar jest ograniczony latencją, nie przepustowością.
comptime CHAINS = 8
comptime CHAINS16 = 16
comptime CHAINS4 = 4
comptime ROUNDS = 128


def wmma_i8_exact(dst: UnsafePointer[Int32, MutAnyOrigin]):
    """Wiersz szesnastu jedynek razy kolumna jedynek daje 16 w każdym polu.

    Drugi przypadek ma bajty UJEMNE — na RDNA3 to jedyny sposób odróżnienia
    iloczynu ze znakiem od bez znaku, a różnica jest cicha (patrz `arch_dot`).
    """
    ones = SIMD[DType.int32, 4](0x01010101, 0x01010101, 0x01010101, 0x01010101)
    minus = SIMD[DType.int32, 4](-1, -1, -1, -1)  # cztery bajty 0xFF = -1
    var acc = SIMD[DType.int32, 8](0)
    acc = wmma_i8_16x16x16(ones, ones, acc)
    dst[0] = acc[0]
    var neg = SIMD[DType.int32, 8](0)
    neg = wmma_i8_16x16x16(minus, ones, neg)
    dst[1] = neg[0]


def wmma_i8_throughput(sink: UnsafePointer[Int32, MutAnyOrigin], n: Int):
    a = SIMD[DType.int32, 4](0x01020304, 0x01020304, 0x01020304, 0x01020304)
    b = SIMD[DType.int32, 4](0x04030201, 0x04030201, 0x04030201, 0x04030201)
    var acc = InlineArray[SIMD[DType.int32, 8], CHAINS](fill=SIMD[DType.int32, 8](0))
    for _ in range(ROUNDS):
        comptime for j in range(CHAINS):
            acc[j] = wmma_i8_16x16x16(a, b, acc[j])
    i = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    var s: Int32 = 0
    comptime for j in range(CHAINS):
        s += acc[j][0]
    if i < n:
        sink[i] = s


def wmma_f16_throughput(sink: UnsafePointer[Float32, MutAnyOrigin], n: Int):
    var a = SIMD[DType.float16, 16](1.0)
    var b = SIMD[DType.float16, 16](0.5)
    var acc = InlineArray[SIMD[DType.float32, 8], CHAINS](fill=SIMD[DType.float32, 8](0.0))
    for _ in range(ROUNDS):
        comptime for j in range(CHAINS):
            acc[j] = wmma_f16_16x16x16(a, b, acc[j])
    i = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    var s: Float32 = 0.0
    comptime for j in range(CHAINS):
        s += acc[j][0]
    if i < n:
        sink[i] = s


def wmma_f16_chains[C: Int](sink: UnsafePointer[Float32, MutAnyOrigin], n: Int):
    """Ten sam pomiar co `wmma_f16_throughput`, ale z parametryzowana liczba
    niezaleznych akumulatorow — kafel prefillowy ma ich 16, mikrobenchmark 8."""
    var a = SIMD[DType.float16, 16](1.0)
    var b = SIMD[DType.float16, 16](0.5)
    var acc = InlineArray[SIMD[DType.float32, 8], C](fill=SIMD[DType.float32, 8](0.0))
    for _ in range(ROUNDS):
        comptime for j in range(C):
            acc[j] = wmma_f16_16x16x16(a, b, acc[j])
    i = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    var s: Float32 = 0.0
    comptime for j in range(C):
        s += acc[j][0]
    if i < n:
        sink[i] = s


def wmma_fp8_throughput(sink: UnsafePointer[Float32, MutAnyOrigin], n: Int):
    a = SIMD[DType.int32, 2](0x38383838, 0x38383838)  # cztery e4m3 = 0.5
    b = SIMD[DType.int32, 2](0x3C3C3C3C, 0x3C3C3C3C)  # cztery e4m3 = 1.0
    var acc = InlineArray[SIMD[DType.float32, 8], CHAINS](fill=SIMD[DType.float32, 8](0.0))
    for _ in range(ROUNDS):
        comptime for j in range(CHAINS):
            acc[j] = wmma_fp8_16x16x16(a, b, acc[j])
    i = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    var s: Float32 = 0.0
    comptime for j in range(CHAINS):
        s += acc[j][0]
    if i < n:
        sink[i] = s


def wmma_iu4_throughput(sink: UnsafePointer[Int32, MutAnyOrigin], n: Int):
    a = SIMD[DType.int32, 2](0x11111111, 0x11111111)
    b = SIMD[DType.int32, 2](0x22222222, 0x22222222)
    var acc = InlineArray[SIMD[DType.int32, 8], CHAINS](fill=SIMD[DType.int32, 8](0))
    for _ in range(ROUNDS):
        comptime for j in range(CHAINS):
            acc[j] = wmma_iu4_16x16x32(a, b, acc[j])
    i = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    var s: Int32 = 0
    comptime for j in range(CHAINS):
        s += acc[j][0]
    if i < n:
        sink[i] = s


def wmma_iu4_exact(dst: UnsafePointer[Int32, MutAnyOrigin]):
    """Wiersz 32 jedynek razy kolumna jedynek daje 32; drugi przypadek ma kody
    UJEMNE (0xF = -1), bo wariant bez znaku policzyłby 15 i nie zgłosiłby tego."""
    ones = SIMD[DType.int32, 2](0x11111111, 0x11111111)
    minus = SIMD[DType.int32, 2](-1, -1)  # osiem polbajtow 0xF = -1
    var acc = SIMD[DType.int32, 8](0)
    acc = wmma_iu4_16x16x32(ones, ones, acc)
    dst[0] = acc[0]
    var neg = SIMD[DType.int32, 8](0)
    neg = wmma_iu4_16x16x32(minus, ones, neg)
    dst[1] = neg[0]


def main() raises:
    var ctx = DeviceContext()
    var exact = ctx.enqueue_create_buffer[DType.int32](2)
    ctx.enqueue_function[wmma_i8_exact](exact.unsafe_ptr(), grid_dim=(1,), block_dim=32)
    ctx.synchronize()
    with exact.map_to_host() as host:
        if host[0] != 16 or host[1] != -16:
            raise Error(
                String("wmma int8 liczy zle: ")
                + String(host[0])
                + " (ocz. 16), "
                + String(host[1])
                + " (ocz. -16)"
            )
    print("wmma_i8: przypadki dodatni i ujemny dokladne")

    comptime BLK = 256
    # Jeden wave32 wykonuje kafel 16x16x16, więc siatkę liczymy w falach, a nie
    # w pojedynczych wątkach.
    waves = 84 * 8 * 32
    threads = waves * 32
    grid = threads // BLK

    var sink = ctx.enqueue_create_buffer[DType.int32](threads)
    for _ in range(3):
        ctx.enqueue_function[wmma_i8_throughput](
            sink.unsafe_ptr(), threads, grid_dim=(grid,), block_dim=BLK
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[wmma_i8_throughput](
            sink.unsafe_ptr(), threads, grid_dim=(grid,), block_dim=BLK
        )
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / Float64(ITERS)
    # Kafel 16x16x16 na falę: 4096 MAC-ów, czyli 8192 operacje.
    ops = Float64(waves) * Float64(ROUNDS) * Float64(CHAINS) * 8192.0
    print("wmma int8 16x16x16:", Int(ops / dt / 1e12), "TOPS")

    var fsink = ctx.enqueue_create_buffer[DType.float32](threads)
    for _ in range(3):
        ctx.enqueue_function[wmma_f16_throughput](
            fsink.unsafe_ptr(), threads, grid_dim=(grid,), block_dim=BLK
        )
    ctx.synchronize()
    t1 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[wmma_f16_throughput](
            fsink.unsafe_ptr(), threads, grid_dim=(grid,), block_dim=BLK
        )
    ctx.synchronize()
    dtf = Float64(perf_counter_ns() - t1) / 1e9 / Float64(ITERS)
    print("wmma f16 16x16x16:", Int(ops / dtf / 1e12), "TFLOPS")

    var e2 = ctx.enqueue_create_buffer[DType.int32](2)
    ctx.enqueue_function[wmma_iu4_exact](e2.unsafe_ptr(), grid_dim=(1,), block_dim=32)
    ctx.synchronize()
    with e2.map_to_host() as host:
        print("wmma_iu4 dodatni:", host[0], "(ocz. 32) ujemny:", host[1], "(ocz. -32)")

    var psink = ctx.enqueue_create_buffer[DType.float32](threads)
    for _ in range(3):
        ctx.enqueue_function[wmma_fp8_throughput](
            psink.unsafe_ptr(), threads, grid_dim=(grid,), block_dim=BLK
        )
    ctx.synchronize()
    t2 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[wmma_fp8_throughput](
            psink.unsafe_ptr(), threads, grid_dim=(grid,), block_dim=BLK
        )
    ctx.synchronize()
    dtp = Float64(perf_counter_ns() - t2) / 1e9 / Float64(ITERS)
    print("wmma fp8 16x16x16:", Int(ops / dtp / 1e12), "TFLOPS")

    var qsink = ctx.enqueue_create_buffer[DType.int32](threads)
    for _ in range(3):
        ctx.enqueue_function[wmma_iu4_throughput](
            qsink.unsafe_ptr(), threads, grid_dim=(grid,), block_dim=BLK
        )
    ctx.synchronize()
    t3 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[wmma_iu4_throughput](
            qsink.unsafe_ptr(), threads, grid_dim=(grid,), block_dim=BLK
        )
    ctx.synchronize()
    dtq = Float64(perf_counter_ns() - t3) / 1e9 / Float64(ITERS)
    # Kafel 16x16x32: 8192 MAC-ow na fale, czyli 16384 operacje.
    print("wmma iu4 16x16x32:", Int(ops * 2.0 / dtq / 1e12), "TOPS")

    for_chains = ctx.enqueue_create_buffer[DType.float32](threads)
    for _ in range(3):
        ctx.enqueue_function[wmma_f16_chains[CHAINS16]](
            for_chains.unsafe_ptr(), threads, grid_dim=(grid,), block_dim=BLK
        )
    ctx.synchronize()
    t4 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[wmma_f16_chains[CHAINS16]](
            for_chains.unsafe_ptr(), threads, grid_dim=(grid,), block_dim=BLK
        )
    ctx.synchronize()
    dt4 = Float64(perf_counter_ns() - t4) / 1e9 / Float64(ITERS)
    ops16 = Float64(waves) * Float64(ROUNDS) * Float64(CHAINS16) * 8192.0
    print("wmma f16, 16 akumulatorow:", Int(ops16 / dt4 / 1e12), "TFLOPS")

    for _ in range(3):
        ctx.enqueue_function[wmma_f16_chains[CHAINS4]](
            for_chains.unsafe_ptr(), threads, grid_dim=(grid,), block_dim=BLK
        )
    ctx.synchronize()
    t5 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[wmma_f16_chains[CHAINS4]](
            for_chains.unsafe_ptr(), threads, grid_dim=(grid,), block_dim=BLK
        )
    ctx.synchronize()
    dt5 = Float64(perf_counter_ns() - t5) / 1e9 / Float64(ITERS)
    ops4 = Float64(waves) * Float64(ROUNDS) * Float64(CHAINS4) * 8192.0
    print("wmma f16, 4 akumulatory:", Int(ops4 / dt5 / 1e12), "TFLOPS")
