# =============================================================================
# Plik: bench_tier_read.mojo
# Opis: Rozstrzyga architekture tieringu wag: czy kernel ma czytac wagi WPROST
#       z pamieci przypietej hosta (przez PCIe), czy lepiej skopiowac warstwe
#       blokowo do VRAM i liczyc na miejscu. Porownuje trzy warianty na
#       ksztalcie warstwy FFN.
# Przyklad: pixi run mojo run -I . bench-amd/bench_tier_read.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.gemv2 import gemv_q4_0_f16_v2
from src.nvfp4 import gemv_nvfp4_gguf_f16

comptime ITERS = 10


def main() raises:
    var ctx = DeviceContext()
    print("arch:", ctx.arch_name())
    # ~400 MB wagi: tyle wazy jedna warstwa modelu 27B.
    rows = 24576
    cols = 8192
    nb = cols // 32
    bytes = rows * nb * 18
    print("waga warstwy:", Int(Float64(bytes) / 1048576.0), "MB")

    var y = ctx.enqueue_create_buffer[DType.float16](rows)
    var x = ctx.enqueue_create_buffer[DType.float16](cols)
    var w_dev = ctx.enqueue_create_buffer[DType.uint8](bytes)
    var w_host = ctx.enqueue_create_host_buffer[DType.uint8](bytes)
    ctx.synchronize()
    grid = (rows + 7) // 8

    # (a) wagi rezydentne w VRAM
    for _ in range(2):
        ctx.enqueue_function[gemv_q4_0_f16_v2](
            y.unsafe_ptr(), w_dev.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256)
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q4_0_f16_v2](
            y.unsafe_ptr(), w_dev.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256)
    ctx.synchronize()
    d_vram = Float64(perf_counter_ns() - t0) / 1e9 / ITERS
    print("a) VRAM rezydentne :", Int(d_vram * 1e6), "us",
          Int(Float64(bytes) / d_vram / 1e9), "GB/s")

    # (b) kernel czyta WPROST z pamieci przypietej hosta
    for _ in range(2):
        ctx.enqueue_function[gemv_q4_0_f16_v2](
            y.unsafe_ptr(), w_host.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256)
    ctx.synchronize()
    t1 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q4_0_f16_v2](
            y.unsafe_ptr(), w_host.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256)
    ctx.synchronize()
    d_host = Float64(perf_counter_ns() - t1) / 1e9 / ITERS
    print("b) wprost z hosta  :", Int(d_host * 1e6), "us",
          Int(Float64(bytes) / d_host / 1e9), "GB/s")

    # (c) kopia blokowa host->VRAM, potem obliczenia na miejscu
    t2 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_copy(w_dev, w_host)
        ctx.enqueue_function[gemv_q4_0_f16_v2](
            y.unsafe_ptr(), w_dev.unsafe_ptr(), x.unsafe_ptr(), cols, rows,
            grid_dim=grid, block_dim=256)
    ctx.synchronize()
    d_copy = Float64(perf_counter_ns() - t2) / 1e9 / ITERS
    print("c) kopia + VRAM    :", Int(d_copy * 1e6), "us",
          Int(Float64(bytes) / d_copy / 1e9), "GB/s")

    # NVFP4 GGUF: 36 B na 64 wartosci, jeden blok roboczy na wiersz.
    nvbytes = rows * (cols // 64) * 36
    var nv_dev = ctx.enqueue_create_buffer[DType.uint8](nvbytes)
    var nv_host = ctx.enqueue_create_host_buffer[DType.uint8](nvbytes)
    ctx.synchronize()
    print("--- NVFP4, waga", Int(Float64(nvbytes) / 1048576.0), "MB")
    for _ in range(2):
        ctx.enqueue_function[gemv_nvfp4_gguf_f16](
            y.unsafe_ptr(), nv_dev.unsafe_ptr(), x.unsafe_ptr(), cols, Float32(1.0),
            grid_dim=rows, block_dim=256)
    ctx.synchronize()
    t3 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_nvfp4_gguf_f16](
            y.unsafe_ptr(), nv_dev.unsafe_ptr(), x.unsafe_ptr(), cols, Float32(1.0),
            grid_dim=rows, block_dim=256)
    ctx.synchronize()
    dn_v = Float64(perf_counter_ns() - t3) / 1e9 / ITERS
    print("d) NVFP4 z VRAM    :", Int(dn_v * 1e6), "us",
          Int(Float64(nvbytes) / dn_v / 1e9), "GB/s")

    for _ in range(2):
        ctx.enqueue_function[gemv_nvfp4_gguf_f16](
            y.unsafe_ptr(), nv_host.unsafe_ptr(), x.unsafe_ptr(), cols, Float32(1.0),
            grid_dim=rows, block_dim=256)
    ctx.synchronize()
    t4 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_nvfp4_gguf_f16](
            y.unsafe_ptr(), nv_host.unsafe_ptr(), x.unsafe_ptr(), cols, Float32(1.0),
            grid_dim=rows, block_dim=256)
    ctx.synchronize()
    dn_h = Float64(perf_counter_ns() - t4) / 1e9 / ITERS
    print("e) NVFP4 z hosta   :", Int(dn_h * 1e6), "us",
          Int(Float64(nvbytes) / dn_h / 1e9), "GB/s")

    t5 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_copy(nv_dev, nv_host)
        ctx.enqueue_function[gemv_nvfp4_gguf_f16](
            y.unsafe_ptr(), nv_dev.unsafe_ptr(), x.unsafe_ptr(), cols, Float32(1.0),
            grid_dim=rows, block_dim=256)
    ctx.synchronize()
    dn_c = Float64(perf_counter_ns() - t5) / 1e9 / ITERS
    print("f) NVFP4 kopia+VRAM:", Int(dn_c * 1e6), "us",
          Int(Float64(nvbytes) / dn_c / 1e9), "GB/s")
