# =============================================================================
# Plik: bench_dot2_f16.mojo
# Opis: Sprawdza poprawność i mierzy pułap `v_dot2_f32_f16` na RDNA — dźwignię
#       prefillu f16 na kartach bez jednostki macierzowej.
# Przykład: pixi run mojo run -I . bench-amd/bench_dot2_f16.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.gpu import block_idx, thread_idx, block_dim
from std.time import perf_counter_ns
from src.arch_dot import dot2_f16

comptime ITERS = 20


def dot2_exact(dst: UnsafePointer[Float32, MutAnyOrigin]):
    """Cztery przypadki brzegowe liczone na urządzeniu, weryfikowane na hoście."""
    i = Int(thread_idx.x)
    if i == 0:
        dst[0] = dot2_f16(
            SIMD[DType.float16, 2](1.5, 2.5), SIMD[DType.float16, 2](3.0, 4.0), 1.0
        )
    elif i == 1:
        dst[1] = dot2_f16(
            SIMD[DType.float16, 2](-2.0, 0.5), SIMD[DType.float16, 2](8.0, -16.0), 0.0
        )
    elif i == 2:
        dst[2] = dot2_f16(
            SIMD[DType.float16, 2](0.0, 0.0), SIMD[DType.float16, 2](5.0, 7.0), -3.25
        )
    elif i == 3:
        # 1024 + 0.25: suma wychodzi poza dokładność f16, więc wynik dowodzi, że
        # akumulacja naprawdę odbywa się w f32.
        dst[3] = dot2_f16(
            SIMD[DType.float16, 2](1024.0, 0.25), SIMD[DType.float16, 2](1.0, 1.0), 0.0
        )


# Osiem niezaleznych lancuchow akumulacji — to jest jednoczesnie wymaganie
# projektowe dla kerneli GEMM na RDNA2: mniej niz osiem akumulatorow na watek
# nie wyciagnie z karty wiecej niz polowy pulapu.
comptime CHAINS = 8


def dot2_throughput(sink: UnsafePointer[Float32, MutAnyOrigin], n: Int):
    var acc = InlineArray[Float32, CHAINS](fill=0.0)
    a = SIMD[DType.float16, 2](1.0, 2.0)
    b = SIMD[DType.float16, 2](0.5, 0.25)
    for _ in range(1024):
        comptime for j in range(CHAINS):
            acc[j] = dot2_f16(a, b, acc[j])
    i = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    var s: Float32 = 0.0
    comptime for j in range(CHAINS):
        s += acc[j]
    if i < n:
        sink[i] = s


def main() raises:
    var ctx = DeviceContext()
    var exact = ctx.enqueue_create_buffer[DType.float32](4)
    ctx.enqueue_function[dot2_exact](exact.unsafe_ptr(), grid_dim=(1,), block_dim=64)
    ctx.synchronize()
    expected = SIMD[DType.float32, 4](15.5, -24.0, -3.25, 1024.25)
    with exact.map_to_host() as host:
        for i in range(4):
            if host[i] != expected[i]:
                raise Error(
                    String("dot2_f16 przypadek ")
                    + String(i)
                    + ": "
                    + String(host[i])
                    + " != "
                    + String(expected[i])
                )
    print("dot2_f16: 4/4 przypadki dokladne")

    var sink = ctx.enqueue_create_buffer[DType.float32](1 << 20)
    ctx.synchronize()
    comptime BLK = 256
    threads = 80 * 32 * 256
    grid = threads // BLK
    for _ in range(3):
        ctx.enqueue_function[dot2_throughput](
            sink.unsafe_ptr(), threads, grid_dim=(grid,), block_dim=BLK
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[dot2_throughput](
            sink.unsafe_ptr(), threads, grid_dim=(grid,), block_dim=BLK
        )
    ctx.synchronize()
    dt = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    flops = Float64(threads) * 1024.0 * Float64(CHAINS) * 2.0 * 2.0
    print("f16 dot2:", Int(flops / dt / 1e12), "TFLOPS")
