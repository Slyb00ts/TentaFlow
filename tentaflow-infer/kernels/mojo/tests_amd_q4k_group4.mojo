# =============================================================================
# Plik: tests_amd_q4k_group4.mojo
# Opis: Porownuje grouped Q4_K DP4A RDNA4 z niezalezna referencja CPU GGUF.
# Przykład: mojo run tests_amd_q4k_group4.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.memory import bitcast
from std.random import random_si64, seed
from src.decode_dp4a import gemv_q4_k_dp4a_group4_f16

comptime COLS = 5120
comptime MAX_ROWS = 73
comptime WEIGHT_BYTES = MAX_ROWS * (COLS // 256) * 144
comptime GUARD_BYTES = 256
comptime WEIGHT_ALLOCATION_BYTES = WEIGHT_BYTES + 2 * GUARD_BYTES


def assert_q4k_weight_loader_bounds() raises:
    for c in range(4):
        for half in range(2):
            base = 16 + c * 32 + half * 16
            if base < 16 or base + 15 > 143:
                raise Error("portable c/half 16B mapping wychodzi poza superblok")

    for j in range(4):
        for w4 in range(4):
            lower_half = 16 + j * 32
            upper_half = lower_half + 16
            q = 16 + j * 32 + 4 * w4
            if q != lower_half + 4 * w4 or q + 16 != upper_half + 4 * w4:
                raise Error("legacy j/w4 nie jest rownowazne podzialowi portable c/half")
            if q < lower_half or q + 3 >= lower_half + 16:
                raise Error("pierwszy odczyt 4B legacy wychodzi poza dolna polowe portable")
            if q + 16 < upper_half or q + 19 >= upper_half + 16:
                raise Error("drugi odczyt 4B legacy wychodzi poza gorna polowe portable")

            crosses = q + 23 >= 144
            if crosses != (j == 3 and w4 == 3):
                raise Error("tylko lane j=3,w4=3 przekracza superblok przy drugim odczycie 8B")


def f16_at(host: UnsafePointer[UInt8, MutUntrackedOrigin], offset: Int) -> Float32:
    bits = UInt16(host[offset]) | (UInt16(host[offset + 1]) << 8)
    return Float32(bitcast[DType.float16, 1](SIMD[DType.uint16, 1](bits))[0])


def scale_min(host: UnsafePointer[UInt8, MutUntrackedOrigin], base: Int, sub: Int) -> Tuple[Int, Int]:
    if sub < 4:
        return (Int(host[base + 4 + sub] & 63), Int(host[base + 8 + sub] & 63))
    return (
        Int(host[base + 8 + sub] & 0x0F) | (Int(host[base + sub]) >> 6) << 4,
        Int(host[base + 8 + sub] >> 4) | (Int(host[base + 4 + sub]) >> 2) & 0x30,
    )


def reference_row(
    weights: UnsafePointer[UInt8, MutUntrackedOrigin],
    activation: UnsafePointer[Float16, MutUntrackedOrigin],
    row: Int,
) -> Float32:
    var total: Float32 = 0.0
    for superblock in range(COLS // 256):
        base = (row * (COLS // 256) + superblock) * 144
        d = f16_at(weights, base)
        dmin = f16_at(weights, base + 2)
        for sub in range(8):
            scale, minimum = scale_min(weights, base, sub)
            pair = sub // 2
            var sum: Float32 = 0.0
            var amax: Float32 = 0.0
            for index in range(32):
                value = Float32(activation[superblock * 256 + sub * 32 + index])
                sum += value
                amax = max(amax, abs(value))
            if amax == 0.0:
                continue
            var dot: Int = 0
            for index in range(32):
                packed = weights[base + 16 + pair * 32 + index]
                quant = Int(packed & 0x0F) if sub % 2 == 0 else Int(packed >> 4)
                value = Float32(activation[superblock * 256 + sub * 32 + index])
                dot += quant * Int(round(value * (127.0 / amax)))
            total += d * Float32(scale) * (amax / 127.0) * Float32(dot)
            total -= dmin * Float32(minimum) * sum
    return total


def fill_weights(host: UnsafePointer[UInt8, MutUntrackedOrigin]):
    for index in range(WEIGHT_BYTES):
        host[index] = UInt8(Int(random_si64(0, 255)))
    for row in range(MAX_ROWS):
        for block in range(COLS // 256):
            base = (row * (COLS // 256) + block) * 144
            host[base] = 0x00
            host[base + 1] = 0x2C
            host[base + 2] = 0x00
            host[base + 3] = 0x28


def check_group(ctx: DeviceContext, count: Int, rows0: Int, rows1: Int, rows2: Int, rows3: Int, model_id: String) raises:
    var xh = ctx.enqueue_create_host_buffer[DType.float16](COLS)
    var w0h = ctx.enqueue_create_host_buffer[DType.uint8](WEIGHT_ALLOCATION_BYTES)
    var w1h = ctx.enqueue_create_host_buffer[DType.uint8](WEIGHT_ALLOCATION_BYTES)
    var w2h = ctx.enqueue_create_host_buffer[DType.uint8](WEIGHT_ALLOCATION_BYTES)
    var w3h = ctx.enqueue_create_host_buffer[DType.uint8](WEIGHT_ALLOCATION_BYTES)
    ctx.synchronize()
    for index in range(COLS):
        xh[index] = Float16(Float32(Int(random_si64(-32, 32))) * 0.0625)
    fill_weights(w0h.unsafe_ptr() + GUARD_BYTES)
    fill_weights(w1h.unsafe_ptr() + GUARD_BYTES)
    fill_weights(w2h.unsafe_ptr() + GUARD_BYTES)
    fill_weights(w3h.unsafe_ptr() + GUARD_BYTES)

    var x = ctx.enqueue_create_buffer[DType.float16](COLS)
    var w0 = ctx.enqueue_create_buffer[DType.uint8](WEIGHT_ALLOCATION_BYTES)
    var w1 = ctx.enqueue_create_buffer[DType.uint8](WEIGHT_ALLOCATION_BYTES)
    var w2 = ctx.enqueue_create_buffer[DType.uint8](WEIGHT_ALLOCATION_BYTES)
    var w3 = ctx.enqueue_create_buffer[DType.uint8](WEIGHT_ALLOCATION_BYTES)
    var got0 = ctx.enqueue_create_buffer[DType.float16](MAX_ROWS)
    var got1 = ctx.enqueue_create_buffer[DType.float16](MAX_ROWS)
    var got2 = ctx.enqueue_create_buffer[DType.float16](MAX_ROWS)
    var got3 = ctx.enqueue_create_buffer[DType.float16](MAX_ROWS)
    ctx.enqueue_copy(x, xh)
    ctx.enqueue_copy(w0, w0h)
    ctx.enqueue_copy(w1, w1h)
    ctx.enqueue_copy(w2, w2h)
    ctx.enqueue_copy(w3, w3h)
    ctx.enqueue_function[gemv_q4_k_dp4a_group4_f16](
        got0.unsafe_ptr(), w0.unsafe_ptr() + GUARD_BYTES, rows0,
        got1.unsafe_ptr(), w1.unsafe_ptr() + GUARD_BYTES, rows1 if count >= 2 else 0,
        got2.unsafe_ptr(), w2.unsafe_ptr() + GUARD_BYTES, rows2 if count >= 3 else 0,
        got3.unsafe_ptr(), w3.unsafe_ptr() + GUARD_BYTES, rows3 if count == 4 else 0,
        x.unsafe_ptr(), COLS,
        grid_dim=((rows0 + 7) // 8 + (rows1 + 7) // 8 if count >= 2 else 0) + ((rows2 + 7) // 8 if count >= 3 else 0) + ((rows3 + 7) // 8 if count == 4 else 0), block_dim=256,
    )
    ctx.synchronize()

    var weights = [w0h, w1h, w2h, w3h]
    var got = [got0, got1, got2, got3]
    var rows = [rows0, rows1, rows2, rows3]
    for projection in range(count):
        var goth = ctx.enqueue_create_host_buffer[DType.float16](MAX_ROWS)
        ctx.enqueue_copy(goth, got[projection])
        ctx.synchronize()
        for row in range(rows[projection]):
            want = Float32(Float16(reference_row(weights[projection].unsafe_ptr() + GUARD_BYTES, xh.unsafe_ptr(), row)))
            if abs(Float32(goth[row]) - want) > 0.125:
                print("projection=", projection, " row=", row, " got=", goth[row], " want=", want)
                raise Error("group4 Q4_K DP4A rozni sie od niezaleznej referencji CPU")
    print(model_id, "group=", count, "PASS")


def main() raises:
    seed(20260810)
    assert_q4k_weight_loader_bounds()
    var ctx = DeviceContext()
    check_group(ctx, 2, 17, 33, 0, 0, "qwen35-27b")
    check_group(ctx, 3, 65, 9, 41, 0, "qwen35-27b-qkv")
    check_group(ctx, 4, 73, 1, 32, 57, "qwen35-27b-deltanet")
