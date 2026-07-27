# =============================================================================
# Plik: test_gemm_wmma.mojo
# Opis: Test złoty GEMM-u Q8_0 na WMMA — porównuje z tą samą matematyką liczoną
#       na hoście w int32 i f32, na kształtach z ogonami po T i po wierszach.
# Przykład: pixi run mojo run -I . test_gemm_wmma.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.random import random_si64, seed
from std.memory import bitcast

from src.gemm_wmma import gemm_q8_0_wmma_64x128

comptime QK = 32
comptime QBYTES = 34


def build_and_check(ctx: DeviceContext, n_tokens: Int, n_rows: Int, n_cols: Int) raises:
    blocks = n_cols // QK
    var wb = ctx.enqueue_create_host_buffer[DType.uint8](n_rows * blocks * QBYTES)
    var xqb = ctx.enqueue_create_host_buffer[DType.int8](n_tokens * n_cols)
    var xdb = ctx.enqueue_create_host_buffer[DType.float32](blocks * n_tokens)
    ctx.synchronize()

    for r in range(n_rows):
        for b in range(blocks):
            base = (r * blocks + b) * QBYTES
            # Skala jako f16 zapisany bajtowo — dokładnie jak w GGUF.
            scale = Float16(0.01 + 0.003 * Float64((r + b) % 7))
            bits = bitcast[DType.uint16, 1](SIMD[DType.float16, 1](scale))[0]
            wb[base] = UInt8(bits & 0xFF)
            wb[base + 1] = UInt8(bits >> 8)
            for i in range(QK):
                wb[base + 2 + i] = UInt8(
                    Int(random_si64(-127, 127)) & 0xFF
                )
    for t in range(n_tokens):
        for k in range(n_cols):
            xqb[t * n_cols + k] = Int8(Int(random_si64(-127, 127)))
    for b in range(blocks):
        for t in range(n_tokens):
            xdb[b * n_tokens + t] = Float32(0.02 + 0.001 * Float64((b * 3 + t) % 11))

    var wd = ctx.enqueue_create_buffer[DType.uint8](n_rows * blocks * QBYTES)
    var xqd = ctx.enqueue_create_buffer[DType.int8](n_tokens * n_cols)
    var xdd = ctx.enqueue_create_buffer[DType.float32](blocks * n_tokens)
    var xsm = ctx.enqueue_create_buffer[DType.float32](blocks * n_tokens)
    var yd = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(wd, wb)
    ctx.enqueue_copy(xqd, xqb)
    ctx.enqueue_copy(xdd, xdb)
    ctx.synchronize()

    comptime WAVES = 4
    comptime BM = 2 * 2 * 16
    comptime BN = 2 * 4 * 16
    grid_x = (n_rows + BN - 1) // BN
    grid_y = (n_tokens + BM - 1) // BM
    ctx.enqueue_function[gemm_q8_0_wmma_64x128](
        yd.unsafe_ptr(),
        wd.unsafe_ptr(),
        xqd.unsafe_ptr(),
        xdd.unsafe_ptr(),
        xsm.unsafe_ptr(),
        n_cols,
        n_rows,
        n_tokens,
        grid_dim=(grid_x, grid_y),
        block_dim=WAVES * 32,
    )
    ctx.synchronize()

    var yh = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(yh, yd)
    ctx.synchronize()

    var worst: Float64 = 0.0
    for t in range(n_tokens):
        for r in range(n_rows):
            var want: Float64 = 0.0
            for b in range(blocks):
                var isum: Int32 = 0
                base = (r * blocks + b) * QBYTES
                for i in range(QK):
                    # Bajt wagi to kod ZE ZNAKIEM — reinterpretacja, nie konwersja:
                    # `cast` z UInt8 zacisnąłby 200 do 127 i referencja byłaby zła.
                    wq = Int32(
                        bitcast[DType.int8, 1](SIMD[DType.uint8, 1](wb[base + 2 + i]))[0]
                    )
                    xv = Int32(xqb[t * n_cols + b * QK + i])
                    isum += wq * xv
                bits = UInt16(wb[base]) | (UInt16(wb[base + 1]) << 8)
                scale = bitcast[DType.float16, 1](SIMD[DType.uint16, 1](bits))[0]
                want += Float64(Float32(isum) * Float32(scale) * xdb[b * n_tokens + t])
            got = Float64(yh[t * n_rows + r])
            denom = abs(want)
            if denom < 1.0:
                denom = 1.0
            rel = abs(got - want) / denom
            if rel > worst:
                worst = rel
    print(
        "T=",
        n_tokens,
        "rows=",
        n_rows,
        "cols=",
        n_cols,
        "najgorszy blad wzgledny:",
        worst,
    )
    # Wyjście jest f16, więc próg mieści JEDNO zaokrąglenie wyniku i nic więcej —
    # zła kolejność akumulacji albo pomylony układ fragmentu dałyby rzędy wielkości.
    if worst > 2e-3:
        raise Error("GEMM WMMA rozjezdza sie z referencja")


def main() raises:
    seed(20260727)
    var ctx = DeviceContext()
    # Kształty równe, z ogonem po tokenach i z ogonem po wierszach.
    build_and_check(ctx, 16, 64, 64)
    build_and_check(ctx, 32, 128, 256)
    build_and_check(ctx, 5, 70, 96)
    build_and_check(ctx, 33, 65, 128)
    print("GEMM Q8_0 WMMA: wszystkie ksztalty zgodne z referencja")
