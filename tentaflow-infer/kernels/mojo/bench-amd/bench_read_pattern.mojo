# =============================================================================
# Plik: bench_read_pattern.mojo
# Opis: Izoluje KOSZT SAMEGO WZORCA DOSTĘPU do wag NVFP4. Blok GGUF ma 36 bajtów,
#       więc szesnastobajtowe odczyty kodów są wyrównane tylko do czterech —
#       ten bench mierzy, ile to kosztuje wobec odczytu ciągłego i wyrównanego.
# Przykład: pixi run mojo run -I . bench-amd/bench_read_pattern.mojo
# =============================================================================
from std.gpu import WARP_SIZE, block_dim, block_idx, thread_idx
from std.gpu.primitives import warp
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns

comptime ROWS_PER_BLOCK = 8
comptime WARMUP = 3
comptime ITERS = 20


def read_gguf_stride(
    y: UnsafePointer[Float32, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Dokładnie wzorzec z GEMV: 16 B kodów spod `base + 4 + half*8`, krok 36 B."""
    lane = Int(thread_idx.x) % WARP_SIZE
    wave = Int(thread_idx.x) // WARP_SIZE
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wave
    if row >= n_rows:
        return
    pairs = n_cols // 32
    row_base = row * (n_cols // 64) * 36
    var acc: Int32 = 0
    var pair = lane
    while pair < pairs:
        block_base = row_base + (pair // 2) * 36
        half = (pair % 2) * 2
        codes = (weights + block_base + 4 + half * 8).load[width=16, alignment=1]()
        acc += codes.cast[DType.int32]().reduce_add()
        pair += WARP_SIZE
    total = warp.sum(Float32(acc))
    if lane == 0:
        y[row] = total


def read_contiguous(
    y: UnsafePointer[Float32, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Ten sam wolumen bajtow, ale odczyty ciagle i wyrownane do 16 B."""
    lane = Int(thread_idx.x) % WARP_SIZE
    wave = Int(thread_idx.x) // WARP_SIZE
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wave
    if row >= n_rows:
        return
    chunks = (n_cols // 64) * 36 // 16
    row_base = row * (n_cols // 64) * 36
    var acc: Int32 = 0
    var chunk = lane
    while chunk < chunks:
        codes = (weights + row_base + chunk * 16).load[width=16, alignment=16]()
        acc += codes.cast[DType.int32]().reduce_add()
        chunk += WARP_SIZE
    total = warp.sum(Float32(acc))
    if lane == 0:
        y[row] = total


def run(ctx: DeviceContext, n_rows: Int, n_cols: Int) raises:
    bytes_total = n_rows * (n_cols // 64) * 36
    var w = ctx.enqueue_create_buffer[DType.uint8](bytes_total + 64)
    var y = ctx.enqueue_create_buffer[DType.float32](n_rows)
    ctx.synchronize()
    grid = (n_rows + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK
    var t0: Int = 0
    var t1: Int = 0
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t0 = perf_counter_ns()
        ctx.enqueue_function[read_gguf_stride](
            y.unsafe_ptr(), w.unsafe_ptr(), n_cols, n_rows,
            grid_dim=(grid,), block_dim=ROWS_PER_BLOCK * 32,
        )
    ctx.synchronize()
    stride_s = Float64(perf_counter_ns() - t0) / 1e9 / Float64(ITERS)
    for i in range(WARMUP + ITERS):
        if i == WARMUP:
            ctx.synchronize()
            t1 = perf_counter_ns()
        ctx.enqueue_function[read_contiguous](
            y.unsafe_ptr(), w.unsafe_ptr(), n_cols, n_rows,
            grid_dim=(grid,), block_dim=ROWS_PER_BLOCK * 32,
        )
    ctx.synchronize()
    contig_s = Float64(perf_counter_ns() - t1) / 1e9 / Float64(ITERS)
    b = Float64(bytes_total)
    print(
        "rows=", n_rows, "cols=", n_cols,
        "| wzorzec GGUF", Int(b / stride_s / 1e9), "GB/s",
        "| ciagly wyrownany", Int(b / contig_s / 1e9), "GB/s",
        "| strata", Int((1.0 - stride_s / contig_s) * -100.0), "%",
    )


def main() raises:
    var ctx = DeviceContext()
    run(ctx, 17408, 5120)
    run(ctx, 5120, 5120)
    run(ctx, 6144, 5120)
    run(ctx, 65536, 5120)
