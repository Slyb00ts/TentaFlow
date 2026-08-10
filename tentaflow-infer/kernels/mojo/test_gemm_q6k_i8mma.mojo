# ===== File: test_gemm_q6k_i8mma.mojo — int8 tensor-core Q6_K prefill GEMM =====
# Q6_K changes its scale every SIXTEEN columns while the int8 mma consumes
# thirty-two, so the tile runs two half-k products on the same fragments. That
# is the part worth proving: the kernel is compared against a CPU reference that
# rebuilds every code from the raw ql/qh/scales bytes and quantizes activations
# exactly as `quantize_act_q8_1` does, so any error in the ql/qh split, the
# scale pairing or the fragment halves shows up as a mismatch.
#
# Second contract: bm64 and bm128 must be BIT-identical — same per-element chain.
from std.gpu.host import DeviceContext
from src.gemm import gemm_q6_k_i8mma, gemm_q6_k_i8mma_bm64
from src.gemm import quantize_act_q8_1

comptime KERNEL_TOL: Float32 = 0.01


def _fill(i: Int) -> Float32:
    seed = (UInt32(i) * 2654435761 + 1013904223) & 0xFFFFFFFF
    return Float32(seed) * (2.0 / 4294967296.0) - 1.0


def _quant_block(
    xh: UnsafePointer[Float16, MutUntrackedOrigin], base: Int
) -> Tuple[InlineArray[Int32, 32], Float32]:
    var q = InlineArray[Int32, 32](fill=0)
    var amax: Float32 = 0.0
    for k in range(32):
        v = abs(Float32(xh[base + k]))
        if v > amax:
            amax = v
    if amax == 0.0:
        return (q, Float32(0.0))
    for k in range(32):
        q[k] = Int32(round(Float32(xh[base + k]) * (127.0 / amax)))
    return (q, amax / 127.0)


def main() raises:
    var ctx = DeviceContext()
    comptime T = 100  # not a multiple of 128 or 64: exercises the token guard
    comptime ROWS = 70  # not a multiple of 64: exercises the row guard
    comptime COLS = 512  # two superblocks per row

    var x = ctx.enqueue_create_buffer[DType.float16](T * COLS)
    var w = ctx.enqueue_create_buffer[DType.uint8](ROWS * (COLS // 256) * 210)
    var y128 = ctx.enqueue_create_buffer[DType.float16](T * ROWS)
    var y64 = ctx.enqueue_create_buffer[DType.float16](T * ROWS)
    var xq = ctx.enqueue_create_buffer[DType.int8](T * COLS)
    var xd = ctx.enqueue_create_buffer[DType.float32](T * (COLS // 32))
    var xs = ctx.enqueue_create_buffer[DType.float32](T * (COLS // 32))

    with x.map_to_host() as h:
        for i in range(T * COLS):
            h[i] = Float16(_fill(i))
    with w.map_to_host() as h:
        for r in range(ROWS):
            for b in range(COLS // 256):
                off = (r * (COLS // 256) + b) * 210
                for i in range(128):
                    h[off + i] = UInt8((r * 7 + b * 13 + i * 3) % 256)
                for i in range(64):
                    h[off + 128 + i] = UInt8((r * 11 + b * 5 + i * 17) % 256)
                for i in range(16):
                    # Signed scales, both polarities, never zero.
                    # Signed scales of both polarities, never zero.
                    v = ((r + b + i) % 9) - 4
                    h[off + 192 + i] = UInt8(v + 1 if v >= 0 else v + 256)
                d = Float16(0.004 + Float32((r + b) % 5) * 0.001)
                bits = d.to_bits()
                h[off + 208] = UInt8(bits & 0xFF)
                h[off + 209] = UInt8((bits >> 8) & 0xFF)

    nbq = (T * (COLS // 32) + 255) // 256
    ctx.enqueue_function[quantize_act_q8_1](
        xq.unsafe_ptr(), xd.unsafe_ptr(), xs.unsafe_ptr(), x.unsafe_ptr(),
        COLS, T, grid_dim=nbq, block_dim=256,
    )
    ctx.enqueue_function[gemm_q6_k_i8mma](
        y128.unsafe_ptr(), w.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
        xs.unsafe_ptr(), COLS, ROWS, T,
        grid_dim=((ROWS + 63) // 64, (T + 127) // 128), block_dim=256,
    )
    ctx.enqueue_function[gemm_q6_k_i8mma_bm64](
        y64.unsafe_ptr(), w.unsafe_ptr(), xq.unsafe_ptr(), xd.unsafe_ptr(),
        xs.unsafe_ptr(), COLS, ROWS, T,
        grid_dim=((ROWS + 63) // 64, (T + 63) // 64), block_dim=256,
    )
    ctx.synchronize()

    var worst: Float32 = 0.0
    var bit_mismatch = 0
    with x.map_to_host() as xh, w.map_to_host() as wh, y128.map_to_host() as h128, y64.map_to_host() as h64:
        for t in range(T):
            for r in range(ROWS):
                var acc: Float32 = 0.0
                for b in range(COLS // 256):
                    off = (r * (COLS // 256) + b) * 210
                    dsb = Float32(Float16(0.004 + Float32((r + b) % 5) * 0.001))
                    for sub in range(8):
                        n = sub // 4
                        m = sub % 4
                        blk = _quant_block(xh.unsafe_ptr(), t * COLS + b * 256 + sub * 32)
                        qb = blk[0]
                        dx = blk[1]
                        for l in range(32):
                            ql = Int(wh[off + n * 64 + l + (32 if m % 2 == 1 else 0)])
                            qh = Int(wh[off + 128 + n * 32 + l])
                            shift = 4 * (m // 2)
                            code = ((ql >> shift) & 0x0F) | (
                                ((qh >> (2 * m)) & 3) << 4
                            )
                            var sc = Int(wh[off + 192 + n * 8 + 2 * m + l // 16])
                            if sc > 127:
                                sc -= 256
                            acc += (
                                dsb
                                * Float32(sc)
                                * dx
                                * Float32(code - 32)
                                * Float32(Int(qb[l]))
                            )
                got = Float32(h128[t * ROWS + r])
                err = abs(got - acc) / (abs(acc) + 1e-3)
                if err > worst:
                    worst = err
                if h128[t * ROWS + r].to_bits() != h64[t * ROWS + r].to_bits():
                    bit_mismatch += 1

    print("q6_k i8mma vs CPU MMQ: max rel err", worst)
    print("bm128 vs bm64 mismatches:", bit_mismatch)
    if worst > KERNEL_TOL:
        raise Error("kernel Q6_K i8mma rozjeżdża się z referencją CPU")
    if bit_mismatch != 0:
        raise Error("bm64 nie jest bitowo zgodny z bm128")
    print("OK")
