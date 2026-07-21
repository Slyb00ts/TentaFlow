# =============================================================================
# Plik: nvfp4_gguf_mma.mojo
# Opis: Kafelkowany GEMM MMA NVIDIA czytajacy bezposrednio bloki GGUF NVFP4.
# Przyklad: gemm_nvfp4_gguf_mma_f16_bm128 obsluguje dlugi prefill bez repacku.
# =============================================================================

from std.gpu import WARP_SIZE, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace, async_copy_commit_group, async_copy_wait_group
from std.gpu.compute.mma import mma, ld_matrix
from std.memory import stack_allocation
from src.gemm import _issue_x, _store_tile
from src.gemv2 import _e2m1x8
from src.nvfp4_gguf_batch import _ue4m3_value

comptime BN = 64
comptime BK = 32
comptime LDK = 40
comptime LDW = 40
comptime WTILE = BN * LDW


def gemm_nvfp4_gguf_mma_impl[BM: Int, NW: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    output_scale: Float32,
):
    """Liczy GEMM na tensor cores NVIDIA z dekwantyzacja wag w rejestrach.

    Grid ma ksztalt (ceil(n_rows / 64), ceil(n_tokens / BM)), blok NW*WARP_SIZE,
    a n_cols musi byc wielokrotnoscia 64. Format zrodla pozostaje GGUF NVFP4.
    """
    comptime XTILE = BM * LDK
    comptime NT = NW * WARP_SIZE
    comptime x_rows_per_pass = NT // 4
    comptime weight_passes = (BN * 4) // NT
    comptime m_warps = BM // 32

    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    warp_id = tid // WARP_SIZE
    row0 = Int(block_idx.x) * BN
    token0 = Int(block_idx.y) * BM

    xs = stack_allocation[2 * XTILE, Float16, address_space=AddressSpace.SHARED]()
    ws = stack_allocation[2 * WTILE, Float16, address_space=AddressSpace.SHARED]()

    x_row = tid // 4
    column8 = (tid % 4) * 8
    var source_token0 = token0 + x_row
    if source_token0 > n_tokens - 1:
        source_token0 = n_tokens - 1
    var source_token1 = token0 + x_row + x_rows_per_pass
    if source_token1 > n_tokens - 1:
        source_token1 = n_tokens - 1
    x_source0 = (x + source_token0 * n_cols + column8).address_space_cast[
        AddressSpace.GLOBAL
    ]()
    x_source1 = (x + source_token1 * n_cols + column8).address_space_cast[
        AddressSpace.GLOBAL
    ]()
    x_target0 = xs + x_row * LDK + column8
    x_target1 = x_target0 + x_rows_per_pass * LDK

    part = tid % 4
    blocks_per_row = n_cols // 64
    var weight_base = InlineArray[
        UnsafePointer[UInt8, MutAnyOrigin], weight_passes
    ](uninitialized=True)
    comptime for weight_pass in range(weight_passes):
        var source_row = row0 + tid // 4 + weight_pass * (NT // 4)
        if source_row > n_rows - 1:
            source_row = n_rows - 1
        weight_base[weight_pass] = weights + source_row * blocks_per_row * 36
    weight_target = ws + (tid // 4) * LDW + part * 8

    group = lane >> 2
    lane4 = lane & 3
    warp_m = (warp_id % m_warps) * 32
    warp_n = (warp_id // m_warps) * 32
    sub = lane // 8
    lane8 = lane % 8
    a_base = xs + (warp_m + (sub % 2) * 8 + lane8) * LDK + (sub // 2) * 8
    b_base = ws + (warp_n + lane8) * LDW + (sub % 2) * 8

    var accumulators = InlineArray[SIMD[DType.float32, 4], 8](
        fill=SIMD[DType.float32, 4](0.0)
    )
    stages = n_cols // BK

    def fetch_weight(
        stage: Int,
        part: Int,
        weight_base: InlineArray[
            UnsafePointer[UInt8, MutAnyOrigin], weight_passes
        ],
        mut codes: InlineArray[SIMD[DType.uint8, 8], weight_passes],
        mut scales: InlineArray[Float32, weight_passes],
    ):
        block = stage // 2
        scale_group = (stage % 2) * 2 + part // 2
        comptime for weight_pass in range(weight_passes):
            base = weight_base[weight_pass] + block * 36
            raw = (base + 4 + scale_group * 8).load[width=8, alignment=4]()
            codes[weight_pass] = raw & 0x0F if part % 2 == 0 else raw >> 4
            scales[weight_pass] = _ue4m3_value(base[scale_group])

    _issue_x[BM, NW](
        0, n_cols, column8, x_source0, x_source1, x_target0, x_target1
    )
    async_copy_commit_group()
    var codes = InlineArray[SIMD[DType.uint8, 8], weight_passes](
        fill=SIMD[DType.uint8, 8](0)
    )
    var scales = InlineArray[Float32, weight_passes](fill=0.0)
    fetch_weight(0, part, weight_base, codes, scales)

    var stage = 0
    while stage < stages:
        if stage + 1 < stages:
            _issue_x[BM, NW](
                stage + 1, n_cols, column8, x_source0, x_source1,
                x_target0, x_target1,
            )
            async_copy_commit_group()

        comptime for weight_pass in range(weight_passes):
            values = (_e2m1x8(codes[weight_pass]) * scales[weight_pass]).cast[
                DType.float16
            ]()
            (weight_target + (stage % 2) * WTILE + weight_pass * (NT // 4) * LDW).store[
                width=8, alignment=16
            ](values)
        if stage + 1 < stages:
            fetch_weight(stage + 1, part, weight_base, codes, scales)
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()

        buffer = stage % 2
        comptime for k16 in range(BK // 16):
            comptime k_offset = k16 * 16
            a0 = ld_matrix[8](a_base + buffer * XTILE + k_offset)
            a1 = ld_matrix[8](a_base + buffer * XTILE + k_offset + 16 * LDK)
            comptime for n8 in range(4):
                b = ld_matrix[4](
                    b_base + buffer * WTILE + n8 * 8 * LDW + k_offset
                )
                mma(accumulators[n8], a0, b, accumulators[n8])
                mma(accumulators[4 + n8], a1, b, accumulators[4 + n8])
        barrier()
        stage += 1

    if output_scale != 1.0:
        comptime for i in range(8):
            accumulators[i] *= output_scale
    _store_tile(
        y, accumulators, token0, row0, warp_m, warp_n, group, lane4,
        n_rows, n_tokens,
    )


comptime gemm_nvfp4_gguf_mma_f16_bm32 = gemm_nvfp4_gguf_mma_impl[32, 2]
comptime gemm_nvfp4_gguf_mma_f16_bm128 = gemm_nvfp4_gguf_mma_impl[128, 8]
