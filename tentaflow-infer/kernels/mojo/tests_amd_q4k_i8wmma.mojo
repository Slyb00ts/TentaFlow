# Golden test kafla Q4_K na WMMA int8. Referencja liczy iloczyn na SKWANTOWANYCH
# aktywacjach (xd * xq), zeby mierzyc kernel, a nie blad kwantyzacji q8_1.
from std.gpu.host import DeviceContext
from std.random import random_si64, seed

from src.gemm_q4_k_i8wmma import (
    gemm_q4_k_i8wmma_f16_bm32,
    gemm_q4_k_i8wmma_f16_bm256,
    gemm_q4_k_i8wmma_f16_grouped,
    gemm_q4_k_i8wmma_f16_grouped_bm128_bn64,
)

comptime SB_BYTES = 144
comptime SUPERBLOCK = 256


def scale_min(s: UnsafePointer[UInt8, MutUntrackedOrigin], j: Int) -> Tuple[Float32, Float32]:
    if j < 4:
        return (Float32(Int(s[4 + j] & 63)), Float32(Int(s[8 + j] & 63)))
    return (
        Float32(Int((s[8 + j] & 0x0F) | ((s[4 + j - 4] >> 6) << 4))),
        Float32(Int((s[8 + j] >> 4) | ((s[4 + j] >> 6) << 4))),
    )


def weight(w: UnsafePointer[UInt8, MutUntrackedOrigin], row: Int, col: Int, n_cols: Int) -> Float32:
    sb = row * (n_cols // SUPERBLOCK) * SB_BYTES + (col // SUPERBLOCK) * SB_BYTES
    r = col % SUPERBLOCK
    d = Float32((w + sb).bitcast[Float16]().load[width=1, alignment=1]()[0])
    dmin = Float32((w + sb + 2).bitcast[Float16]().load[width=1, alignment=1]()[0])
    sc, mn = scale_min(w + sb, r // 32)
    byte = (w + sb + 16 + (r // 64) * 32 + (r % 32))[0]
    var nib: Int
    if (r % 64) < 32:
        nib = Int(byte & 0x0F)
    else:
        nib = Int(byte >> 4)
    return Float32(nib) * (d * sc) - dmin * mn


def run(ctx: DeviceContext, n_rows: Int, n_cols: Int, n_tokens: Int, big: Bool) raises:
    sbs = n_cols // SUPERBLOCK
    nb = n_cols // 32
    var wh = ctx.enqueue_create_host_buffer[DType.uint8](n_rows * sbs * SB_BYTES)
    var xf = ctx.enqueue_create_host_buffer[DType.float32](n_tokens * n_cols)
    var xqh = ctx.enqueue_create_host_buffer[DType.int8](n_tokens * n_cols)
    var xdh = ctx.enqueue_create_host_buffer[DType.float32](nb * n_tokens)
    var xsh = ctx.enqueue_create_host_buffer[DType.float32](nb * n_tokens)
    ctx.synchronize()
    for i in range(n_rows * sbs * SB_BYTES):
        wh[i] = UInt8(Int(random_si64(0, 255)))
    for r in range(n_rows):
        for s in range(sbs):
            base = r * sbs * SB_BYTES + s * SB_BYTES
            wh[base] = 0x00
            wh[base + 1] = 0x2C      # d ~0.0625
            wh[base + 2] = 0x00
            wh[base + 3] = 0x28      # dmin ~0.03125
    for i in range(n_tokens * n_cols):
        xf[i] = Float32(Int(random_si64(-40, 40))) * 0.05

    # kwantyzacja aktywacji dokladnie jak `quantize_act_q8_1`
    for t in range(n_tokens):
        for b in range(nb):
            var amax: Float32 = 0.0
            for e in range(32):
                v = abs(xf[t * n_cols + b * 32 + e])
                if v > amax:
                    amax = v
            var d: Float32 = 0.0
            var sumq: Int = 0
            if amax > 0.0:
                d = amax / 127.0
                for e in range(32):
                    q = Int(round(xf[t * n_cols + b * 32 + e] * (127.0 / amax)))
                    xqh[t * n_cols + b * 32 + e] = Int8(q)
                    sumq += q
            else:
                for e in range(32):
                    xqh[t * n_cols + b * 32 + e] = Int8(0)
            xdh[b * n_tokens + t] = d
            xsh[b * n_tokens + t] = d * Float32(sumq)

    var wh_alt = ctx.enqueue_create_host_buffer[DType.uint8](n_rows * sbs * SB_BYTES)
    for i in range(n_rows * sbs * SB_BYTES):
        wh_alt[i] = wh[i]
    for r in range(n_rows):
        for s in range(sbs):
            base = r * sbs * SB_BYTES + s * SB_BYTES + 16
            wh_alt[base] = (wh_alt[base] & 0xF0) | ((wh_alt[base] + 3) & 0x0F)

    var w = ctx.enqueue_create_buffer[DType.uint8](n_rows * sbs * SB_BYTES)
    var w_alt = ctx.enqueue_create_buffer[DType.uint8](n_rows * sbs * SB_BYTES)
    var xq = ctx.enqueue_create_buffer[DType.int8](n_tokens * n_cols)
    var xd = ctx.enqueue_create_buffer[DType.float32](nb * n_tokens)
    var xs = ctx.enqueue_create_buffer[DType.float32](nb * n_tokens)
    var y = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(w, wh)
    ctx.enqueue_copy(w_alt, wh_alt)
    ctx.enqueue_copy(xq, xqh)
    ctx.enqueue_copy(xd, xdh)
    ctx.enqueue_copy(xs, xsh)
    ctx.synchronize()

    if big:
        comptime BM = 256
        comptime BN = 64
        ctx.enqueue_function[gemm_q4_k_i8wmma_f16_bm256](
            y.unsafe_ptr(), w.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
            xs.unsafe_ptr(), n_cols, n_rows, n_tokens,
            grid_dim=((n_rows + BN - 1) // BN, (n_tokens + BM - 1) // BM),
            block_dim=256,
        )
    else:
        comptime BM = 32
        comptime BN = 64
        ctx.enqueue_function[gemm_q4_k_i8wmma_f16_bm32](
            y.unsafe_ptr(), w.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
            xs.unsafe_ptr(), n_cols, n_rows, n_tokens,
            grid_dim=((n_rows + BN - 1) // BN, (n_tokens + BM - 1) // BM),
            block_dim=128,
        )
    ctx.synchronize()

    tiles = (n_tokens + 63) // 64
    var grouped = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_rows)
    var table = ctx.enqueue_create_buffer[DType.uint64](2)
    var tile_expert = ctx.enqueue_create_buffer[DType.int32](tiles)
    var tile_first = ctx.enqueue_create_buffer[DType.int32](tiles)
    var tile_end = ctx.enqueue_create_buffer[DType.int32](tiles)
    with table.map_to_host() as host:
        host[0] = UInt64(Int(w.unsafe_ptr()))
        host[1] = UInt64(Int(w_alt.unsafe_ptr()))
    with tile_expert.map_to_host() as expert, tile_first.map_to_host() as first, tile_end.map_to_host() as end:
        for tile in range(tiles):
            expert[tile] = Int32(tile % 2)
            first[tile] = Int32(tile * 64)
            end[tile] = Int32(min((tile + 1) * 64, n_tokens))
    ctx.enqueue_function[gemm_q4_k_i8wmma_f16_grouped](
        grouped.unsafe_ptr(),
        table.unsafe_ptr().bitcast[UnsafePointer[UInt8, MutAnyOrigin]](),
        xq.unsafe_ptr(),
        xd.unsafe_ptr(),
        xs.unsafe_ptr(),
        tile_expert.unsafe_ptr(),
        tile_first.unsafe_ptr(),
        tile_end.unsafe_ptr(),
        n_cols,
        n_rows,
        n_tokens,
        grid_dim=((n_rows + 63) // 64, tiles),
        block_dim=128,
    )
    ctx.synchronize()
    wide_tiles = (n_tokens + 127) // 128
    var grouped_wide = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_rows)
    var wide_expert = ctx.enqueue_create_buffer[DType.int32](wide_tiles)
    var wide_first = ctx.enqueue_create_buffer[DType.int32](wide_tiles)
    var wide_end = ctx.enqueue_create_buffer[DType.int32](wide_tiles)
    with wide_expert.map_to_host() as expert, wide_first.map_to_host() as first, wide_end.map_to_host() as end:
        for tile in range(wide_tiles):
            expert[tile] = Int32(tile % 2)
            first[tile] = Int32(tile * 128)
            end[tile] = Int32(min((tile + 1) * 128, n_tokens))
    ctx.enqueue_function[gemm_q4_k_i8wmma_f16_grouped_bm128_bn64](
        grouped_wide.unsafe_ptr(),
        table.unsafe_ptr().bitcast[UnsafePointer[UInt8, MutAnyOrigin]](),
        xq.unsafe_ptr(), xd.unsafe_ptr(), xs.unsafe_ptr(),
        wide_expert.unsafe_ptr(), wide_first.unsafe_ptr(), wide_end.unsafe_ptr(),
        n_cols, n_rows, n_tokens,
        grid_dim=((n_rows + 63) // 64, wide_tiles), block_dim=256,
    )
    ctx.synchronize()
    var yh = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_rows)
    var grouped_h = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_rows)
    var grouped_wide_h = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(yh, y)
    ctx.enqueue_copy(grouped_h, grouped)
    ctx.enqueue_copy(grouped_wide_h, grouped_wide)
    ctx.synchronize()

    var worst: Float64 = 0.0
    var worst_ref: Float64 = 0.0
    var max_ref: Float64 = 0.0
    for t in range(n_tokens):
        for r in range(n_rows):
            var acc: Float32 = 0.0
            for c in range(n_cols):
                xv = xdh[(c // 32) * n_tokens + t] * Float32(Int(xqh[t * n_cols + c]))
                acc += weight(wh.unsafe_ptr(), r, c, n_cols) * xv
            got = Float64(Float32(yh[t * n_rows + r]))
            narrow_w = wh.unsafe_ptr() if (t // 64) % 2 == 0 else wh_alt.unsafe_ptr()
            wide_w = wh.unsafe_ptr() if (t // 128) % 2 == 0 else wh_alt.unsafe_ptr()
            var narrow_acc: Float32 = 0.0
            var wide_acc: Float32 = 0.0
            for c in range(n_cols):
                xv = xdh[(c // 32) * n_tokens + t] * Float32(Int(xqh[t * n_cols + c]))
                narrow_acc += weight(narrow_w, r, c, n_cols) * xv
                wide_acc += weight(wide_w, r, c, n_cols) * xv
            if abs(Float32(grouped_h[t * n_rows + r]) - narrow_acc) > 1.1:
                raise Error("grouped narrow Q4_K WMMA rozni sie od referencji")
            if abs(Float32(grouped_wide_h[t * n_rows + r]) - wide_acc) > 1.1:
                raise Error("grouped wide Q4_K WMMA rozni sie od referencji")
            if abs(Float64(acc)) > max_ref:
                max_ref = abs(Float64(acc))
            diff = abs(got - Float64(acc))
            if diff > worst:
                worst = diff
                worst_ref = Float64(acc)
    var label = String("bm256") if big else String("bm32")
    print(label, "rows=", n_rows, "cols=", n_cols, "T=", n_tokens,
          "| najgorsza roznica", worst, "przy referencji", worst_ref,
          "| max |ref|", max_ref)


def main() raises:
    seed(20260729)
    var ctx = DeviceContext()
    run(ctx, 64, 256, 32, False)
    run(ctx, 128, 512, 40, False)
    run(ctx, 128, 512, 300, True)
    run(ctx, 70, 768, 17, False)
