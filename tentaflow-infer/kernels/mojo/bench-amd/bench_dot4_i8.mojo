# =============================================================================
# Plik: bench_dot4_i8.mojo
# Opis: Mierzy pułap int8 `v_dot4_i32_i8` na RDNA — dźwignię prefillu na AMD.
# Przykład: pixi run mojo run -I . bench-amd/bench_dot4_i8.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.gpu import block_idx, thread_idx, block_dim
from std.time import perf_counter_ns
from std.gpu.intrinsics import inlined_assembly

comptime ITERS = 20

def dot4_throughput(sink: UnsafePointer[Int32, MutAnyOrigin], n: Int):
    var acc0: Int32 = 0
    var acc1: Int32 = 0
    var acc2: Int32 = 0
    var acc3: Int32 = 0
    a: Int32 = 0x01020304
    b: Int32 = 0x04030201
    for _ in range(1024):
        acc0 = inlined_assembly["v_dot4_i32_i8 $0, $1, $2, $0", Int32, constraints="=v,v,v,0"](a, b, acc0)
        acc1 = inlined_assembly["v_dot4_i32_i8 $0, $1, $2, $0", Int32, constraints="=v,v,v,0"](a, b, acc1)
        acc2 = inlined_assembly["v_dot4_i32_i8 $0, $1, $2, $0", Int32, constraints="=v,v,v,0"](a, b, acc2)
        acc3 = inlined_assembly["v_dot4_i32_i8 $0, $1, $2, $0", Int32, constraints="=v,v,v,0"](a, b, acc3)
    i = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    if i < n:
        sink[i] = acc0 + acc1 + acc2 + acc3

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
    ops = Float64(threads) * 1024.0 * 4.0 * 8.0
    print("int8 dot4:", Int(ops / dt / 1e12), "TOPS")
