# =============================================================================
# Plik: test_gemm_wmma.mojo
# Opis: Test złoty GEMM-u Q8_0 na WMMA — porównuje z tą samą matematyką liczoną
#       na hoście w int32 i f32, na kształtach z ogonami po T i po wierszach.
# Przykład: pixi run mojo run -I . test_gemm_wmma.mojo
# =============================================================================
from std.gpu.host import DeviceContext
from std.random import random_si64, seed
from std.memory import bitcast

from src.gemm_wmma import (
    gemm_q8_0_wmma_64x128,
    gemm_q8_0_wmma_f16_grouped,
    gemm_q8_0_wmma_f16_grouped_bm128_bn64,
)

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

    var wb_alt = ctx.enqueue_create_host_buffer[DType.uint8](n_rows * blocks * QBYTES)
    for i in range(n_rows * blocks * QBYTES):
        wb_alt[i] = wb[i]
    for r in range(n_rows):
        for b in range(blocks):
            base = (r * blocks + b) * QBYTES + 2
            wb_alt[base] = UInt8(0) - wb_alt[base]

    var wd = ctx.enqueue_create_buffer[DType.uint8](n_rows * blocks * QBYTES)
    var wd_alt = ctx.enqueue_create_buffer[DType.uint8](n_rows * blocks * QBYTES)
    var xqd = ctx.enqueue_create_buffer[DType.int8](n_tokens * n_cols)
    var xdd = ctx.enqueue_create_buffer[DType.float32](blocks * n_tokens)
    var xsm = ctx.enqueue_create_buffer[DType.float32](blocks * n_tokens)
    var yd = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(wd, wb)
    ctx.enqueue_copy(wd_alt, wb_alt)
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

    # Granice kafli obejmują także niepełny ogon ostatniego zakresu grouped.
    tiles = (n_tokens + 63) // 64
    var grouped = ctx.enqueue_create_buffer[DType.float16](n_tokens * n_rows)
    var table = ctx.enqueue_create_buffer[DType.uint64](2)
    var tile_expert = ctx.enqueue_create_buffer[DType.int32](tiles)
    var tile_first = ctx.enqueue_create_buffer[DType.int32](tiles)
    var tile_end = ctx.enqueue_create_buffer[DType.int32](tiles)
    with table.map_to_host() as host:
        host[0] = UInt64(Int(wd.unsafe_ptr()))
        host[1] = UInt64(Int(wd_alt.unsafe_ptr()))
    with tile_expert.map_to_host() as expert, tile_first.map_to_host() as first, tile_end.map_to_host() as end:
        for tile in range(tiles):
            expert[tile] = Int32(tile % 2)
            first[tile] = Int32(tile * 64)
            end[tile] = Int32(min((tile + 1) * 64, n_tokens))
    ctx.enqueue_function[gemm_q8_0_wmma_f16_grouped](
        grouped.unsafe_ptr(),
        table.unsafe_ptr().bitcast[UnsafePointer[UInt8, MutAnyOrigin]](),
        xqd.unsafe_ptr(),
        xdd.unsafe_ptr(),
        xsm.unsafe_ptr(),
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
    ctx.enqueue_function[gemm_q8_0_wmma_f16_grouped_bm128_bn64](
        grouped_wide.unsafe_ptr(),
        table.unsafe_ptr().bitcast[UnsafePointer[UInt8, MutAnyOrigin]](),
        xqd.unsafe_ptr(), xdd.unsafe_ptr(), xsm.unsafe_ptr(),
        wide_expert.unsafe_ptr(), wide_first.unsafe_ptr(), wide_end.unsafe_ptr(),
        n_cols, n_rows, n_tokens,
        grid_dim=((n_rows + 63) // 64, wide_tiles), block_dim=256,
    )
    ctx.synchronize()

    var yh = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_rows)
    var grouped_h = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_rows)
    var grouped_wide_h = ctx.enqueue_create_host_buffer[DType.float16](n_tokens * n_rows)
    ctx.enqueue_copy(yh, yd)
    ctx.enqueue_copy(grouped_h, grouped)
    ctx.enqueue_copy(grouped_wide_h, grouped_wide)
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
            narrow_w = wb.unsafe_ptr() if (t // 64) % 2 == 0 else wb_alt.unsafe_ptr()
            wide_w = wb.unsafe_ptr() if (t // 128) % 2 == 0 else wb_alt.unsafe_ptr()
            var narrow_want: Float64 = 0.0
            var wide_want: Float64 = 0.0
            for b in range(blocks):
                var narrow_sum: Int32 = 0
                var wide_sum: Int32 = 0
                for i in range(QK):
                    narrow_base = (r * blocks + b) * QBYTES
                    wide_base = narrow_base
                    nwq = Int32(bitcast[DType.int8, 1](SIMD[DType.uint8, 1](narrow_w[narrow_base + 2 + i]))[0])
                    wwq = Int32(bitcast[DType.int8, 1](SIMD[DType.uint8, 1](wide_w[wide_base + 2 + i]))[0])
                    xv = Int32(xqb[t * n_cols + b * QK + i])
                    narrow_sum += nwq * xv
                    wide_sum += wwq * xv
                base = (r * blocks + b) * QBYTES
                bits = UInt16(wb[base]) | (UInt16(wb[base + 1]) << 8)
                scale = bitcast[DType.float16, 1](SIMD[DType.uint16, 1](bits))[0]
                narrow_want += Float64(Float32(narrow_sum) * Float32(scale) * xdb[b * n_tokens + t])
                wide_want += Float64(Float32(wide_sum) * Float32(scale) * xdb[b * n_tokens + t])
            if abs(Float64(grouped_h[t * n_rows + r]) - narrow_want) > 0.1:
                raise Error("grouped narrow Q8_0 WMMA rozni sie od referencji")
            if abs(Float64(grouped_wide_h[t * n_rows + r]) - wide_want) > 0.1:
                raise Error("grouped wide Q8_0 WMMA rozni sie od referencji")
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
    build_and_check(ctx, 129, 70, 128)
    print("GEMM Q8_0 WMMA: wszystkie ksztalty zgodne z referencja")
