# =============================================================================
# Plik: bench_q8_single_big_poststage.mojo
# Opis: Porownuje Q8 single_big i poststage dla T1024/T2048.
# Przyklad: pixi run mojo bench_q8_single_big_poststage.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.time import perf_counter_ns
from test_q8_single_big_poststage import (
    ROWS0, ROWS1, ROWS2, COLS, BLOCKS, GUARD,
    _fill_weight, _fill_prepared, _launch_reference, _launch_candidate,
)

comptime WARMUP = 5
comptime ITERATIONS = 10
comptime ROUNDS = 5


def _bench[steps: Int](
    ctx: DeviceContext,
    w0: DeviceBuffer[DType.uint8],
    w1: DeviceBuffer[DType.uint8],
    w2: DeviceBuffer[DType.uint8],
) raises:
    var xq = ctx.enqueue_create_buffer[DType.int8](steps * COLS)
    var xd = ctx.enqueue_create_buffer[DType.float32](steps * BLOCKS)
    var xsm = ctx.enqueue_create_buffer[DType.float32](steps * BLOCKS)
    _fill_prepared(xq, xd, steps)
    var y0 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS0 + 2 * GUARD)
    var y1 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS1 + 2 * GUARD)
    var y2 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS2 + 2 * GUARD)
    for _ in range(WARMUP):
        _launch_reference[steps](ctx, y0, y1, y2, w0, w1, w2, xq, xd, xsm)
        _launch_candidate[steps](ctx, y0, y1, y2, w0, w1, w2, xq, xd, xsm)
    ctx.synchronize()

    var reference = InlineArray[Float64, ROUNDS](fill=0.0)
    var candidate = InlineArray[Float64, ROUNDS](fill=0.0)
    for round in range(ROUNDS):
        var started = perf_counter_ns()
        if round % 2 == 0:
            for _ in range(ITERATIONS):
                _launch_reference[steps](ctx, y0, y1, y2, w0, w1, w2, xq, xd, xsm)
            ctx.synchronize()
            reference[round] = Float64(perf_counter_ns() - started) / 1e6 / ITERATIONS
            started = perf_counter_ns()
            for _ in range(ITERATIONS):
                _launch_candidate[steps](ctx, y0, y1, y2, w0, w1, w2, xq, xd, xsm)
            ctx.synchronize()
            candidate[round] = Float64(perf_counter_ns() - started) / 1e6 / ITERATIONS
        else:
            for _ in range(ITERATIONS):
                _launch_candidate[steps](ctx, y0, y1, y2, w0, w1, w2, xq, xd, xsm)
            ctx.synchronize()
            candidate[round] = Float64(perf_counter_ns() - started) / 1e6 / ITERATIONS
            started = perf_counter_ns()
            for _ in range(ITERATIONS):
                _launch_reference[steps](ctx, y0, y1, y2, w0, w1, w2, xq, xd, xsm)
            ctx.synchronize()
            reference[round] = Float64(perf_counter_ns() - started) / 1e6 / ITERATIONS
        print(
            "T", steps, "round", round + 1, "single_big_ms", reference[round],
            "poststage_ms", candidate[round], "speedup", reference[round] / candidate[round],
        )
    for i in range(ROUNDS):
        for j in range(i + 1, ROUNDS):
            if reference[j] < reference[i]:
                reference[i], reference[j] = reference[j], reference[i]
            if candidate[j] < candidate[i]:
                candidate[i], candidate[j] = candidate[j], candidate[i]
    print(
        "MEDIAN T", steps, "single_big_ms", reference[ROUNDS // 2],
        "poststage_ms", candidate[ROUNDS // 2],
        "speedup", reference[ROUNDS // 2] / candidate[ROUNDS // 2],
    )


def main() raises:
    var ctx = DeviceContext()
    var w0 = ctx.enqueue_create_buffer[DType.uint8](ROWS0 * BLOCKS * 34)
    var w1 = ctx.enqueue_create_buffer[DType.uint8](ROWS1 * BLOCKS * 34)
    var w2 = ctx.enqueue_create_buffer[DType.uint8](ROWS2 * BLOCKS * 34)
    _fill_weight(w0, ROWS0, 3)
    _fill_weight(w1, ROWS1, 7)
    _fill_weight(w2, ROWS2, 11)
    _bench[1024](ctx, w0, w1, w2)
    _bench[2048](ctx, w0, w1, w2)
