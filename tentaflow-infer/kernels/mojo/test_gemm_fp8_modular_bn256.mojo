# =============================================================================
# Plik: test_gemm_fp8_modular_bn256.mojo
# Opis: Porównuje numerycznie kafle BN128 i BN256 wieloetapowego GEMM FP8.
# Przykład: pixi run mojo test_gemm_fp8_modular_bn256.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from src.gemm_fp8_modular import gemm_fp8_mod_tile

comptime M = 127
comptime N = 256
comptime K = 256


def main() raises:
    var ctx = DeviceContext()
    var a = ctx.enqueue_create_buffer[DType.float8_e4m3fn](M * K)
    var b = ctx.enqueue_create_buffer[DType.float8_e4m3fn](N * K)
    var xs = ctx.enqueue_create_buffer[DType.float32](M)
    var ws = ctx.enqueue_create_buffer[DType.float32](N)
    var reference = ctx.enqueue_create_buffer[DType.float16](M * N)
    var candidate = ctx.enqueue_create_buffer[DType.float16](M * N)

    with a.map_to_host() as values:
        for index in range(len(values)):
            values[index] = Scalar[DType.float8_e4m3fn](
                Float32((index * 17 + 3) % 15 - 7) * 0.03125
            )
    with b.map_to_host() as values:
        for index in range(len(values)):
            values[index] = Scalar[DType.float8_e4m3fn](
                Float32((index * 11 + 5) % 15 - 7) * 0.03125
            )
    with xs.map_to_host() as values:
        for index in range(len(values)):
            values[index] = Float32(index % 7 + 1) * 0.125
    with ws.map_to_host() as values:
        for index in range(len(values)):
            values[index] = Float32(index % 5 + 1) * 0.125

    ctx.enqueue_function[gemm_fp8_mod_tile[N, K, 128]](
        reference.unsafe_ptr(),
        a.unsafe_ptr(),
        b.unsafe_ptr(),
        xs.unsafe_ptr(),
        ws.unsafe_ptr(),
        M,
        grid_dim=((N + 127) // 128, (M + 127) // 128),
        block_dim=128,
        shared_mem_bytes=65_536,
    )
    ctx.enqueue_function[gemm_fp8_mod_tile[N, K, 256]](
        candidate.unsafe_ptr(),
        a.unsafe_ptr(),
        b.unsafe_ptr(),
        xs.unsafe_ptr(),
        ws.unsafe_ptr(),
        M,
        grid_dim=((N + 255) // 256, (M + 127) // 128),
        block_dim=256,
        shared_mem_bytes=98_304,
    )
    ctx.synchronize()

    var mismatches = 0
    var max_abs = Float32(0.0)
    with reference.map_to_host() as expected, candidate.map_to_host() as actual:
        for index in range(M * N):
            difference = abs(Float32(expected[index]) - Float32(actual[index]))
            max_abs = max(max_abs, difference)
            if expected[index] != actual[index]:
                mismatches += 1
    print("mismatches", mismatches, "max_abs", max_abs)
    if mismatches != 0:
        raise Error("BN256 nie jest bitowo zgodny z BN128")
    print("PASS")
