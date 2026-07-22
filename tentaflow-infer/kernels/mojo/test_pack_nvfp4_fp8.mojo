# =============================================================================
# Plik: test_pack_nvfp4_fp8.mojo
# Opis: Golden i benchmark GPU repacku NVFP4 do FP8 E4M3.
# Przykład: pixi run mojo test_pack_nvfp4_fp8.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.memory import bitcast
from std.time import perf_counter_ns
from src.nvfp4 import pack_nvfp4_fp8

comptime THREADS = 256


def _e4m3_decode(code: UInt8) -> Float32:
    var sign: Float32 = 1.0
    if (code & 0x80) != 0:
        sign = -1.0
    exp = Int((code >> 3) & 0x0F)
    man = Float32(code & 0x07)
    if exp == 0:
        return sign * man / 512.0
    var scale: Float32 = 1.0
    var power = exp - 7
    while power > 0:
        scale *= 2.0
        power -= 1
    while power < 0:
        scale *= 0.5
        power += 1
    return sign * (1.0 + man / 8.0) * scale


def _e2m1_decode(code: UInt8) -> Float32:
    comptime values = SIMD[DType.float32, 16](
        0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0,
        -0.0, -0.5, -1.0, -1.5, -2.0, -3.0, -4.0, -6.0,
    )
    return values[Int(code & 0x0F)]


def _golden(ctx: DeviceContext) raises:
    comptime SOURCE_ROWS = 5
    comptime OFFSET = 1
    comptime ROWS = 3
    comptime COLS = 256
    comptime GLOBAL = 12.5
    var packed = ctx.enqueue_create_buffer[DType.uint8](SOURCE_ROWS * COLS // 2)
    var scales = ctx.enqueue_create_buffer[DType.uint8](SOURCE_ROWS * COLS // 16)
    var output = ctx.enqueue_create_buffer[DType.int8](ROWS * COLS)
    var output_scales = ctx.enqueue_create_buffer[DType.float32](ROWS)

    with packed.map_to_host() as p:
        for i in range(SOURCE_ROWS * COLS // 2):
            p[i] = UInt8((i * 13 + 5) & 0xFF)
        # Zerowy wiersz sprawdza kontrakt scale=0 i code=0.
        for i in range(COLS // 2):
            p[(OFFSET + 1) * (COLS // 2) + i] = 0
    with scales.map_to_host() as s:
        comptime scale_codes = SIMD[DType.uint8, 4](0x28, 0x38, 0x40, 0x48)
        for r in range(SOURCE_ROWS):
            for g in range(COLS // 16):
                s[r * (COLS // 16) + g] = scale_codes[(r + g) % 4]

    ctx.enqueue_function[pack_nvfp4_fp8](
        output.unsafe_ptr(), output_scales.unsafe_ptr(), packed.unsafe_ptr(),
        scales.unsafe_ptr(), COLS, OFFSET, ROWS, Float32(1.0 / GLOBAL),
        grid_dim=ROWS, block_dim=THREADS,
    )
    ctx.synchronize()

    with packed.map_to_host() as p, scales.map_to_host() as s, output.map_to_host() as o, output_scales.map_to_host() as os:
        for row in range(ROWS):
            source_row = OFFSET + row
            var amax: Float32 = 0.0
            for c in range(COLS):
                byte = p[source_row * (COLS // 2) + c // 2]
                code = byte & 0x0F if c % 2 == 0 else byte >> 4
                value = _e2m1_decode(code) * _e4m3_decode(
                    s[source_row * (COLS // 16) + c // 16]
                ) / GLOBAL
                if abs(value) > amax:
                    amax = abs(value)
            scale = amax / 448.0 if amax != 0.0 else 0.0
            inv = 448.0 / amax if amax != 0.0 else 0.0
            if os[row] != scale:
                raise Error("niezgodna skala wiersza")
            for c in range(COLS):
                byte = p[source_row * (COLS // 2) + c // 2]
                code = byte & 0x0F if c % 2 == 0 else byte >> 4
                value = _e2m1_decode(code) * _e4m3_decode(
                    s[source_row * (COLS // 16) + c // 16]
                ) / GLOBAL
                expected = bitcast[DType.int8, 1](
                    Scalar[DType.float8_e4m3fn](value * inv)
                )
                if o[row * COLS + c] != expected:
                    raise Error("niezgodny kod E4M3")
    print("golden offset/zero: PASS")


def _bench[ROWS: Int, COLS: Int](ctx: DeviceContext) raises -> Float64:
    var packed = ctx.enqueue_create_buffer[DType.uint8](ROWS * COLS // 2)
    var scales = ctx.enqueue_create_buffer[DType.uint8](ROWS * COLS // 16)
    var output = ctx.enqueue_create_buffer[DType.int8](ROWS * COLS)
    var output_scales = ctx.enqueue_create_buffer[DType.float32](ROWS)
    with packed.map_to_host() as p:
        for i in range(ROWS * COLS // 2):
            p[i] = UInt8((i * 17 + 9) & 0xFF)
    with scales.map_to_host() as s:
        for i in range(ROWS * COLS // 16):
            s[i] = 0x38

    for _ in range(10):
        ctx.enqueue_function[pack_nvfp4_fp8](
            output.unsafe_ptr(), output_scales.unsafe_ptr(), packed.unsafe_ptr(),
            scales.unsafe_ptr(), COLS, 0, ROWS, Float32(1.0),
            grid_dim=ROWS, block_dim=THREADS,
        )
    ctx.synchronize()
    t0 = perf_counter_ns()
    comptime ITERS = 20
    for _ in range(ITERS):
        ctx.enqueue_function[pack_nvfp4_fp8](
            output.unsafe_ptr(), output_scales.unsafe_ptr(), packed.unsafe_ptr(),
            scales.unsafe_ptr(), COLS, 0, ROWS, Float32(1.0),
            grid_dim=ROWS, block_dim=THREADS,
        )
    ctx.synchronize()
    ms = Float64(perf_counter_ns() - t0) / 1e6 / ITERS
    print("repack rows=", ROWS, " cols=", COLS, " ms=", ms)
    return ms


def main() raises:
    var ctx = DeviceContext()
    _golden(ctx)
    q = _bench[4096, 4096](ctx)
    kv = _bench[1024, 4096](ctx)
    gu = _bench[11264, 4096](ctx)
    down = _bench[4096, 11264](ctx)
    layer_ms = 2.0 * q + 2.0 * kv + 2.0 * gu + down
    print("szacowany repack 32 warstw 7B ms=", layer_ms * 32.0)
