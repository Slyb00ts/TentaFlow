# =============================================================================
# Plik: bench_dot4_i8.mojo
# Opis: Mierzy pułap int8 `v_dot4_i32_i8` na RDNA — dźwignię prefillu na AMD.
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
