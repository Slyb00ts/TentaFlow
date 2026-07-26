# =============================================================================
# Plik: bench_transfer.mojo
# Opis: Mierzy realne pasmo transferow host<->GPU (pamiec przypieta) na tej
#       maszynie. To ono wyznacza, ile wag da sie strumieniowac na token przy
#       tieringu modelu wiekszego niz VRAM.
# Przyklad: pixi run mojo run -I . bench-amd/bench_transfer.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns

comptime ITERS = 20


def bench(label: String, ctx: DeviceContext, mb: Int) raises:
    n = mb * 1024 * 1024
    var host = ctx.enqueue_create_host_buffer[DType.uint8](n)
    var dev = ctx.enqueue_create_buffer[DType.uint8](n)
    ctx.synchronize()
    for _ in range(3):
        ctx.enqueue_copy(dev, host)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_copy(dev, host)
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    print(label, mb, "MB H2D ->", Int(Float64(n) / dt / 1e9), "GB/s", Int(dt * 1e6), "us")

    t1 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_copy(host, dev)
    ctx.synchronize()
    dt2 = Float64(perf_counter_ns() - t1) / 1e9 / ITERS
    print(label, mb, "MB D2H ->", Int(Float64(n) / dt2 / 1e9), "GB/s")


def main() raises:
    var ctx = DeviceContext()
    print("arch:", ctx.arch_name())
    bench("blok", ctx, 16)
    bench("blok", ctx, 64)
    bench("blok", ctx, 256)
