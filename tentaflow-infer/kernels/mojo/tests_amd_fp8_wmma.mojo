# Golden test GEMM-u FP8 na WMMA: wynik GPU wobec referencji liczonej na hoscie
# z tych samych kodow e4m3. Kernel nie dekoduje niczego sam — mnozy kody przez
# jednostke macierzowa i skaluje raz na koncu — wiec referencja musi dekodowac
# e4m3 dokladnie tak samo, jak robi to sprzet.
from std.gpu.host import DeviceContext
from std.random import random_si64, seed

from src.gemm_fp8_wmma import (
    gemm_fp8_wmma_bm32,
    gemm_fp8_wmma_bm256_bn128,
    gemm_fp8_wmma_bm512_bn128,
)


def e4m3_to_f32(code: UInt8) -> Float32:
    """Dekod e4m3 (OCP FP8): znak, cztery bity wykladnika, trzy mantysy.

    Wartosci subnormalne maja wykladnik zero i brak jedynki wiodacej. Kod
    0x7F/0xFF to NaN — generator testu ich nie produkuje.
    """
    var sign: Float32 = -1.0 if (code & 0x80) != 0 else 1.0
    exponent = Int((code >> 3) & 0x0F)
    mantissa = Int(code & 0x07)
    if exponent == 0:
        return sign * Float32(mantissa) * 0.001953125  # 2^-9
    var scale: Float32 = 1.0
    var e = exponent - 7
    while e > 0:
        scale *= 2.0
        e -= 1
    while e < 0:
        scale *= 0.5
        e += 1
    return sign * (1.0 + Float32(mantissa) * 0.125) * scale


def run(ctx: DeviceContext, n_rows: Int, n_cols: Int, n_tokens: Int, tile: Int) raises:
    var wh = ctx.enqueue_create_host_buffer[DType.uint8](n_rows * n_cols)
    var xh = ctx.enqueue_create_host_buffer[DType.uint8](n_tokens * n_cols)
    var sh = ctx.enqueue_create_host_buffer[DType.float32](n_rows)
    var th = ctx.enqueue_create_host_buffer[DType.float32](n_tokens)
    ctx.synchronize()
    # Zakres kodow z dala od NaN i od skrajnych wykladnikow, zeby referencja f32
    # i akumulacja f32 zostaly w porownywalnym zakresie.
    for i in range(n_rows * n_cols):
        wh[i] = UInt8(Int(random_si64(0, 1)) * 128 + 0x30 + Int(random_si64(0, 15)))
    for i in range(n_tokens * n_cols):
        xh[i] = UInt8(Int(random_si64(0, 1)) * 128 + 0x30 + Int(random_si64(0, 15)))
    for i in range(n_rows):
        sh[i] = 0.01 + Float32(i % 7) * 0.001
    for i in range(n_tokens):
        th[i] = 0.02 + Float32(i % 5) * 0.002

    var w = ctx.enqueue_create_buffer[DType.uint8](n_rows * n_cols)
    var x = ctx.enqueue_create_buffer[DType.uint8](n_tokens * n_cols)
    var wsg = ctx.enqueue_create_buffer[DType.float32](n_rows)
    var xsg = ctx.enqueue_create_buffer[DType.float32](n_tokens)
    var y = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(w, wh)
    ctx.enqueue_copy(x, xh)
    ctx.enqueue_copy(wsg, sh)
    ctx.enqueue_copy(xsg, th)
    ctx.synchronize()

    if tile == 0:
        comptime BM = 32
        comptime BN = 64
        ctx.enqueue_function[gemm_fp8_wmma_bm32](
            y.unsafe_ptr(), w.unsafe_ptr(), wsg.unsafe_ptr(),
            x.unsafe_ptr(), xsg.unsafe_ptr(), n_cols, n_rows, n_tokens,
            grid_dim=((n_rows + BN - 1) // BN, (n_tokens + BM - 1) // BM),
            block_dim=128,
        )
    elif tile == 1:
        comptime BM = 256
        comptime BN = 128
        ctx.enqueue_function[gemm_fp8_wmma_bm256_bn128](
            y.unsafe_ptr(), w.unsafe_ptr(), wsg.unsafe_ptr(),
            x.unsafe_ptr(), xsg.unsafe_ptr(), n_cols, n_rows, n_tokens,
            grid_dim=((n_rows + BN - 1) // BN, (n_tokens + BM - 1) // BM),
            block_dim=256,
        )
    else:
        comptime BM = 512
        comptime BN = 128
        ctx.enqueue_function[gemm_fp8_wmma_bm512_bn128](
            y.unsafe_ptr(), w.unsafe_ptr(), wsg.unsafe_ptr(),
            x.unsafe_ptr(), xsg.unsafe_ptr(), n_cols, n_rows, n_tokens,
            grid_dim=((n_rows + BN - 1) // BN, (n_tokens + BM - 1) // BM),
            block_dim=512,
        )

    var yh = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(yh, y)
    ctx.synchronize()

    var worst: Float64 = 0.0
    var worst_ref: Float64 = 0.0
    for t in range(n_tokens):
        for r in range(n_rows):
            var acc: Float32 = 0.0
            for c in range(n_cols):
                acc += e4m3_to_f32(wh[r * n_cols + c]) * e4m3_to_f32(
                    xh[t * n_cols + c]
                )
            want = Float64(acc * sh[r] * th[t])
            got = Float64(Float32(yh[t * n_rows + r]))
            rel = abs(got - want) / (abs(want) + 1.0)
            if rel > worst:
                worst = rel
                worst_ref = want
    print(
        "kafel", tile, "rows=", n_rows, "cols=", n_cols, "T=", n_tokens,
        "| najgorszy blad wzgledny", worst, "przy referencji", worst_ref,
    )
    if worst > 0.01:
        raise Error("GEMM FP8 WMMA rozjezdza sie z referencja")


def main() raises:
    seed(20260730)
    var ctx = DeviceContext()
    for tile in range(3):
        run(ctx, 64, 256, 32, tile)
        run(ctx, 130, 512, 70, tile)
    print("GEMM FP8 WMMA: wszystkie kafle zgodne z referencja")
