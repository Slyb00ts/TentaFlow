# =============================================================================
# Plik: bench_q8_decode_b1_probe.mojo
# Opis: Porownuje dekod Q8 DP4A z lokalna kwantyzacja i wariant B1 z
#       jednorazowo przygotowana aktywacja dla rzeczywistych ksztaltow modelu.
# Przyklad: pixi run mojo bench_q8_decode_b1_probe.mojo
# =============================================================================

from std.gpu import WARP_SIZE
from std.gpu.host import DeviceBuffer, DeviceContext
from std.time import perf_counter_ns
from src.decode_dp4a import gemv_q8_0_dp4a_f16
from src.gemm import quantize_act_q8_1
from src.q8_0_batch import gemm_q8_0_small_dp4a_impl

comptime GUARD = 37
comptime GUARD_VALUE = Float16(29.5)
comptime WARMUP = 20
comptime ITERATIONS = 200
comptime ROUNDS = 7
comptime WEIGHT_RING = 8
comptime q8_dp4a_b1 = gemm_q8_0_small_dp4a_impl[DType.float16, 1]


def _fill_weights(
    weights: DeviceBuffer[DType.uint8], rows: Int, cols: Int, copies: Int
) raises:
    blocks = cols // 32
    with weights.map_to_host() as values:
        for block in range(copies * rows * blocks):
            offset = block * 34
            bits = Float16(
                0.00390625 + Float32(block % 13) * 0.001953125
            ).to_bits()
            values[offset] = UInt8(bits & 0xFF)
            values[offset + 1] = UInt8(bits >> 8)
            for element in range(32):
                values[offset + 2 + element] = UInt8(
                    (block * 29 + element * 17 + 11) & 0xFF
                )


def _fill_input(x: DeviceBuffer[DType.float16]) raises:
    with x.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(
                Float32((i * 19 + 7) % 61 - 30) * 0.015625
            )


def _fill_guard(output: DeviceBuffer[DType.float16]) raises:
    with output.map_to_host() as values:
        for i in range(len(values)):
            values[i] = GUARD_VALUE


def _check_parity(
    reference: DeviceBuffer[DType.float16],
    candidate: DeviceBuffer[DType.float16],
    rows: Int,
) raises:
    with reference.map_to_host() as expected, candidate.map_to_host() as actual:
        for i in range(GUARD):
            if expected[i].to_bits() != GUARD_VALUE.to_bits():
                raise Error("naruszony poczatkowy canary referencji")
            if actual[i].to_bits() != GUARD_VALUE.to_bits():
                raise Error("naruszony poczatkowy canary wariantu B1")
        for i in range(rows):
            if expected[GUARD + i].to_bits() != actual[GUARD + i].to_bits():
                raise Error("wariant B1 nie jest bitowo zgodny")
        for i in range(GUARD + rows, rows + 2 * GUARD):
            if expected[i].to_bits() != GUARD_VALUE.to_bits():
                raise Error("naruszony koncowy canary referencji")
            if actual[i].to_bits() != GUARD_VALUE.to_bits():
                raise Error("naruszony koncowy canary wariantu B1")


def _launch_reference[
    rows: Int, cols: Int
](
    ctx: DeviceContext,
    mut output: DeviceBuffer[DType.float16],
    mut weights: DeviceBuffer[DType.uint8],
    mut x: DeviceBuffer[DType.float16],
    weight_offset: Int,
) raises:
    ctx.enqueue_function[gemv_q8_0_dp4a_f16](
        output.unsafe_ptr() + GUARD,
        weights.unsafe_ptr() + weight_offset,
        x.unsafe_ptr(),
        cols,
        rows,
        grid_dim=(rows + 7) // 8,
        block_dim=8 * WARP_SIZE,
    )


def _launch_candidate[
    rows: Int, cols: Int
](
    ctx: DeviceContext,
    mut output: DeviceBuffer[DType.float16],
    mut weights: DeviceBuffer[DType.uint8],
    mut xq: DeviceBuffer[DType.int8],
    mut xd: DeviceBuffer[DType.float32],
    weight_offset: Int,
) raises:
    ctx.enqueue_function[q8_dp4a_b1](
        output.unsafe_ptr() + GUARD,
        weights.unsafe_ptr() + weight_offset,
        xq.unsafe_ptr(),
        xd.unsafe_ptr(),
        cols,
        rows,
        1,
        grid_dim=(rows + 3) // 4,
        block_dim=4 * WARP_SIZE,
    )


def _launch_quant[
    cols: Int
](
    ctx: DeviceContext,
    mut xq: DeviceBuffer[DType.int8],
    mut xd: DeviceBuffer[DType.float32],
    mut xsm: DeviceBuffer[DType.float32],
    mut x: DeviceBuffer[DType.float16],
) raises:
    ctx.enqueue_function[quantize_act_q8_1](
        xq.unsafe_ptr(),
        xd.unsafe_ptr(),
        xsm.unsafe_ptr(),
        x.unsafe_ptr(),
        cols,
        1,
        grid_dim=((cols // 32) + 255) // 256,
        block_dim=256,
    )


def _case[rows: Int, cols: Int](ctx: DeviceContext) raises:
    blocks = cols // 32
    weight_bytes = rows * blocks * 34
    var weights = ctx.enqueue_create_buffer[DType.uint8](
        WEIGHT_RING * weight_bytes
    )
    var x = ctx.enqueue_create_buffer[DType.float16](cols)
    var xq = ctx.enqueue_create_buffer[DType.int8](cols)
    var xd = ctx.enqueue_create_buffer[DType.float32](blocks)
    var xsm = ctx.enqueue_create_buffer[DType.float32](blocks)
    var reference = ctx.enqueue_create_buffer[DType.float16](rows + 2 * GUARD)
    var candidate = ctx.enqueue_create_buffer[DType.float16](rows + 2 * GUARD)
    _fill_weights(weights, rows, cols, WEIGHT_RING)
    _fill_input(x)
    _fill_guard(reference)
    _fill_guard(candidate)

    _launch_quant[cols](ctx, xq, xd, xsm, x)
    _launch_reference[rows, cols](ctx, reference, weights, x, 0)
    _launch_candidate[rows, cols](ctx, candidate, weights, xq, xd, 0)
    ctx.synchronize()
    _check_parity(reference, candidate, rows)
    print("PARITY", rows, "x", cols, "PASS")

    for iteration in range(WARMUP):
        offset = (iteration % WEIGHT_RING) * weight_bytes
        _launch_reference[rows, cols](ctx, reference, weights, x, offset)
        _launch_candidate[rows, cols](
            ctx, candidate, weights, xq, xd, offset
        )
        _launch_quant[cols](ctx, xq, xd, xsm, x)
    ctx.synchronize()

    var reference_ms = InlineArray[Float64, ROUNDS](fill=0.0)
    var candidate_ms = InlineArray[Float64, ROUNDS](fill=0.0)
    var quant_ms = InlineArray[Float64, ROUNDS](fill=0.0)
    for round in range(ROUNDS):
        var started = perf_counter_ns()
        for iteration in range(ITERATIONS):
            offset = (iteration % WEIGHT_RING) * weight_bytes
            _launch_reference[rows, cols](
                ctx, reference, weights, x, offset
            )
        ctx.synchronize()
        reference_ms[round] = (
            Float64(perf_counter_ns() - started) / 1e6 / ITERATIONS
        )

        started = perf_counter_ns()
        for iteration in range(ITERATIONS):
            offset = (iteration % WEIGHT_RING) * weight_bytes
            _launch_candidate[rows, cols](
                ctx, candidate, weights, xq, xd, offset
            )
        ctx.synchronize()
        candidate_ms[round] = (
            Float64(perf_counter_ns() - started) / 1e6 / ITERATIONS
        )

        started = perf_counter_ns()
        for _ in range(ITERATIONS):
            _launch_quant[cols](ctx, xq, xd, xsm, x)
        ctx.synchronize()
        quant_ms[round] = (
            Float64(perf_counter_ns() - started) / 1e6 / ITERATIONS
        )

    for i in range(ROUNDS):
        for j in range(i + 1, ROUNDS):
            if reference_ms[j] < reference_ms[i]:
                reference_ms[i], reference_ms[j] = (
                    reference_ms[j], reference_ms[i]
                )
            if candidate_ms[j] < candidate_ms[i]:
                candidate_ms[i], candidate_ms[j] = (
                    candidate_ms[j], candidate_ms[i]
                )
            if quant_ms[j] < quant_ms[i]:
                quant_ms[i], quant_ms[j] = quant_ms[j], quant_ms[i]

    median = ROUNDS // 2
    bytes_gb = Float64(weight_bytes) / 1e9
    reference_gbps = bytes_gb / (reference_ms[median] / 1e3)
    candidate_gbps = bytes_gb / (candidate_ms[median] / 1e3)
    amortized_ms = candidate_ms[median] + quant_ms[median]
    print(
        "MEDIAN", rows, "x", cols,
        "reference_ms", reference_ms[median],
        "candidate_ms", candidate_ms[median],
        "quant_ms", quant_ms[median],
        "amortized_ms", amortized_ms,
        "kernel_speedup", reference_ms[median] / candidate_ms[median],
        "amortized_speedup", reference_ms[median] / amortized_ms,
        "reference_GBps", reference_gbps,
        "candidate_GBps", candidate_gbps,
    )


def main() raises:
    var ctx = DeviceContext()
    _case[5120, 6144](ctx)
    _case[6144, 5120](ctx)
