# Golden test PRZENOSNEGO kafla (bez jednostki macierzowej) Q3_K WMMA: wynik GPU wobec referencji liczonej na hoscie
# dokladnie ta sama formula co `gemm_q5_k_impl` (q5 * d*sc - dmin*mn).
from std.gpu.host import DeviceContext
from std.random import random_si64, seed

from src.gemm_q3_k_wmma import gemm_q3_k_tile_f16_bm32
from src.gemv2 import _q3k_scales8

comptime SB_BYTES = 110
comptime SUPERBLOCK = 256


def scale_min(s: UnsafePointer[UInt8, MutUntrackedOrigin], j: Int) -> Tuple[Float32, Float32]:
    if j < 4:
        return (Float32(Int(s[4 + j] & 63)), Float32(Int(s[8 + j] & 63)))
    return (
        Float32(Int((s[8 + j] & 0x0F) | ((s[4 + j - 4] >> 6) << 4))),
        Float32(Int((s[8 + j] >> 4) | ((s[4 + j] >> 6) << 4))),
    )


def reference(w: UnsafePointer[UInt8, MutUntrackedOrigin], row: Int, col: Int, n_cols: Int) -> Float32:
    # Skale 6-bitowe rozpakowuje ten sam pomocnik co sciezka NVIDIA — testowana
    # jest indeksacja kafla, nie on.
    sb = row * (n_cols // SUPERBLOCK) * SB_BYTES + (col // SUPERBLOCK) * SB_BYTES
    r = col % SUPERBLOCK
    n = r // 128
    sh = (r % 128) // 32
    d = Float32((w + sb + 108).bitcast[Float16]().load[width=1, alignment=1]()[0])
    sc8 = _q3k_scales8(rebind[UnsafePointer[UInt8, MutAnyOrigin]](w), sb, n)
    scale = d * sc8[2 * sh + (r % 32) // 16]
    q = Int(((w + sb + 32 + n * 32 + (r % 32))[0] >> UInt8(2 * sh)) & 3)
    hb = Int(((w + sb + (r % 32))[0] >> UInt8(4 * n + sh)) & 1)
    return Float32(q + 4 * hb - 4) * scale


def run(ctx: DeviceContext, n_rows: Int, n_cols: Int, n_tokens: Int) raises:
    sbs = n_cols // SUPERBLOCK
    var wh = ctx.enqueue_create_host_buffer[DType.uint8](n_rows * sbs * SB_BYTES)
    var xh = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_cols)
    ctx.synchronize()
    for i in range(n_rows * sbs * SB_BYTES):
        wh[i] = UInt8(Int(random_si64(0, 255)))
    # d/dmin musza byc male i dodatnie, zeby referencja f32 i akumulacja f32
    # zostaly w porownywalnym zakresie.
    for r in range(n_rows):
        for s in range(sbs):
            base = r * sbs * SB_BYTES + s * SB_BYTES
            wh[base + 108] = 0x00
            wh[base + 109] = 0x2C    # d ~0.0625
    for i in range(n_tokens * n_cols):
        xh[i] = Float16(Float64(Int(random_si64(-4, 4))) * 0.25)

    var w = ctx.enqueue_create_buffer[DType.uint8](n_rows * sbs * SB_BYTES)
    var x = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_cols)
    var y = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(w, wh)
    ctx.enqueue_copy(x, xh)
    ctx.synchronize()

    comptime BM = 32
    comptime BN = 64
    ctx.enqueue_function[gemm_q3_k_tile_f16_bm32](
        y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(),
        n_cols, n_rows, n_tokens,
        grid_dim=((n_rows + BN - 1) // BN, (n_tokens + BM - 1) // BM),
        block_dim=(BM // 4) * (BN // 4),
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
                acc += reference(wh.unsafe_ptr(), r, c, n_cols) * Float32(xh[t * n_cols + c])
            got = Float64(Float32(yh[t * n_rows + r]))
            diff = abs(got - Float64(acc))
            if diff > worst:
                worst = diff
                worst_ref = Float64(acc)
    var label = String("tile")
    print(label, "rows=", n_rows, "cols=", n_cols, "T=", n_tokens,
          "| najgorsza roznica", worst, "przy referencji", worst_ref)


def main() raises:
    seed(20260728)
    var ctx = DeviceContext()
    run(ctx, 64, 256, 32)
    run(ctx, 128, 512, 40)
    run(ctx, 128, 512, 300)
    run(ctx, 70, 768, 17)
