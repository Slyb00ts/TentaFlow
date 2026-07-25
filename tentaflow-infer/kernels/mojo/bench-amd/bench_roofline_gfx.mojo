# =============================================================================
# Plik: bench_roofline_gfx.mojo
# Opis: Mierzy pasmo HBM i przepustowość FP32 FMA karty AMD przez ścieżkę Mojo.
# Przykład: pixi run mojo run -I . bench-amd/bench_roofline_gfx.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.gpu import block_idx, thread_idx, block_dim
from std.time import perf_counter_ns

# PUŁAPKA: 256 MiB to dokładnie granica 128 MB Infinity Cache tej karty, więc
# pomiar na takim buforze mieszał cache z DRAM i dawał niestabilne 386-395 GB/s.
# Realny stream mierzymy na 1 GiB, a pojemność cache raportujemy osobno.
comptime N = 1 << 28          # 256M f32 = 1 GiB
comptime N_CACHE = 1 << 24    # 16M f32 = 64 MiB, mieści się w Infinity Cache
comptime ITERS = 20

def stream_read(dst: UnsafePointer[Float32, MutAnyOrigin], src: UnsafePointer[Float32, MutAnyOrigin], n: Int):
    i = (Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)) * 4
    if i + 3 < n:
        v = (src + i).load[width=4]()
        s = v[0] + v[1] + v[2] + v[3]
        if s == 1.2345e30:
            dst[0] = s

# Osiem niezaleznych lancuchow: VALU RDNA2 ma kilkutaktowa latencje wyniku, wiec
# ponizej ~8 lancuchow na watek pomiar pokazuje polowe pulapu karty, a nie pulap.
comptime CHAINS = 8


def fma_throughput(sink: UnsafePointer[Float32, MutAnyOrigin], n: Int):
    var a = SIMD[DType.float32, 4](1.0001, 1.0002, 1.0003, 1.0004)
    var b = SIMD[DType.float32, 4](0.9999, 0.9998, 0.9997, 0.9996)
    var acc = InlineArray[SIMD[DType.float32, 4], CHAINS](fill=SIMD[DType.float32, 4](0.0))
    for _ in range(1024):
        comptime for j in range(CHAINS):
            acc[j] = acc[j] * a + b
    i = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    var s = SIMD[DType.float32, 4](0.0)
    comptime for j in range(CHAINS):
        s += acc[j]
    if i < n:
        sink[i] = s[0] + s[1] + s[2] + s[3]

def main() raises:
    var ctx = DeviceContext()
    print("arch:", ctx.arch_name())
    var src = ctx.enqueue_create_buffer[DType.float32](N)
    var dst = ctx.enqueue_create_buffer[DType.float32](N)
    ctx.synchronize()

    comptime BLK = 256
    grid = (N // 4) // BLK
    for _ in range(3):
        ctx.enqueue_function[stream_read](dst.unsafe_ptr(), src.unsafe_ptr(), N, grid_dim=(grid,), block_dim=BLK)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[stream_read](dst.unsafe_ptr(), src.unsafe_ptr(), N, grid_dim=(grid,), block_dim=BLK)
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    print("odczyt DRAM (1 GiB):", Int(Float64(N * 4) / dt / 1e9), "GB/s")

    cgrid = (N_CACHE // 4) // BLK
    for _ in range(3):
        ctx.enqueue_function[stream_read](dst.unsafe_ptr(), src.unsafe_ptr(), N_CACHE, grid_dim=(cgrid,), block_dim=BLK)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[stream_read](dst.unsafe_ptr(), src.unsafe_ptr(), N_CACHE, grid_dim=(cgrid,), block_dim=BLK)
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    print("odczyt z Infinity Cache (64 MiB):", Int(Float64(N_CACHE * 4) / dt / 1e9), "GB/s")

    threads = 80 * 32 * 256
    fgrid = threads // BLK
    for _ in range(3):
        ctx.enqueue_function[fma_throughput](dst.unsafe_ptr(), threads, grid_dim=(fgrid,), block_dim=BLK)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[fma_throughput](dst.unsafe_ptr(), threads, grid_dim=(fgrid,), block_dim=BLK)
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    flops = Float64(threads) * 1024.0 * Float64(CHAINS) * 4.0 * 2.0
    print("FP32 FMA:", Int(flops / dt / 1e12), "TFLOPS")
