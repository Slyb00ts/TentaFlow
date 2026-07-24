# =============================================================================
# Plik: test_gemm_q8_triplet_variants.mojo
# Opis: Sprawdza bitową zgodność, strażniki i wydajność wariantów tripletu Q8
#       dla rzeczywistych wymiarów warstwy hybrydowej.
# Przykład: pixi run mojo test_gemm_q8_triplet_variants.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.time import perf_counter_ns
from src.gemm import gemm_q8_0_i8mma_triplet_bm64
from src.gemm_q8_triplet_variants import (
    gemm_q8_0_i8mma_triplet_single_bm64,
    gemm_q8_0_i8mma_triplet_single_big,
)

comptime ROWS0 = 6144
comptime ROWS1 = 48
comptime ROWS2 = 48
comptime COLS = 5120
comptime BLOCKS = COLS // 32
comptime GUARD = 37
comptime GUARD_VALUE = Float16(29.5)
comptime BENCH_ITERS = 20


def _fill_weight(weight: DeviceBuffer[DType.uint8], rows: Int, seed: Int) raises:
    with weight.map_to_host() as host:
        for row in range(rows):
            for block in range(BLOCKS):
                offset = (row * BLOCKS + block) * 34
                scale = Float16(0.002 + Float32((row + block + seed) % 7) * 0.001)
                bits = scale.to_bits()
                host[offset] = UInt8(bits & 0xFF)
                host[offset + 1] = UInt8((bits >> 8) & 0xFF)
                for k in range(32):
                    host[offset + 2 + k] = UInt8(
                        (row * 17 + block * 29 + k * 13 + seed) & 0xFF
                    )


def _fill_prepared(
    xq: DeviceBuffer[DType.int8],
    xd: DeviceBuffer[DType.float32],
    xsm: DeviceBuffer[DType.float32],
    steps: Int,
) raises:
    with xq.map_to_host() as host:
        for i in range(len(host)):
            host[i] = Int8((i * 31 + 11) % 255 - 127)
    with xd.map_to_host() as dh, xsm.map_to_host() as sh:
        for block in range(BLOCKS):
            for token in range(steps):
                index = block * steps + token
                dh[index] = 0.001 + Float32((block * 7 + token * 3) % 17) * 0.0002
                sh[index] = 0.0


def _fill_guard(buffer: DeviceBuffer[DType.float16]) raises:
    with buffer.map_to_host() as host:
        for i in range(len(host)):
            host[i] = GUARD_VALUE


def _check_guard(
    buffer: DeviceBuffer[DType.float16], elements: Int, name: String
) raises:
    with buffer.map_to_host() as host:
        for i in range(GUARD):
            if host[i].to_bits() != GUARD_VALUE.to_bits():
                raise Error("naruszony początkowy guard " + name)
        for i in range(GUARD + elements, len(host)):
            if host[i].to_bits() != GUARD_VALUE.to_bits():
                raise Error("naruszony końcowy guard " + name)


def _compare(
    reference: DeviceBuffer[DType.float16],
    candidate: DeviceBuffer[DType.float16],
    elements: Int,
    name: String,
) raises:
    with reference.map_to_host() as a, candidate.map_to_host() as b:
        for i in range(elements):
            if a[GUARD + i].to_bits() != b[GUARD + i].to_bits():
                raise Error("różnica bitowa " + name + " przy elemencie " + String(i))
    _check_guard(reference, elements, name + " reference")
    _check_guard(candidate, elements, name + " candidate")


def _launch_reference[
    steps: Int
](
    ctx: DeviceContext,
    mut output0: DeviceBuffer[DType.float16], mut output1: DeviceBuffer[DType.float16], mut output2: DeviceBuffer[DType.float16],
    mut weight0: DeviceBuffer[DType.uint8], mut weight1: DeviceBuffer[DType.uint8], mut weight2: DeviceBuffer[DType.uint8],
    mut xq: DeviceBuffer[DType.int8], mut xd: DeviceBuffer[DType.float32], mut xsm: DeviceBuffer[DType.float32],
) raises:
    ctx.enqueue_function[gemm_q8_0_i8mma_triplet_bm64](
        output0.unsafe_ptr() + GUARD, weight0.unsafe_ptr(), ROWS0,
        output1.unsafe_ptr() + GUARD, weight1.unsafe_ptr(), ROWS1,
        output2.unsafe_ptr() + GUARD, weight2.unsafe_ptr(), ROWS2,
        xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), COLS, steps,
        grid_dim=(98, (steps + 63) // 64), block_dim=256,
    )


def _launch_single64[
    steps: Int
](
    ctx: DeviceContext,
    mut output0: DeviceBuffer[DType.float16], mut output1: DeviceBuffer[DType.float16], mut output2: DeviceBuffer[DType.float16],
    weight0: DeviceBuffer[DType.uint8], weight1: DeviceBuffer[DType.uint8], weight2: DeviceBuffer[DType.uint8],
    xq: DeviceBuffer[DType.int8], xd: DeviceBuffer[DType.float32], xsm: DeviceBuffer[DType.float32],
) raises:
    ctx.enqueue_function[gemm_q8_0_i8mma_triplet_single_bm64](
        output0.unsafe_ptr() + GUARD, weight0.unsafe_ptr(), ROWS0,
        output1.unsafe_ptr() + GUARD, weight1.unsafe_ptr(), ROWS1,
        output2.unsafe_ptr() + GUARD, weight2.unsafe_ptr(), ROWS2,
        xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), COLS, steps,
        grid_dim=(98, (steps + 63) // 64), block_dim=256,
    )


def _launch_big[
    steps: Int
](
    ctx: DeviceContext,
    mut output0: DeviceBuffer[DType.float16], mut output1: DeviceBuffer[DType.float16], mut output2: DeviceBuffer[DType.float16],
    weight0: DeviceBuffer[DType.uint8], weight1: DeviceBuffer[DType.uint8], weight2: DeviceBuffer[DType.uint8],
    xq: DeviceBuffer[DType.int8], xd: DeviceBuffer[DType.float32], xsm: DeviceBuffer[DType.float32],
) raises:
    ctx.enqueue_function[gemm_q8_0_i8mma_triplet_single_big](
        output0.unsafe_ptr() + GUARD, weight0.unsafe_ptr(), ROWS0,
        output1.unsafe_ptr() + GUARD, weight1.unsafe_ptr(), ROWS1,
        output2.unsafe_ptr() + GUARD, weight2.unsafe_ptr(), ROWS2,
        xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), COLS, steps,
        grid_dim=(50, (steps + 127) // 128), block_dim=512,
    )


def _case[steps: Int](
    ctx: DeviceContext,
    mut weight0: DeviceBuffer[DType.uint8], mut weight1: DeviceBuffer[DType.uint8], mut weight2: DeviceBuffer[DType.uint8],
) raises:
    var xq = ctx.enqueue_create_buffer[DType.int8](steps * COLS)
    var xd = ctx.enqueue_create_buffer[DType.float32](steps * BLOCKS)
    var xsm = ctx.enqueue_create_buffer[DType.float32](steps * BLOCKS)
    _fill_prepared(xq, xd, xsm, steps)
    var r0 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS0 + 2 * GUARD)
    var r1 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS1 + 2 * GUARD)
    var r2 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS2 + 2 * GUARD)
    var s0 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS0 + 2 * GUARD)
    var s1 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS1 + 2 * GUARD)
    var s2 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS2 + 2 * GUARD)
    var b0 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS0 + 2 * GUARD)
    var b1 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS1 + 2 * GUARD)
    var b2 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS2 + 2 * GUARD)
    _fill_guard(r0)
    _fill_guard(r1)
    _fill_guard(r2)
    _fill_guard(s0)
    _fill_guard(s1)
    _fill_guard(s2)
    _fill_guard(b0)
    _fill_guard(b1)
    _fill_guard(b2)
    _launch_reference[steps](ctx, r0, r1, r2, weight0, weight1, weight2, xq, xd, xsm)
    _launch_single64[steps](ctx, s0, s1, s2, weight0, weight1, weight2, xq, xd, xsm)
    _launch_big[steps](ctx, b0, b1, b2, weight0, weight1, weight2, xq, xd, xsm)
    ctx.synchronize()
    _compare(r0, s0, steps * ROWS0, "single64 gate")
    _compare(r1, s1, steps * ROWS1, "single64 alpha")
    _compare(r2, s2, steps * ROWS2, "single64 beta")
    _compare(r0, b0, steps * ROWS0, "big gate")
    _compare(r1, b1, steps * ROWS1, "big alpha")
    _compare(r2, b2, steps * ROWS2, "big beta")
    print("PASS triplet Q8 T=", steps)


def _benchmark[steps: Int](
    ctx: DeviceContext,
    mut weight0: DeviceBuffer[DType.uint8], mut weight1: DeviceBuffer[DType.uint8], mut weight2: DeviceBuffer[DType.uint8],
) raises:
    var xq = ctx.enqueue_create_buffer[DType.int8](steps * COLS)
    var xd = ctx.enqueue_create_buffer[DType.float32](steps * BLOCKS)
    var xsm = ctx.enqueue_create_buffer[DType.float32](steps * BLOCKS)
    _fill_prepared(xq, xd, xsm, steps)
    var output0 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS0 + 2 * GUARD)
    var output1 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS1 + 2 * GUARD)
    var output2 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS2 + 2 * GUARD)
    _launch_reference[steps](ctx, output0, output1, output2, weight0, weight1, weight2, xq, xd, xsm)
    ctx.synchronize()
    start = perf_counter_ns()
    for _ in range(BENCH_ITERS):
        _launch_reference[steps](ctx, output0, output1, output2, weight0, weight1, weight2, xq, xd, xsm)
    ctx.synchronize()
    reference_ms = Float64(perf_counter_ns() - start) / 1e6 / BENCH_ITERS
    start = perf_counter_ns()
    for _ in range(BENCH_ITERS):
        _launch_single64[steps](ctx, output0, output1, output2, weight0, weight1, weight2, xq, xd, xsm)
    ctx.synchronize()
    single64_ms = Float64(perf_counter_ns() - start) / 1e6 / BENCH_ITERS
    start = perf_counter_ns()
    for _ in range(BENCH_ITERS):
        _launch_big[steps](ctx, output0, output1, output2, weight0, weight1, weight2, xq, xd, xsm)
    ctx.synchronize()
    big_ms = Float64(perf_counter_ns() - start) / 1e6 / BENCH_ITERS
    print("BENCH triplet Q8 T=", steps, " reference=", reference_ms, "ms single64=", single64_ms, "ms big=", big_ms, "ms")


def main() raises:
    var ctx = DeviceContext()
    var weight0 = ctx.enqueue_create_buffer[DType.uint8](ROWS0 * BLOCKS * 34)
    var weight1 = ctx.enqueue_create_buffer[DType.uint8](ROWS1 * BLOCKS * 34)
    var weight2 = ctx.enqueue_create_buffer[DType.uint8](ROWS2 * BLOCKS * 34)
    _fill_weight(weight0, ROWS0, 3)
    _fill_weight(weight1, ROWS1, 7)
    _fill_weight(weight2, ROWS2, 11)
    _case[128](ctx, weight0, weight1, weight2)
    _case[1024](ctx, weight0, weight1, weight2)
    _case[2048](ctx, weight0, weight1, weight2)
    _benchmark[128](ctx, weight0, weight1, weight2)
    _benchmark[1024](ctx, weight0, weight1, weight2)
    _benchmark[2048](ctx, weight0, weight1, weight2)
