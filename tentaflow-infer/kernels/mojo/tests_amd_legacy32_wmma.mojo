# Golden test kafla starych formatow 32-elementowych (Q4_0/Q4_1/Q5_0/Q5_1)
# wobec referencji liczonej na hoscie ta sama formula co `gemm_legacy32_impl`.
from std.gpu.host import DeviceContext
from std.random import random_si64, seed

from src.gemm_legacy32_wmma import (
    gemm_q4_0_wmma_f16_bm32,
    gemm_q4_1_wmma_f16_bm32,
    gemm_q5_0_wmma_f16_bm32,
    gemm_q5_1_wmma_f16_bm32,
)

comptime BLOCK = 32


def reference[FMT: Int](
    w: UnsafePointer[UInt8, MutUntrackedOrigin], row: Int, col: Int, n_cols: Int
) -> Float32:
    comptime BB = 18 + 2 * FMT
    comptime QS_OFF = 2 + (2 if FMT == 1 else 0) + (4 if FMT == 2 else 0) + (
        6 if FMT == 3 else 0
    )
    blk = row * (n_cols // BLOCK) * BB + (col // BLOCK) * BB
    e = col % BLOCK
    d = Float32((w + blk).bitcast[Float16]().load[width=1, alignment=1]()[0])
    byte = (w + blk + QS_OFF + (e % 16))[0]
    var q: Int
    if e < 16:
        q = Int(byte & 0x0F)
    else:
        q = Int(byte >> 4)
    comptime if FMT >= 2:
        comptime QH_OFF = 2 + (2 if FMT == 3 else 0)
        lo = UInt32((w + blk + QH_OFF).bitcast[UInt16]().load[width=1, alignment=1]()[0])
        hi = UInt32((w + blk + QH_OFF + 2).bitcast[UInt16]().load[width=1, alignment=1]()[0])
        qh = lo | (hi << 16)
        q += Int((qh >> UInt32(e)) & 1) * 16
    comptime if FMT == 0:
        q -= 8
    comptime if FMT == 2:
        q -= 16
    var out = Float32(q) * d
    comptime if FMT == 1 or FMT == 3:
        out += Float32((w + blk + 2).bitcast[Float16]().load[width=1, alignment=1]()[0])
    return out


def run[FMT: Int](ctx: DeviceContext, label: String, n_rows: Int, n_cols: Int, n_tokens: Int) raises:
    comptime BB = 18 + 2 * FMT
    blocks = n_cols // BLOCK
    var wh = ctx.enqueue_create_host_buffer[DType.uint8](n_rows * blocks * BB)
    var xh = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_cols)
    ctx.synchronize()
    for i in range(n_rows * blocks * BB):
        wh[i] = UInt8(Int(random_si64(0, 255)))
    # d (i m) male i dodatnie, zeby referencja f32 i akumulacja zostaly w
    # porownywalnym zakresie.
    for r in range(n_rows):
        for b in range(blocks):
            base = r * blocks * BB + b * BB
            wh[base] = 0x00
            wh[base + 1] = 0x2C          # d ~0.0625
            comptime if FMT == 1 or FMT == 3:
                wh[base + 2] = 0x00
                wh[base + 3] = 0x28      # m ~0.03125
    for i in range(n_tokens * n_cols):
        xh[i] = Float16(Float64(Int(random_si64(-4, 4))) * 0.25)

    var w = ctx.enqueue_create_buffer[DType.uint8](n_rows * blocks * BB)
    var x = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_cols)
    var y = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(w, wh)
    ctx.enqueue_copy(x, xh)
    ctx.synchronize()

    comptime BM = 32
    comptime BN = 64
    comptime kern = gemm_q4_0_wmma_f16_bm32 if FMT == 0 else (
        gemm_q4_1_wmma_f16_bm32 if FMT == 1 else (
            gemm_q5_0_wmma_f16_bm32 if FMT == 2 else gemm_q5_1_wmma_f16_bm32
        )
    )
    ctx.enqueue_function[kern](
        y.unsafe_ptr(), w.unsafe_ptr(), x.unsafe_ptr(),
        n_cols, n_rows, n_tokens,
        grid_dim=((n_rows + BN - 1) // BN, (n_tokens + BM - 1) // BM),
        block_dim=128,
    )
    var yh = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(yh, y)
    ctx.synchronize()

    var worst: Float64 = 0.0
    var worst_ref: Float64 = 0.0
    var max_ref: Float64 = 0.0
    var max_got: Float64 = 0.0
    for t in range(n_tokens):
        for r in range(n_rows):
            var acc: Float32 = 0.0
            for c in range(n_cols):
                acc += reference[FMT](wh.unsafe_ptr(), r, c, n_cols) * Float32(xh[t * n_cols + c])
            got = Float64(Float32(yh[t * n_rows + r]))
            diff = abs(got - Float64(acc))
            if abs(Float64(acc)) > max_ref:
                max_ref = abs(Float64(acc))
            if abs(got) > max_got:
                max_got = abs(got)
            if diff > worst:
                worst = diff
                worst_ref = Float64(acc)
    print(label, "rows=", n_rows, "cols=", n_cols, "T=", n_tokens,
          "| najgorsza roznica", worst, "przy referencji", worst_ref,
          "| max |ref|", max_ref, "max |gpu|", max_got)


def main() raises:
    seed(20260729)
    var ctx = DeviceContext()
    run[0](ctx, String("Q4_0"), 64, 256, 24)
    run[1](ctx, String("Q4_1"), 70, 512, 17)
    run[2](ctx, String("Q5_0"), 128, 256, 20)
    run[3](ctx, String("Q5_1"), 70, 768, 17)
