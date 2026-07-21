# =============================================================================
# Plik: test_q8_0_batch.mojo
# Opis: Golden i benchmark weight-stationary GEMM Q8_0 dla T=2/3/4.
# Przyklad: pixi run mojo test_q8_0_batch.mojo
# =============================================================================

from std.gpu import WARP_SIZE
from std.gpu.host import DeviceContext
from std.time import perf_counter_ns
from src.gemv2 import gemv_q8_0_f16_v2
from src.gemm import gemm_q8_0_i8mma_bm64, gemm_q8_0_out_f32, quantize_act_q8_1
from src.q8_0_batch import gemm_q8_0_i8mma_b2, gemm_q8_0_i8mma_b3, gemm_q8_0_i8mma_b4
from src.q8_0_batch import gemm_q8_0_i8mma_out_f32_b3, gemm_q8_0_i8mma_out_f32_b4
from src.sampling import argmax_batched_f32

comptime ROWS_PER_BLOCK = 8


def _fill(i: Int) -> Float32:
    seed = (UInt32(i) * 2654435761 + 1013904223) & 0xFFFFFFFF
    return Float32(seed) * (2.0 / 4294967296.0) - 1.0


def _quant_block(
    xh: UnsafePointer[Float16, MutUntrackedOrigin], base: Int
) -> Tuple[InlineArray[Int32, 32], Float32]:
    var quant = InlineArray[Int32, 32](fill=0)
    var amax: Float32 = 0.0
    for k in range(32):
        value = abs(Float32(xh[base + k]))
        if value > amax:
            amax = value
    if amax == 0.0:
        return (quant, Float32(0.0))
    scale = amax * (1.0 / 127.0)
    for k in range(32):
        quant[k] = Int32(round(Float32(xh[base + k]) * (127.0 / amax)))
    return (quant, scale)


def _golden(ctx: DeviceContext) raises:
    comptime TOKENS = 4
    comptime ROWS = 11
    comptime COLS = 192
    var weights = ctx.enqueue_create_buffer[DType.uint8](ROWS * (COLS // 32) * 34)
    var x = ctx.enqueue_create_buffer[DType.float16](TOKENS * COLS)
    var y2 = ctx.enqueue_create_buffer[DType.float16](2 * ROWS)
    var y3 = ctx.enqueue_create_buffer[DType.float16](3 * ROWS)
    var y4 = ctx.enqueue_create_buffer[DType.float16](4 * ROWS)
    var y3f = ctx.enqueue_create_buffer[DType.float32](3 * ROWS)
    var y4f = ctx.enqueue_create_buffer[DType.float32](4 * ROWS)
    var xq2 = ctx.enqueue_create_buffer[DType.int8](2 * COLS)
    var xd2 = ctx.enqueue_create_buffer[DType.float32](2 * (COLS // 32))
    var xsm2 = ctx.enqueue_create_buffer[DType.float32](2 * (COLS // 32))
    var xq3 = ctx.enqueue_create_buffer[DType.int8](3 * COLS)
    var xd3 = ctx.enqueue_create_buffer[DType.float32](3 * (COLS // 32))
    var xsm3 = ctx.enqueue_create_buffer[DType.float32](3 * (COLS // 32))
    var xq4 = ctx.enqueue_create_buffer[DType.int8](4 * COLS)
    var xd4 = ctx.enqueue_create_buffer[DType.float32](4 * (COLS // 32))
    var xsm4 = ctx.enqueue_create_buffer[DType.float32](4 * (COLS // 32))
    with weights.map_to_host() as values:
        for row in range(ROWS):
            for block in range(COLS // 32):
                offset = (row * (COLS // 32) + block) * 34
                scale = Float16(0.02 + Float32((row + block) % 5) * 0.01)
                bits = scale.to_bits()
                values[offset] = UInt8(bits & 0xFF)
                values[offset + 1] = UInt8((bits >> 8) & 0xFF)
                for k in range(32):
                    values[offset + 2 + k] = UInt8(
                        (Int((row * 31 + block * 17 + k * 13) % 255) - 127) & 0xFF
                    )
    with x.map_to_host() as values:
        for i in range(TOKENS * COLS):
            values[i] = Float16(_fill(i))

    ctx.enqueue_function[quantize_act_q8_1](
        xq2.unsafe_ptr(), xd2.unsafe_ptr(), xsm2.unsafe_ptr(), x.unsafe_ptr(),
        COLS, 2, grid_dim=(2 * (COLS // 32) + 255) // 256,
        block_dim=256,
    )
    ctx.enqueue_function[quantize_act_q8_1](
        xq3.unsafe_ptr(), xd3.unsafe_ptr(), xsm3.unsafe_ptr(), x.unsafe_ptr(),
        COLS, 3, grid_dim=(3 * (COLS // 32) + 255) // 256,
        block_dim=256,
    )
    ctx.enqueue_function[quantize_act_q8_1](
        xq4.unsafe_ptr(), xd4.unsafe_ptr(), xsm4.unsafe_ptr(), x.unsafe_ptr(),
        COLS, 4, grid_dim=(4 * (COLS // 32) + 255) // 256,
        block_dim=256,
    )
    ctx.enqueue_function[gemm_q8_0_i8mma_b2](
        y2.unsafe_ptr(), weights.unsafe_ptr(), xq2.unsafe_ptr(), xd2.unsafe_ptr(),
        COLS, ROWS, 2,
        grid_dim=(ROWS + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK,
        block_dim=ROWS_PER_BLOCK * WARP_SIZE,
    )
    ctx.enqueue_function[gemm_q8_0_i8mma_b3](
        y3.unsafe_ptr(), weights.unsafe_ptr(), xq3.unsafe_ptr(), xd3.unsafe_ptr(),
        COLS, ROWS, 3,
        grid_dim=(ROWS + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK,
        block_dim=ROWS_PER_BLOCK * WARP_SIZE,
    )
    ctx.enqueue_function[gemm_q8_0_i8mma_b4](
        y4.unsafe_ptr(), weights.unsafe_ptr(), xq4.unsafe_ptr(), xd4.unsafe_ptr(),
        COLS, ROWS, 4,
        grid_dim=(ROWS + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK,
        block_dim=ROWS_PER_BLOCK * WARP_SIZE,
    )
    ctx.enqueue_function[gemm_q8_0_i8mma_out_f32_b3](
        y3f.unsafe_ptr(), weights.unsafe_ptr(), xq3.unsafe_ptr(), xd3.unsafe_ptr(),
        COLS, ROWS, 3,
        grid_dim=(ROWS + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK,
        block_dim=ROWS_PER_BLOCK * WARP_SIZE,
    )
    ctx.enqueue_function[gemm_q8_0_i8mma_out_f32_b4](
        y4f.unsafe_ptr(), weights.unsafe_ptr(), xq4.unsafe_ptr(), xd4.unsafe_ptr(),
        COLS, ROWS, 4,
        grid_dim=(ROWS + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK,
        block_dim=ROWS_PER_BLOCK * WARP_SIZE,
    )
    ctx.synchronize()

    with weights.map_to_host() as w, x.map_to_host() as xv, y2.map_to_host() as result2, y3.map_to_host() as result3, y4.map_to_host() as result4, y3f.map_to_host() as result3f, y4f.map_to_host() as result4f:
        for token in range(TOKENS):
            for row in range(ROWS):
                var expected: Float32 = 0.0
                for block in range(COLS // 32):
                    offset = (row * (COLS // 32) + block) * 34
                    ref_scale = Float32((w.unsafe_ptr() + offset).bitcast[Float16]()[0])
                    activation = _quant_block(
                        xv.unsafe_ptr(), token * COLS + block * 32
                    )
                    activation_quant = activation[0]
                    var dot: Int32 = 0
                    for k in range(32):
                        code = Int(w[offset + 2 + k])
                        if code > 127:
                            code -= 256
                        dot += Int32(code) * activation_quant[k]
                    expected += ref_scale * activation[1] * Float32(dot)
                tolerance = 0.002 * (abs(expected) + 1.0)
                if token < 2 and abs(Float32(result2[token * ROWS + row]) - expected) > tolerance:
                    raise Error("niezgodny wynik GEMM Q8_0 B2")
                if token < 3 and abs(Float32(result3[token * ROWS + row]) - expected) > tolerance:
                    raise Error("niezgodny wynik GEMM Q8_0 B3")
                if abs(Float32(result4[token * ROWS + row]) - expected) > tolerance:
                    raise Error("niezgodny wynik GEMM Q8_0 B4")
                tolerance_f32 = 0.00001 * (abs(expected) + 1.0)
                if token < 3 and abs(result3f[token * ROWS + row] - expected) > tolerance_f32:
                    raise Error("niezgodny wynik F32 GEMM Q8_0 B3")
                if abs(result4f[token * ROWS + row] - expected) > tolerance_f32:
                    raise Error("niezgodny wynik F32 GEMM Q8_0 B4")
    print("golden Q8_0 T=2/3/4 F16/F32: PASS")


def _bench[ROWS: Int, COLS: Int](ctx: DeviceContext) raises:
    comptime TOKENS = 3
    comptime ITERS = 40
    var weights = ctx.enqueue_create_buffer[DType.uint8](ROWS * (COLS // 32) * 34)
    var x = ctx.enqueue_create_buffer[DType.float16](TOKENS * COLS)
    var y = ctx.enqueue_create_buffer[DType.float16](TOKENS * ROWS)
    var xq = ctx.enqueue_create_buffer[DType.int8](TOKENS * COLS)
    var xd = ctx.enqueue_create_buffer[DType.float32](TOKENS * (COLS // 32))
    var xsm = ctx.enqueue_create_buffer[DType.float32](TOKENS * (COLS // 32))
    with weights.map_to_host() as values:
        for i in range(len(values)):
            values[i] = UInt8((i * 29 + 11) & 0xFF)
        for i in range(0, len(values), 34):
            scale = Float16(0.03125)
            bits = scale.to_bits()
            values[i] = UInt8(bits & 0xFF)
            values[i + 1] = UInt8((bits >> 8) & 0xFF)
    with x.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(_fill(i))

    for _ in range(5):
        ctx.enqueue_function[quantize_act_q8_1](
            xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), x.unsafe_ptr(),
            COLS, TOKENS, grid_dim=(TOKENS * (COLS // 32) + 255) // 256,
            block_dim=256,
        )
        ctx.enqueue_function[gemm_q8_0_i8mma_b3](
            y.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
            COLS, ROWS, TOKENS,
            grid_dim=(ROWS + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK,
            block_dim=ROWS_PER_BLOCK * WARP_SIZE,
        )
    ctx.synchronize()
    start = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[quantize_act_q8_1](
            xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), x.unsafe_ptr(),
            COLS, TOKENS, grid_dim=(TOKENS * (COLS // 32) + 255) // 256,
            block_dim=256,
        )
        ctx.enqueue_function[gemm_q8_0_i8mma_b3](
            y.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
            COLS, ROWS, TOKENS,
            grid_dim=(ROWS + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK,
            block_dim=ROWS_PER_BLOCK * WARP_SIZE,
        )
    ctx.synchronize()
    batch_ms = Float64(perf_counter_ns() - start) / 1e6 / ITERS

    start = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[quantize_act_q8_1](
            xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), x.unsafe_ptr(),
            COLS, TOKENS, grid_dim=(TOKENS * (COLS // 32) + 255) // 256,
            block_dim=256,
        )
    ctx.synchronize()
    quant_ms = Float64(perf_counter_ns() - start) / 1e6 / ITERS

    for _ in range(5):
        ctx.enqueue_function[quantize_act_q8_1](
            xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), x.unsafe_ptr(),
            COLS, TOKENS, grid_dim=(TOKENS * (COLS // 32) + 255) // 256,
            block_dim=256,
        )
        ctx.enqueue_function[gemm_q8_0_i8mma_bm64](
            y.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
            xsm.unsafe_ptr(), COLS, ROWS, TOKENS,
            grid_dim=((ROWS + 63) // 64, 1), block_dim=256,
        )
    ctx.synchronize()
    start = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[quantize_act_q8_1](
            xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), x.unsafe_ptr(),
            COLS, TOKENS, grid_dim=(TOKENS * (COLS // 32) + 255) // 256,
            block_dim=256,
        )
        ctx.enqueue_function[gemm_q8_0_i8mma_bm64](
            y.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
            xsm.unsafe_ptr(), COLS, ROWS, TOKENS,
            grid_dim=((ROWS + 63) // 64, 1), block_dim=256,
        )
    ctx.synchronize()
    i8mma_ms = Float64(perf_counter_ns() - start) / 1e6 / ITERS

    for _ in range(5):
        for token in range(TOKENS):
            ctx.enqueue_function[gemv_q8_0_f16_v2](
                y.unsafe_ptr() + token * ROWS, weights.unsafe_ptr(),
                x.unsafe_ptr() + token * COLS, COLS, ROWS,
                grid_dim=(ROWS + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK,
                block_dim=ROWS_PER_BLOCK * WARP_SIZE,
            )
    ctx.synchronize()
    start = perf_counter_ns()
    for _ in range(ITERS):
        for token in range(TOKENS):
            ctx.enqueue_function[gemv_q8_0_f16_v2](
                y.unsafe_ptr() + token * ROWS, weights.unsafe_ptr(),
                x.unsafe_ptr() + token * COLS, COLS, ROWS,
                grid_dim=(ROWS + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK,
                block_dim=ROWS_PER_BLOCK * WARP_SIZE,
            )
    ctx.synchronize()
    gemv_ms = Float64(perf_counter_ns() - start) / 1e6 / ITERS
    print(
        "Q8_0 ", ROWS, "x", COLS, " T=3 batch_ms=", batch_ms,
        " quant_ms=", quant_ms,
        " i8mma_ms=", i8mma_ms, " i8mma_speedup=", i8mma_ms / batch_ms,
        " gemv_ms=", gemv_ms, " gemv_speedup=", gemv_ms / batch_ms,
    )


def _bench_logits[TOKENS: Int](ctx: DeviceContext) raises:
    comptime ROWS = 248320
    comptime COLS = 5120
    comptime ITERS = 20
    var weights = ctx.enqueue_create_buffer[DType.uint8](ROWS * (COLS // 32) * 34)
    var x = ctx.enqueue_create_buffer[DType.float16](TOKENS * COLS)
    var y = ctx.enqueue_create_buffer[DType.float32](TOKENS * ROWS)
    var ids = ctx.enqueue_create_buffer[DType.int32](TOKENS)
    var xq = ctx.enqueue_create_buffer[DType.int8](TOKENS * COLS)
    var xd = ctx.enqueue_create_buffer[DType.float32](TOKENS * (COLS // 32))
    var xsm = ctx.enqueue_create_buffer[DType.float32](TOKENS * (COLS // 32))
    with weights.map_to_host() as values:
        for i in range(len(values)):
            values[i] = UInt8((i * 29 + 11) & 0xFF)
        for i in range(0, len(values), 34):
            scale = Float16(0.03125)
            bits = scale.to_bits()
            values[i] = UInt8(bits & 0xFF)
            values[i + 1] = UInt8((bits >> 8) & 0xFF)
    with x.map_to_host() as values:
        for i in range(len(values)):
            values[i] = Float16(_fill(i))

    for _ in range(4):
        ctx.enqueue_function[quantize_act_q8_1](
            xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), x.unsafe_ptr(),
            COLS, TOKENS, grid_dim=(TOKENS * (COLS // 32) + 255) // 256,
            block_dim=256,
        )
        comptime if TOKENS == 3:
            ctx.enqueue_function[gemm_q8_0_i8mma_out_f32_b3](
                y.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
                COLS, ROWS, TOKENS,
                grid_dim=(ROWS + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK,
                block_dim=ROWS_PER_BLOCK * WARP_SIZE,
            )
        else:
            ctx.enqueue_function[gemm_q8_0_i8mma_out_f32_b4](
                y.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
                COLS, ROWS, TOKENS,
                grid_dim=(ROWS + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK,
                block_dim=ROWS_PER_BLOCK * WARP_SIZE,
            )
        ctx.enqueue_function[argmax_batched_f32](
            ids.unsafe_ptr(), y.unsafe_ptr(), ROWS,
            grid_dim=TOKENS, block_dim=256,
        )
    ctx.synchronize()

    start = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[quantize_act_q8_1](
            xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), x.unsafe_ptr(),
            COLS, TOKENS, grid_dim=(TOKENS * (COLS // 32) + 255) // 256,
            block_dim=256,
        )
        comptime if TOKENS == 3:
            ctx.enqueue_function[gemm_q8_0_i8mma_out_f32_b3](
                y.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
                COLS, ROWS, TOKENS,
                grid_dim=(ROWS + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK,
                block_dim=ROWS_PER_BLOCK * WARP_SIZE,
            )
        else:
            ctx.enqueue_function[gemm_q8_0_i8mma_out_f32_b4](
                y.unsafe_ptr(), weights.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
                COLS, ROWS, TOKENS,
                grid_dim=(ROWS + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK,
                block_dim=ROWS_PER_BLOCK * WARP_SIZE,
            )
        ctx.enqueue_function[argmax_batched_f32](
            ids.unsafe_ptr(), y.unsafe_ptr(), ROWS,
            grid_dim=TOKENS, block_dim=256,
        )
    ctx.synchronize()
    small_ms = Float64(perf_counter_ns() - start) / 1e6 / ITERS

    for _ in range(4):
        ctx.enqueue_function[gemm_q8_0_out_f32](
            y.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS, TOKENS,
            grid_dim=((ROWS + 63) // 64, 1), block_dim=256,
        )
        ctx.enqueue_function[argmax_batched_f32](
            ids.unsafe_ptr(), y.unsafe_ptr(), ROWS,
            grid_dim=TOKENS, block_dim=256,
        )
    ctx.synchronize()
    start = perf_counter_ns()
    for _ in range(ITERS):
        ctx.enqueue_function[gemm_q8_0_out_f32](
            y.unsafe_ptr(), weights.unsafe_ptr(), x.unsafe_ptr(), COLS, ROWS, TOKENS,
            grid_dim=((ROWS + 63) // 64, 1), block_dim=256,
        )
        ctx.enqueue_function[argmax_batched_f32](
            ids.unsafe_ptr(), y.unsafe_ptr(), ROWS,
            grid_dim=TOKENS, block_dim=256,
        )
    ctx.synchronize()
    old_ms = Float64(perf_counter_ns() - start) / 1e6 / ITERS
    print(
        "Q8_0 logits 248320x5120 T=", TOKENS,
        " q8_1_small_argmax_ms=", small_ms,
        " old_gemm_argmax_ms=", old_ms,
        " speedup=", old_ms / small_ms,
    )


def main() raises:
    var ctx = DeviceContext()
    _golden(ctx)
    _bench[5120, 5120](ctx)
    _bench[1024, 5120](ctx)
    _bench[17408, 5120](ctx)
    _bench[5120, 17408](ctx)
    _bench_logits[3](ctx)
    _bench_logits[4](ctx)
