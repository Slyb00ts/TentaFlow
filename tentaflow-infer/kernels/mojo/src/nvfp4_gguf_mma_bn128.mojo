# =============================================================================
# Plik: nvfp4_gguf_mma_bn128.mojo
# Opis: Dokladny raw pipeline NVFP4 FP16 MMA z jedna bariera na etap K32.
# Przyklad: warianty BN64 i BN128 licza prefill bez requantyzacji.
# =============================================================================

from std.gpu import WARP_SIZE, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace, async_copy_commit_group, async_copy_wait_group
from std.gpu.compute.mma import mma, ld_matrix
from std.memory import stack_allocation
from src.gemm import _issue_x
from src.nvfp4_gguf_batch import _ue4m3_value

comptime BM = 128
comptime BK = 32
comptime LDK = 40
comptime LDW = 40
comptime NW = 8


def gemm_nvfp4_gguf_mma_sync1_impl[BN: Int, N_TILES: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    output_scale: Float32,
):
    """Liczy dokladny GEMM z surowego GGUF NVFP4 bez dodatkowej kwantyzacji.

    Grid ma kafle BN na BM, blok 256 watkow, a K jest wielokrotnoscia 64.
    Podwojny bufor K32 naklada cp.async aktywacji na dekodowanie wag.
    """
    comptime XTILE = BM * LDK
    comptime WTILE = BN * LDW
    comptime THREADS = NW * WARP_SIZE
    comptime M_WARPS = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    warp_id = tid // WARP_SIZE
    row0 = Int(block_idx.x) * BN
    token0 = Int(block_idx.y) * BM

    xs = stack_allocation[
        2 * XTILE, Float16, address_space=AddressSpace.SHARED
    ]()
    ws = stack_allocation[
        2 * WTILE, Float16, address_space=AddressSpace.SHARED
    ]()
    lut = stack_allocation[16, Float32, address_space=AddressSpace.SHARED]()
    comptime e2m1_values = SIMD[DType.float32, 16](
        0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0,
        -0.0, -0.5, -1.0, -1.5, -2.0, -3.0, -4.0, -6.0,
    )
    if tid < 16:
        lut[tid] = e2m1_values[tid]
    barrier()

    x_row = tid // 4
    column8 = (tid % 4) * 8
    var source_token0 = token0 + x_row
    if source_token0 > n_tokens - 1:
        source_token0 = n_tokens - 1
    var source_token1 = source_token0 + 64
    if source_token1 > n_tokens - 1:
        source_token1 = n_tokens - 1
    x_source0 = (x + source_token0 * n_cols + column8).address_space_cast[
        AddressSpace.GLOBAL
    ]()
    x_source1 = (x + source_token1 * n_cols + column8).address_space_cast[
        AddressSpace.GLOBAL
    ]()
    x_target0 = xs + x_row * LDK + column8
    x_target1 = x_target0 + 64 * LDK

    blocks_per_row = n_cols // 64
    weight_row = tid // 2
    weight_subblock = tid % 2
    var source_row = row0 + weight_row
    if source_row > n_rows - 1:
        source_row = n_rows - 1
    weight_base = weights + source_row * blocks_per_row * 36
    weight_target = ws + weight_row * LDW + weight_subblock * 16

    group = lane >> 2
    lane4 = lane & 3
    warp_m = (warp_id % M_WARPS) * 32
    warp_n = (warp_id // M_WARPS) * (N_TILES * 8)
    sub = lane // 8
    lane8 = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lane8) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lane8) * LDW + (sub % 2) * 8

    var accumulators = InlineArray[SIMD[DType.float32, 4], 2 * N_TILES](
        fill=SIMD[DType.float32, 4](0.0)
    )
    stages = n_cols // BK

    def fetch_raw(
        stage: Int,
        weight_subblock: Int,
        weight_base: UnsafePointer[UInt8, MutAnyOrigin],
        mut raw_codes: SIMD[DType.uint8, 8],
        mut raw_scale: UInt8,
    ):
        block = stage // 2
        scale_group = (stage % 2) * 2 + weight_subblock
        base = weight_base + block * 36
        raw_codes = (base + 4 + scale_group * 8).load[
            width=8, alignment=4
        ]()
        raw_scale = base[scale_group]

    _issue_x[BM, NW](
        0, n_cols, column8, x_source0, x_source1, x_target0, x_target1
    )
    async_copy_commit_group()
    var raw_codes = SIMD[DType.uint8, 8](0)
    var raw_scale = UInt8(0)
    if weight_row < BN:
        fetch_raw(0, weight_subblock, weight_base, raw_codes, raw_scale)
    async_copy_wait_group(0)

    if weight_row < BN:
        low_codes = raw_codes & 0x0F
        high_codes = raw_codes >> 4
        var decoded_low = SIMD[DType.float32, 8]()
        var decoded_high = SIMD[DType.float32, 8]()
        comptime for element in range(8):
            decoded_low[element] = lut[Int(low_codes[element])]
            decoded_high[element] = lut[Int(high_codes[element])]
        scale = _ue4m3_value(raw_scale)
        weight_target.store[width=8, alignment=16](
            (decoded_low * scale).cast[DType.float16]()
        )
        (weight_target + 8).store[width=8, alignment=16](
            (decoded_high * scale).cast[DType.float16]()
        )
    barrier()

    var stage = 0
    while stage < stages:
        if stage + 1 < stages:
            _issue_x[BM, NW](
                stage + 1, n_cols, column8, x_source0, x_source1,
                x_target0, x_target1,
            )
            async_copy_commit_group()
            if weight_row < BN:
                fetch_raw(
                    stage + 1, weight_subblock, weight_base,
                    raw_codes, raw_scale,
                )

        buffer = stage % 2
        comptime for k16 in range(BK // 16):
            comptime k_offset = k16 * 16
            a0 = ld_matrix[8](a_base + buffer * XTILE + k_offset)
            a1 = ld_matrix[8](a_base + buffer * XTILE + k_offset + 16 * LDK)
            comptime for n_tile in range(N_TILES):
                b = ld_matrix[4](
                    b_base + buffer * WTILE + n_tile * 8 * LDW + k_offset
                )
                mma(
                    accumulators[n_tile], a0, b, accumulators[n_tile]
                )
                mma(
                    accumulators[N_TILES + n_tile], a1, b,
                    accumulators[N_TILES + n_tile],
                )
        if stage + 1 < stages:
            next_buffer = (stage + 1) % 2
            if weight_row < BN:
                low_codes = raw_codes & 0x0F
                high_codes = raw_codes >> 4
                var decoded_low = SIMD[DType.float32, 8]()
                var decoded_high = SIMD[DType.float32, 8]()
                comptime for element in range(8):
                    decoded_low[element] = lut[Int(low_codes[element])]
                    decoded_high[element] = lut[Int(high_codes[element])]
                scale = _ue4m3_value(raw_scale)
                (weight_target + next_buffer * WTILE).store[
                    width=8, alignment=16
                ]((decoded_low * scale).cast[DType.float16]())
                (weight_target + next_buffer * WTILE + 8).store[
                    width=8, alignment=16
                ]((decoded_high * scale).cast[DType.float16]())
            async_copy_wait_group(0)
            barrier()
        stage += 1

    if output_scale != 1.0:
        comptime for index in range(2 * N_TILES):
            accumulators[index] *= output_scale
    comptime for m_tile in range(2):
        token = token0 + warp_m + m_tile * 16 + group
        comptime for n_tile in range(N_TILES):
            row = row0 + warp_n + n_tile * 8 + lane4 * 2
            output_values = accumulators[m_tile * N_TILES + n_tile]
            if token < n_tokens and row < n_rows:
                y[token * n_rows + row] = Float16(output_values[0])
                if row + 1 < n_rows:
                    y[token * n_rows + row + 1] = Float16(output_values[1])
            if token + 8 < n_tokens and row < n_rows:
                y[(token + 8) * n_rows + row] = Float16(output_values[2])
                if row + 1 < n_rows:
                    y[(token + 8) * n_rows + row + 1] = Float16(output_values[3])


comptime gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1 = (
    gemm_nvfp4_gguf_mma_sync1_impl[64, 4]
)
comptime gemm_nvfp4_gguf_mma_f16_bm128_bn128 = (
    gemm_nvfp4_gguf_mma_sync1_impl[128, 8]
)
