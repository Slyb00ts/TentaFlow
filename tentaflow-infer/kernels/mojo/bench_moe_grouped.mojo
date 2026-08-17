# ===== File: bench_moe_grouped.mojo — grouped expert GEMM at decode widths =====
# A decode batch routes eight lanes into roughly eighteen distinct experts, so a
# grouped tile owns three or four rows of a tile built for sixty-four. This times
# the wide and the narrow tile on that exact shape and reports achieved weight
# bandwidth, which is the only figure that says whether the tile is short of
# memory or short of work between its barriers.
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.gemm import gemm_q4_k_i8mma_grouped
from src.decode_dp4a import gemv_q4_k_dp4a_f16_gidx_batch
from src.gemm import quantize_act_q8_1


def _first(t: Int) -> Int:
    var at = 0
    for i in range(t):
        at += 6 if i < 4 else (3 if i < 10 else 2)
    return at


def _run(ctx: DeviceContext, K: Int, N: Int, TILES: Int, SEL: Int) raises:
    comptime BLK = 144
    row_bytes = (K // 256) * BLK
    expert_bytes = N * row_bytes
    var w = ctx.enqueue_create_buffer[DType.uint8](TILES * expert_bytes)
    var tab = ctx.enqueue_create_buffer[DType.uint64](TILES)
    var y = ctx.enqueue_create_buffer[DType.float16](SEL * N)
    var x = ctx.enqueue_create_buffer[DType.float16](SEL * K)
    var xq = ctx.enqueue_create_buffer[DType.int8](SEL * K)
    var xd = ctx.enqueue_create_buffer[DType.float32](SEL * (K // 32))
    var xsm = ctx.enqueue_create_buffer[DType.float32](SEL * (K // 32))
    var te = ctx.enqueue_create_buffer[DType.int32](TILES)
    var tf = ctx.enqueue_create_buffer[DType.int32](TILES)
    var tl = ctx.enqueue_create_buffer[DType.int32](TILES)

    base = Int(w.unsafe_ptr())
    with tab.map_to_host() as h:
        for e in range(TILES):
            h[e] = UInt64(base + e * expert_bytes)
    with x.map_to_host() as h:
        for i in range(SEL * K):
            h[i] = Float16(Float64((i % 13) - 6) * 0.05)
    with w.map_to_host() as h:
        for i in range(TILES * expert_bytes):
            h[i] = UInt8(i % 251)

    # Selections spread over the experts the way a real router spreads them:
    # a few popular ones and a long tail of singles.
    with te.map_to_host() as e, tf.map_to_host() as f, tl.map_to_host() as l:
        var at = 0
        for t in range(TILES):
            var take = 6 if t < 4 else (3 if t < 10 else 2)
            if at + take > SEL:
                take = SEL - at
            e[t] = Int32(t)
            f[t] = Int32(at)
            l[t] = Int32(at + take)
            at += take

    nbq = (SEL * (K // 32) + 255) // 256
    ctx.enqueue_function[quantize_act_q8_1](
        xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), x.unsafe_ptr(),
        K, SEL, grid_dim=nbq, block_dim=256,
    )
    ctx.synchronize()

    comptime ITERS = 200
    bytes_read = Float64(TILES) * Float64(expert_bytes)

    for _ in range(30):
        ctx.enqueue_function[gemm_q4_k_i8mma_grouped](
            y.unsafe_ptr(), tab.unsafe_ptr().bitcast[UnsafePointer[UInt8, MutAnyOrigin]](),
            xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(),
            te.unsafe_ptr(), tf.unsafe_ptr(), tl.unsafe_ptr(),
            K, N, SEL, grid_dim=(N // 128, TILES), block_dim=256,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q4_k_i8mma_grouped](
            y.unsafe_ptr(), tab.unsafe_ptr().bitcast[UnsafePointer[UInt8, MutAnyOrigin]](),
            xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(),
            te.unsafe_ptr(), tf.unsafe_ptr(), tl.unsafe_ptr(),
            K, N, SEL, grid_dim=(N // 64, TILES), block_dim=256,
        )
    ctx.synchronize()
    w64 = Float64(perf_counter_ns() - t0) / Float64(ITERS)

    # llama.cpp's shape for the same work: one block per (row-block, selection),
    # the expert read from ids on device, no staging and no grouping — the
    # duplicate expert reads are left to the cache.
    var xh = ctx.enqueue_create_buffer[DType.float16](SEL * K)
    var yg = ctx.enqueue_create_buffer[DType.float16](SEL * N)
    var idb = ctx.enqueue_create_buffer[DType.int32](SEL)
    with idb.map_to_host() as h:
        for t in range(TILES):
            var take = 6 if t < 4 else (3 if t < 10 else 2)
            for j in range(take):
                idx = _first(t) + j
                if idx < SEL:
                    h[idx] = Int32(t)
    for _ in range(30):
        ctx.enqueue_function[gemv_q4_k_dp4a_f16_gidx_batch](
            yg.unsafe_ptr(), tab.unsafe_ptr().bitcast[UnsafePointer[UInt8, MutAnyOrigin]](),
            xh.unsafe_ptr(), K, N, idb.unsafe_ptr(), 8,
            grid_dim=((N + 7) // 8, SEL), block_dim=256,
        )
    ctx.synchronize()
    t2 = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemv_q4_k_dp4a_f16_gidx_batch](
            yg.unsafe_ptr(), tab.unsafe_ptr().bitcast[UnsafePointer[UInt8, MutAnyOrigin]](),
            xh.unsafe_ptr(), K, N, idb.unsafe_ptr(), 8,
            grid_dim=((N + 7) // 8, SEL), block_dim=256,
        )
    ctx.synchronize()
    wg = Float64(perf_counter_ns() - t2) / Float64(ITERS)

    print(
        "K", K, "N", N, "tiles", TILES, "sel", SEL,
        "| grouped", Int(w64 / 1000.0), "us", Int(bytes_read / w64), "GB/s",
        "| gidx", Int(wg / 1000.0), "us", Int(bytes_read / wg), "GB/s",
        "| speedup", w64 / wg,
    )


def main() raises:
    ctx = DeviceContext()
    _run(ctx, 2048, 768, 18, 64)
    _run(ctx, 2048, 768, 24, 64)
    _run(ctx, 2048, 768, 12, 32)
    _run(ctx, 768, 2048, 18, 64)
