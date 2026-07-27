# =============================================================================
# Plik: bench_dot4_i8.mojo
# Opis: Sprawdza poprawność i mierzy pułap int8 `dot4_i8` na RDNA — dźwignię
#       prefillu na AMD.
# Przykład: pixi run mojo run -I . bench-amd/bench_dot4_i8.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.gpu import block_idx, thread_idx, block_dim
from std.time import perf_counter_ns
from src.arch_dot import dot4_i8

comptime ITERS = 20
# Osiem niezaleznych lancuchow akumulacji — ponizej tego progu pomiar jest
# ograniczony latencja VALU, nie przepustowoscia (przy 4 wychodzi dokladnie
# polowa pulapu).
comptime CHAINS = 8


def dot4_exact(dst: UnsafePointer[Int32, MutAnyOrigin]):
    """Przypadki brzegowe ze ZNAKIEM — sam pomiar przepustowości ich nie tknie.

    Bez ujemnego bajtu ten helper przechodził na RDNA3 mimo że instrukcja
    liczyła bez znaku, więc dodatnie wejścia niczego nie dowodzą.
    """
    i = Int(thread_idx.x)
    if i == 0:
        dst[0] = dot4_i8(0x04030201, 0x01020304, 0)
    elif i == 1:
        dst[1] = dot4_i8(Int32(-50462977), 0x01020304, 0)  # 0xFCFDFEFF
    elif i == 2:
        dst[2] = dot4_i8(0x7F7F7F7F, 0x7F7F7F7F, 100)
    elif i == 3:
        dst[3] = dot4_i8(Int32(-2139062144), 0x01010101, 0)  # 0x80808080


def dot4_throughput(sink: UnsafePointer[Int32, MutAnyOrigin], n: Int):
    var acc = InlineArray[Int32, CHAINS](fill=0)
    a: Int32 = 0x01020304
    b: Int32 = 0x04030201
    for _ in range(1024):
        comptime for j in range(CHAINS):
            acc[j] = dot4_i8(a, b, acc[j])
    i = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    var s: Int32 = 0
    comptime for j in range(CHAINS):
        s += acc[j]
    if i < n:
        sink[i] = s

def main() raises:
    var ctx = DeviceContext()
    var exact = ctx.enqueue_create_buffer[DType.int32](4)
    ctx.enqueue_function[dot4_exact](exact.unsafe_ptr(), grid_dim=(1,), block_dim=64)
    ctx.synchronize()
    expected = SIMD[DType.int32, 4](20, -20, 64616, -512)
    with exact.map_to_host() as host:
        for i in range(4):
            if host[i] != expected[i]:
                raise Error(
                    String("dot4_i8 przypadek ")
                    + String(i)
                    + ": "
                    + String(host[i])
                    + " != "
                    + String(expected[i])
                )
    print("dot4_i8: 4/4 przypadki dokladne")

    var sink = ctx.enqueue_create_buffer[DType.int32](1 << 20)
    ctx.synchronize()
    comptime BLK = 256
    threads = 80 * 32 * 256
    grid = threads // BLK
    for _ in range(3):
        ctx.enqueue_function[dot4_throughput](sink.unsafe_ptr(), threads, grid_dim=(grid,), block_dim=BLK)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[dot4_throughput](sink.unsafe_ptr(), threads, grid_dim=(grid,), block_dim=BLK)
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    ops = Float64(threads) * 1024.0 * Float64(CHAINS) * 4.0 * 2.0
    print("int8 dot4:", Int(ops / dt / 1e12), "TOPS")
