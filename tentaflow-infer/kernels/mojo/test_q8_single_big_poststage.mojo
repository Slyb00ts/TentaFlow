# =============================================================================
# Plik: test_q8_single_big_poststage.mojo
# Opis: Sprawdza bitowa zgodnosc i canary Q8 poststage dla T1024/T2048.
# Przyklad: pixi run mojo test_q8_single_big_poststage.mojo
# =============================================================================

from std.gpu.host import DeviceBuffer, DeviceContext
from std.pathlib import Path
from src.gemm_q8_triplet_variants import gemm_q8_0_i8mma_triplet_single_big
from src.q8_single_big_poststage import gemm_q8_0_i8mma_triplet_single_big_poststage

comptime ROWS0 = 6144
comptime ROWS1 = 48
comptime ROWS2 = 48
comptime COLS = 5120
comptime BLOCKS = COLS // 32
comptime GUARD = 37
comptime GUARD_VALUE = Float16(29.5)


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
    xq: DeviceBuffer[DType.int8], xd: DeviceBuffer[DType.float32], steps: Int
) raises:
    with xq.map_to_host() as host:
        for i in range(len(host)):
            host[i] = Int8((i * 31 + 11) % 255 - 127)
    with xd.map_to_host() as host:
        for block in range(BLOCKS):
            for token in range(steps):
                host[block * steps + token] = (
                    0.001 + Float32((block * 7 + token * 3) % 17) * 0.0002
                )


def _fill_guard(buffer: DeviceBuffer[DType.float16]) raises:
    with buffer.map_to_host() as host:
        for i in range(len(host)):
            host[i] = GUARD_VALUE


def _compare(
    reference: DeviceBuffer[DType.float16],
    candidate: DeviceBuffer[DType.float16],
    elements: Int,
    name: String,
) raises:
    with reference.map_to_host() as expected, candidate.map_to_host() as actual:
        for i in range(len(expected)):
            if expected[i].to_bits() != actual[i].to_bits():
                raise Error(name + " roznica bitowa lub canary przy " + String(i))


def _launch_reference[steps: Int](
    ctx: DeviceContext,
    mut y0: DeviceBuffer[DType.float16],
    mut y1: DeviceBuffer[DType.float16],
    mut y2: DeviceBuffer[DType.float16],
    w0: DeviceBuffer[DType.uint8],
    w1: DeviceBuffer[DType.uint8],
    w2: DeviceBuffer[DType.uint8],
    xq: DeviceBuffer[DType.int8],
    xd: DeviceBuffer[DType.float32],
    xsm: DeviceBuffer[DType.float32],
) raises:
    ctx.enqueue_function[gemm_q8_0_i8mma_triplet_single_big](
        y0.unsafe_ptr() + GUARD, w0.unsafe_ptr(), ROWS0,
        y1.unsafe_ptr() + GUARD, w1.unsafe_ptr(), ROWS1,
        y2.unsafe_ptr() + GUARD, w2.unsafe_ptr(), ROWS2,
        xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), COLS, steps,
        grid_dim=(50, (steps + 127) // 128), block_dim=512,
    )


def _launch_candidate[steps: Int](
    ctx: DeviceContext,
    mut y0: DeviceBuffer[DType.float16],
    mut y1: DeviceBuffer[DType.float16],
    mut y2: DeviceBuffer[DType.float16],
    w0: DeviceBuffer[DType.uint8],
    w1: DeviceBuffer[DType.uint8],
    w2: DeviceBuffer[DType.uint8],
    xq: DeviceBuffer[DType.int8],
    xd: DeviceBuffer[DType.float32],
    xsm: DeviceBuffer[DType.float32],
) raises:
    ctx.enqueue_function[gemm_q8_0_i8mma_triplet_single_big_poststage](
        y0.unsafe_ptr() + GUARD, w0.unsafe_ptr(), ROWS0,
        y1.unsafe_ptr() + GUARD, w1.unsafe_ptr(), ROWS1,
        y2.unsafe_ptr() + GUARD, w2.unsafe_ptr(), ROWS2,
        xq.unsafe_ptr(), xd.unsafe_ptr(), xsm.unsafe_ptr(), COLS, steps,
        grid_dim=(50, (steps + 127) // 128), block_dim=512,
    )


def _case[steps: Int](
    ctx: DeviceContext,
    w0: DeviceBuffer[DType.uint8],
    w1: DeviceBuffer[DType.uint8],
    w2: DeviceBuffer[DType.uint8],
) raises:
    var xq = ctx.enqueue_create_buffer[DType.int8](steps * COLS)
    var xd = ctx.enqueue_create_buffer[DType.float32](steps * BLOCKS)
    var xsm = ctx.enqueue_create_buffer[DType.float32](steps * BLOCKS)
    _fill_prepared(xq, xd, steps)
    var r0 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS0 + 2 * GUARD)
    var r1 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS1 + 2 * GUARD)
    var r2 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS2 + 2 * GUARD)
    var c0 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS0 + 2 * GUARD)
    var c1 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS1 + 2 * GUARD)
    var c2 = ctx.enqueue_create_buffer[DType.float16](steps * ROWS2 + 2 * GUARD)
    _fill_guard(r0)
    _fill_guard(r1)
    _fill_guard(r2)
    _fill_guard(c0)
    _fill_guard(c1)
    _fill_guard(c2)
    _launch_reference[steps](ctx, r0, r1, r2, w0, w1, w2, xq, xd, xsm)
    _launch_candidate[steps](ctx, c0, c1, c2, w0, w1, w2, xq, xd, xsm)
    ctx.synchronize()
    _compare(r0, c0, steps * ROWS0, "gate T" + String(steps))
    _compare(r1, c1, steps * ROWS1, "alpha T" + String(steps))
    _compare(r2, c2, steps * ROWS2, "beta T" + String(steps))
    print("Q8 poststage bit-exact z canary T", steps, ": PASS")


def main() raises:
    var ctx = DeviceContext()
    _ = ctx.compile_function[
        gemm_q8_0_i8mma_triplet_single_big_poststage,
        dump_asm=Path("gemm_q8_0_i8mma_triplet_single_big_poststage.ptx"),
    ]()
    var w0 = ctx.enqueue_create_buffer[DType.uint8](ROWS0 * BLOCKS * 34)
    var w1 = ctx.enqueue_create_buffer[DType.uint8](ROWS1 * BLOCKS * 34)
    var w2 = ctx.enqueue_create_buffer[DType.uint8](ROWS2 * BLOCKS * 34)
    _fill_weight(w0, ROWS0, 3)
    _fill_weight(w1, ROWS1, 7)
    _fill_weight(w2, ROWS2, 11)
    _case[1024](ctx, w0, w1, w2)
    _case[2048](ctx, w0, w1, w2)
