# Golden test kafla Q6_K WMMA: wynik GPU wobec referencji liczonej na hoscie
# dokladnie ta sama formula co `_gemv_q6_k_row_acc` ((q6 - 32) * d * sc).
from std.gpu.host import DeviceContext
from std.random import random_si64, seed

from src.gemm_q6_k_wmma import (
    gemm_q6_k_wmma_f16_bm32,
    gemm_q6_k_wmma_f16_bm256,
    gemm_q6_k_wmma_f16_bm256_bn128,
    gemm_q6_k_wmma_f16_bm512_bn128,
)

comptime SB_BYTES = 210
comptime SUPERBLOCK = 256


def reference(
    w: UnsafePointer[UInt8, MutUntrackedOrigin], row: Int, col: Int, n_cols: Int
) -> Float32:
    sb = row * (n_cols // SUPERBLOCK) * SB_BYTES + (col // SUPERBLOCK) * SB_BYTES
    r = col % SUPERBLOCK
    half = r // 128
    j = r % 128
    group = j // 32
    l = j % 32
    d = Float32((w + sb + 208).bitcast[Float16]().load[width=1, alignment=1]()[0])
    sc = Float32(
        (w + sb + 192 + half * 8 + group * 2 + l // 16)
        .bitcast[Int8]()
        .load[width=1, alignment=1]()[0]
    )
    byte = (w + sb + half * 64 + (group % 2) * 32 + l)[0]
    var nib: Int
    if group < 2:
        nib = Int(byte & 0x0F)
    else:
        nib = Int(byte >> 4)
    hb = (w + sb + 128 + half * 32 + l)[0]
    bits = Int((hb >> UInt8(group * 2)) & 3)
    return Float32((nib | (bits << 4)) - 32) * (d * sc)


def run(ctx: DeviceContext, n_rows: Int, n_cols: Int, n_tokens: Int, tile: Int) raises:
    sbs = n_cols // SUPERBLOCK
    var wh = ctx.enqueue_create_host_buffer[DType.uint8](n_rows * sbs * SB_BYTES)
    var xh = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_cols)
    ctx.synchronize()
    for i in range(n_rows * sbs * SB_BYTES):
        wh[i] = UInt8(Int(random_si64(0, 255)))
    # `d` male i dodatnie, skale w waskim zakresie ze znakiem — inaczej
    # referencja f32 i akumulacja f32 rozjezdzaja sie na samym zakresie, a nie
    # na kernelu.
    for r in range(n_rows):
        for s in range(sbs):
            base = r * sbs * SB_BYTES + s * SB_BYTES
            wh[base + 208] = 0x00
            wh[base + 209] = 0x2C  # f16 ~0.0625
            for k in range(16):
                wh[base + 192 + k] = UInt8(Int(random_si64(-8, 8)) & 0xFF)
    for i in range(n_tokens * n_cols):
        xh[i] = Float16(Float64(Int(random_si64(-4, 4))) * 0.25)

    var w = ctx.enqueue_create_buffer[DType.uint8](n_rows * sbs * SB_BYTES)
    var x = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_cols)
    var y = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(w, wh)
    ctx.enqueue_copy(x, xh)
    ctx.synchronize()

    if tile == 0:
        comptime BM = 32
        comptime BN = 64
        ctx.enqueue_function[gemm_q6_k_wmma_f16_bm32](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(),
            n_cols, n_rows, n_tokens,
            grid_dim=((n_rows + BN - 1) // BN, (n_tokens + BM - 1) // BM),
            block_dim=128,
        )
    elif tile == 1:
        comptime BM = 256
        comptime BN = 64
        ctx.enqueue_function[gemm_q6_k_wmma_f16_bm256](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(),
            n_cols, n_rows, n_tokens,
            grid_dim=((n_rows + BN - 1) // BN, (n_tokens + BM - 1) // BM),
            block_dim=256,
        )
    elif tile == 2:
        comptime BM = 256
        comptime BN = 128
        ctx.enqueue_function[gemm_q6_k_wmma_f16_bm256_bn128](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(),
            n_cols, n_rows, n_tokens,
            grid_dim=((n_rows + BN - 1) // BN, (n_tokens + BM - 1) // BM),
            block_dim=256,
        )
    else:
        comptime BM = 512
        comptime BN = 128
        ctx.enqueue_function[gemm_q6_k_wmma_f16_bm512_bn128](
            y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(),
            n_cols, n_rows, n_tokens,
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
                acc += reference(wh.unsafe_ptr(), r, c, n_cols) * Float32(
                    xh[t * n_cols + c]
                )
            got = Float64(Float32(yh[t * n_rows + r]))
            diff = abs(got - Float64(acc))
            if diff > worst:
                worst = diff
                worst_ref = Float64(acc)
    var label = String("kafel ") + String(tile)
    print(
        label,
        "rows=",
        n_rows,
        "cols=",
        n_cols,
        "T=",
        n_tokens,
        "| najgorsza roznica",
        worst,
        "przy referencji",
        worst_ref,
    )


def main() raises:
    seed(20260730)
    var ctx = DeviceContext()
    for tile in range(4):
        run(ctx, 128, 512, 300, tile)
        run(ctx, 70, 768, 17, tile)
        run(ctx, 64, 256, 32, tile)
