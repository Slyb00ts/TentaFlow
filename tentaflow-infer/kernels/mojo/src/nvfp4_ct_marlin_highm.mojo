# =============================================================================
# Plik: nvfp4_ct_marlin_highm.mojo
# Opis: Eksperymentalny persistent W4A16 BM64/BN256/BK64 dla prefill NVFP4 S0.
# Przykład: benchmark uruchamia 128 CTA dla pełnych kafli M64 i N256.
# =============================================================================

from std.gpu import WARP_SIZE, block_idx, grid_dim, thread_idx
from std.gpu.compute.mma import ld_matrix, mma
from std.gpu.memory import (
    AddressSpace,
    async_copy,
    async_copy_commit_group,
    async_copy_wait_group,
    external_memory,
)
from std.gpu.sync import barrier
from std.gpu.primitives import warp
from std.memory import bitcast
from src.gemm import _store_tile

comptime BM = 64
comptime BN = 256
comptime BK = 64
comptime PIPE_STAGES = 4
comptime LDA = 72
comptime A_STAGE = BM * LDA
comptime B_STAGE = BN * (BK // 2)
comptime S_STAGE = BN * (BK // 16)


def _canonical_scale_chunk_offset(row: Int) -> Int:
    permuted_index = (row % 8) * 8 + row // 8
    group4_base = (permuted_index // 4) * 4
    index4 = permuted_index % 4
    if index4 == 1:
        return group4_base + 2
    if index4 == 2:
        return group4_base + 1
    return group4_base + index4


def _decode_marlin_fragment(
    packed: UInt32,
    upper_n: Bool,
    scale: Float16,
) -> SIMD[DType.float16, 4]:
    var selected = packed
    if upper_n:
        selected >>= 8
    lower = (
        ((selected & 0x00070007) << 9)
        | ((selected & 0x00080008) << 12)
    )
    selected >>= 4
    upper = (
        ((selected & 0x00070007) << 9)
        | ((selected & 0x00080008) << 12)
    )
    values = bitcast[DType.float16, 4](
        SIMD[DType.uint32, 2](lower, upper)
    )
    return values * scale


def _decode_marlin_scale(encoded: UInt8) -> Float16:
    scale_bits = UInt16(encoded) << 7
    return bitcast[DType.float16, 1](
        SIMD[DType.uint16, 1](scale_bits)
    )[0]


def _decode_marlin_scales(packed: UInt32) -> SIMD[DType.float16, 4]:
    odd = (packed & 0xFF00FF00) >> 1
    even = ((packed << 8) & 0xFF00FF00) >> 1
    return bitcast[DType.float16, 4](
        SIMD[DType.uint32, 2](even, odd)
    )


def gemm_nvfp4_ct_marlin_bm16_bn64_bk128[
    n_cols: Int,
    n_rows: Int,
    valid_tokens: Int,
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy M4/M8/M16 z kanonicznych kafli Marlin bez dodatkowej kopii wag."""
    comptime assert n_cols % 128 == 0
    comptime assert n_rows % 64 == 0
    comptime assert 0 < valid_tokens <= 16
    comptime lda = 136
    comptime a_stage = 16 * lda
    comptime b_stage = 4096
    comptime s_stage = 512
    comptime stages = n_cols // 128
    comptime n_tiles64 = n_rows // 64
    comptime packed_bytes = n_rows * n_cols // 2

    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    warp_k = tid // WARP_SIZE
    lane_row = lane // 4
    tid4 = lane & 3
    sub = lane // 8
    lane8 = lane % 8
    row0 = Int(block_idx.x) * 64
    row_tile = row0 // 64
    canonical_scales = resident + packed_bytes

    shared = external_memory[
        UInt8, address_space=AddressSpace.SHARED, alignment=16
    ]()
    xs = shared.bitcast[Float16]()
    bs = shared + PIPE_STAGES * a_stage * 2
    ss = bs + PIPE_STAGES * b_stage
    a_base = (
        xs
        + ((sub % 2) * 8 + lane8) * lda
        + (sub // 2) * 8
    )

    def issue_stage(
        stage: Int,
        resident: UnsafePointer[UInt8, MutAnyOrigin],
        canonical_scales: UnsafePointer[UInt8, MutAnyOrigin],
        x: UnsafePointer[Float16, MutAnyOrigin],
        tid: Int,
        row_tile: Int,
        bs: UnsafePointer[
            UInt8, MutUntrackedOrigin, address_space=AddressSpace.SHARED
        ],
        ss: UnsafePointer[
            UInt8, MutUntrackedOrigin, address_space=AddressSpace.SHARED
        ],
        xs: UnsafePointer[
            Float16, MutUntrackedOrigin, address_space=AddressSpace.SHARED
        ],
    ):
        slot = stage % PIPE_STAGES
        x_row = tid // 8
        x_col = (tid % 8) * 16
        x_source = (
            x + x_row * n_cols + stage * 128 + x_col
        ).address_space_cast[AddressSpace.GLOBAL]()
        x_target = xs + slot * a_stage + x_row * lda + x_col
        async_copy[16](x_source, x_target)
        async_copy[16](x_source + 8, x_target + 8)
        comptime for copy_index in range(2):
            chunk = tid + copy_index * 128
            local_k_tile = chunk // 32
            global_tile = (
                (stage * 8 + local_k_tile) * n_tiles64 + row_tile
            )
            code_source = (
                resident + global_tile * 512 + (chunk % 32) * 16
            ).address_space_cast[AddressSpace.GLOBAL]()
            async_copy[16](
                code_source,
                bs + slot * b_stage + chunk * 16,
            )
        if tid < 32:
            local_group = tid // 4
            scale_part = tid % 4
            scale_source = (
                canonical_scales
                + (stage * 8 + local_group) * n_rows
                + row_tile * 64
                + scale_part * 16
            ).address_space_cast[AddressSpace.GLOBAL]()
            async_copy[16](
                scale_source,
                ss
                + slot * s_stage
                + local_group * 64
                + scale_part * 16,
            )
        async_copy_commit_group()

    comptime for preload in range(PIPE_STAGES):
        issue_stage(
            preload,
            resident,
            canonical_scales,
            x,
            tid,
            row_tile,
            bs,
            ss,
            xs,
        )
    async_copy_wait_group(3)
    barrier()

    var accumulators = InlineArray[SIMD[DType.float32, 4], 8](
        fill=SIMD[DType.float32, 4](0.0)
    )
    var stage = 0
    while stage < stages:
        slot = stage % PIPE_STAGES
        comptime for local_k16 in range(2):
            group_in_stage = warp_k * 2 + local_k16
            var scale_words = SIMD[DType.uint32, 2](0)
            if tid4 == 0:
                scale_words = (
                    ss
                    + slot * s_stage
                    + group_in_stage * 64
                    + (lane_row // 2) * 16
                    + (lane_row % 2) * 8
                ).bitcast[UInt32]().load[width=2, alignment=8]()
            quantized = (
                bs
                + slot * b_stage
                + group_in_stage * 512
                + lane * 16
            ).bitcast[UInt32]().load[width=4, alignment=16]()
            a = ld_matrix[8](
                a_base
                + slot * a_stage
                + group_in_stage * 16
            )
            scale_source_lane = UInt32(lane_row * 4)
            decoded_scales0 = _decode_marlin_scales(
                warp.shuffle_idx(scale_words[0], scale_source_lane)
            )
            decoded_scales1 = _decode_marlin_scales(
                warp.shuffle_idx(scale_words[1], scale_source_lane)
            )
            comptime for n16 in range(4):
                comptime scale_index = (n16 % 2) * 2
                comptime if n16 < 2:
                    decoded_scales = decoded_scales0
                else:
                    decoded_scales = decoded_scales1
                lower = _decode_marlin_fragment(
                    quantized[n16],
                    False,
                    decoded_scales[scale_index],
                )
                upper = _decode_marlin_fragment(
                    quantized[n16],
                    True,
                    decoded_scales[scale_index + 1],
                )
                mma(
                    accumulators[n16 * 2],
                    a,
                    lower,
                    accumulators[n16 * 2],
                )
                mma(
                    accumulators[n16 * 2 + 1],
                    a,
                    upper,
                    accumulators[n16 * 2 + 1],
                )
        barrier()
        if stage + PIPE_STAGES < stages:
            issue_stage(
                stage + PIPE_STAGES,
                resident,
                canonical_scales,
                x,
                tid,
                row_tile,
                bs,
                ss,
                xs,
            )
        remaining = stages - stage - 2
        if remaining >= 3:
            async_copy_wait_group(3)
        elif remaining == 2:
            async_copy_wait_group(2)
        elif remaining == 1:
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()
        stage += 1

    scratch = shared.bitcast[Float32]()
    group = lane >> 2
    comptime for n8 in range(8):
        scratch_base = (warp_k * 8 + n8) * 128
        scratch_index = group * 8 + tid4 * 2
        value = accumulators[n8]
        scratch[scratch_base + scratch_index] = value[0]
        scratch[scratch_base + scratch_index + 1] = value[1]
        scratch[scratch_base + scratch_index + 64] = value[2]
        scratch[scratch_base + scratch_index + 65] = value[3]
    barrier()
    if warp_k == 0:
        var reduced = InlineArray[SIMD[DType.float32, 4], 8](
            fill=SIMD[DType.float32, 4](0.0)
        )
        comptime for n8 in range(8):
            scratch_index = group * 8 + tid4 * 2
            comptime for k_part in range(4):
                scratch_base = (k_part * 8 + n8) * 128
                reduced[n8][0] += scratch[
                    scratch_base + scratch_index
                ]
                reduced[n8][1] += scratch[
                    scratch_base + scratch_index + 1
                ]
                reduced[n8][2] += scratch[
                    scratch_base + scratch_index + 64
                ]
                reduced[n8][3] += scratch[
                    scratch_base + scratch_index + 65
                ]
            reduced[n8] *= inv_global_scale * 128.0
            output_row = row0 + n8 * 8 + tid4 * 2
            if group < valid_tokens:
                y[group * n_rows + output_row] = Float16(
                    reduced[n8][0]
                )
                y[group * n_rows + output_row + 1] = Float16(
                    reduced[n8][1]
                )
            if group + 8 < valid_tokens:
                y[(group + 8) * n_rows + output_row] = Float16(
                    reduced[n8][2]
                )
                y[(group + 8) * n_rows + output_row + 1] = Float16(
                    reduced[n8][3]
                )


def gemm_nvfp4_ct_marlin_bm64_bn256_bk64[
    n_cols: Int,
    n_rows: Int,
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_tokens: Int,
    inv_global_scale: Float32,
):
    """Przetwarza pełne kafle i rozdziela je persistent między aktywne CTA."""
    comptime assert n_cols % BK == 0
    comptime assert n_rows % BN == 0
    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    warp_id = tid // WARP_SIZE
    lane_row = lane // 4
    sub = lane // 8
    lane8 = lane % 8
    warp_n = (warp_id % 4) * 64
    warp_k = warp_id // 4
    group = lane >> 2
    tid4 = lane & 3

    shared = external_memory[
        UInt8, address_space=AddressSpace.SHARED, alignment=16
    ]()
    xs = shared.bitcast[Float16]()
    bs = shared + PIPE_STAGES * A_STAGE * 2
    ss = bs + PIPE_STAGES * B_STAGE
    a_base = (
        xs
        + ((sub % 2) * 8 + lane8) * LDA
        + (sub // 2) * 8
    )

    comptime n_tiles = n_rows // BN
    comptime n_tiles64 = n_rows // 64
    comptime k_stages = n_cols // BK
    comptime packed_bytes = n_rows * n_cols // 2
    canonical_codes = resident
    canonical_scales = resident + packed_bytes
    total_tiles = (n_tokens // BM) * n_tiles
    var tile = Int(block_idx.x)
    while tile < total_tiles:
        token0 = (tile // n_tiles) * BM
        row0 = (tile % n_tiles) * BN
        x_row = tid // 4
        x_col8 = (tid % 4) * 8
        x_source = (
            x + (token0 + x_row) * n_cols + x_col8
        ).address_space_cast[AddressSpace.GLOBAL]()
        x_target = xs + x_row * LDA + x_col8

        def issue_stage(
            stage: Int,
            canonical_codes: UnsafePointer[UInt8, MutAnyOrigin],
            canonical_scales: UnsafePointer[UInt8, MutAnyOrigin],
            row0: Int,
            tid: Int,
            x_source: UnsafePointer[
                Float16, MutAnyOrigin, address_space=AddressSpace.GLOBAL
            ],
            x_target: UnsafePointer[
                Float16, MutUntrackedOrigin, address_space=AddressSpace.SHARED
            ],
            bs: UnsafePointer[
                UInt8, MutUntrackedOrigin, address_space=AddressSpace.SHARED
            ],
            ss: UnsafePointer[
                UInt8, MutUntrackedOrigin, address_space=AddressSpace.SHARED
            ],
        ):
            slot = stage % PIPE_STAGES
            async_copy[16](
                x_source + stage * BK,
                x_target + slot * A_STAGE,
            )
            async_copy[16](
                x_source + stage * BK + 32,
                x_target + slot * A_STAGE + 32,
            )
            if tid < 64:
                scale_copy = tid
                local_group = scale_copy // 16
                local_n_tile = (scale_copy % 16) // 4
                scale_part = scale_copy % 4
                scale_source = (
                    canonical_scales
                    + (stage * 4 + local_group) * n_rows
                    + (row0 // 64 + local_n_tile) * 64
                    + scale_part * 16
                ).address_space_cast[AddressSpace.GLOBAL]()
                scale_target = (
                    ss
                    + slot * S_STAGE
                    + (local_group * 4 + local_n_tile) * 64
                    + scale_part * 16
                )
                async_copy[16](scale_source, scale_target)
            comptime for copy_index in range(2):
                chunk = tid + copy_index * 256
                local_tile = chunk // 32
                local_k_tile = local_tile // 4
                local_n_tile = local_tile % 4
                global_tile = (
                    (stage * 4 + local_k_tile) * n_tiles64
                    + row0 // 64
                    + local_n_tile
                )
                code_source = (
                    canonical_codes
                    + global_tile * 512
                    + (chunk % 32) * 16
                ).address_space_cast[AddressSpace.GLOBAL]()
                code_target = (
                    bs + slot * B_STAGE + chunk * 16
                )
                async_copy[16](code_source, code_target)
            async_copy_commit_group()

        comptime for preload in range(PIPE_STAGES):
            issue_stage(
                preload,
                canonical_codes,
                canonical_scales,
                row0,
                tid,
                x_source,
                x_target,
                bs,
                ss,
            )
        async_copy_wait_group(3)
        barrier()

        var accumulators = InlineArray[SIMD[DType.float32, 4], 32](
            fill=SIMD[DType.float32, 4](0.0)
        )
        var stage = 0
        while stage < k_stages:
            slot = stage % PIPE_STAGES
            comptime for local_k16 in range(2):
                group_in_stage = warp_k * 2 + local_k16
                var a_fragments = InlineArray[
                    SIMD[DType.float16, 8], 4
                ](
                    fill=SIMD[DType.float16, 8](0.0)
                )
                canonical_tile = group_in_stage * 4 + warp_n // 64
                var scale_words = SIMD[DType.uint32, 2](0)
                comptime if n_cols >= 4096:
                    if tid4 == 0:
                        scale_words = (
                            ss
                            + slot * S_STAGE
                            + canonical_tile * 64
                            + (lane_row // 2) * 16
                            + (lane_row % 2) * 8
                        ).bitcast[UInt32]().load[width=2, alignment=8]()
                quantized = (
                    bs
                    + slot * B_STAGE
                    + canonical_tile * 512
                    + lane * 16
                ).bitcast[UInt32]().load[width=4, alignment=16]()
                scale_chunk = canonical_tile * 64
                comptime for m16 in range(4):
                    a_fragments[m16] = ld_matrix[8](
                        a_base
                        + slot * A_STAGE
                        + group_in_stage * 16
                        + m16 * 16 * LDA
                    )
                comptime if n_cols >= 4096:
                    scale_source_lane = UInt32(lane_row * 4)
                    scale_word0 = warp.shuffle_idx(
                        scale_words[0], scale_source_lane
                    )
                    scale_word1 = warp.shuffle_idx(
                        scale_words[1], scale_source_lane
                    )
                    decoded_scales0 = _decode_marlin_scales(scale_word0)
                    decoded_scales1 = _decode_marlin_scales(scale_word1)
                    comptime for n16 in range(4):
                        comptime scale_index = (n16 % 2) * 2
                        comptime if n16 < 2:
                            decoded_scales = decoded_scales0
                        else:
                            decoded_scales = decoded_scales1
                        lower = _decode_marlin_fragment(
                            quantized[n16],
                            False,
                            decoded_scales[scale_index],
                        )
                        upper = _decode_marlin_fragment(
                            quantized[n16],
                            True,
                            decoded_scales[scale_index + 1],
                        )
                        comptime for m16 in range(4):
                            mma(
                                accumulators[m16 * 8 + n16 * 2],
                                a_fragments[m16],
                                lower,
                                accumulators[m16 * 8 + n16 * 2],
                            )
                            mma(
                                accumulators[m16 * 8 + n16 * 2 + 1],
                                a_fragments[m16],
                                upper,
                                accumulators[m16 * 8 + n16 * 2 + 1],
                            )
                else:
                    comptime for n16 in range(4):
                        lower_row = warp_n + n16 * 16 + lane_row
                        upper_row = lower_row + 8
                        lower = _decode_marlin_fragment(
                            quantized[n16],
                            False,
                            _decode_marlin_scale(
                                ss[
                                    slot * S_STAGE
                                    + scale_chunk
                                    + _canonical_scale_chunk_offset(
                                        lower_row % 64
                                    )
                                ]
                            ),
                        )
                        upper = _decode_marlin_fragment(
                            quantized[n16],
                            True,
                            _decode_marlin_scale(
                                ss[
                                    slot * S_STAGE
                                    + scale_chunk
                                    + _canonical_scale_chunk_offset(
                                        upper_row % 64
                                    )
                                ]
                            ),
                        )
                        comptime for m16 in range(4):
                            mma(
                                accumulators[m16 * 8 + n16 * 2],
                                a_fragments[m16],
                                lower,
                                accumulators[m16 * 8 + n16 * 2],
                            )
                            mma(
                                accumulators[m16 * 8 + n16 * 2 + 1],
                                a_fragments[m16],
                                upper,
                                accumulators[m16 * 8 + n16 * 2 + 1],
                            )

            barrier()
            if stage + PIPE_STAGES < k_stages:
                issue_stage(
                    stage + PIPE_STAGES,
                    canonical_codes,
                    canonical_scales,
                    row0,
                    tid,
                    x_source,
                    x_target,
                    bs,
                    ss,
                )
            remaining = k_stages - stage - 2
            if remaining >= 3:
                async_copy_wait_group(3)
            elif remaining == 2:
                async_copy_wait_group(2)
            elif remaining == 1:
                async_copy_wait_group(1)
            else:
                async_copy_wait_group(0)
            barrier()
            stage += 1

        output_scale = inv_global_scale * 128.0
        scratch = bs.bitcast[Float32]()
        comptime for m_group in range(2):
            comptime for n_group in range(2):
                comptime for local_m in range(2):
                    comptime for local_n in range(4):
                        comptime accumulator_index = (
                            (m_group * 2 + local_m) * 8
                            + n_group * 4
                            + local_n
                        )
                        comptime scratch_slot = local_m * 4 + local_n
                        scratch_base = (
                            (warp_n // 64 * 2 + warp_k) * 8
                            + scratch_slot
                        ) * 128
                        scratch_index = group * 8 + tid4 * 2
                        value = accumulators[accumulator_index]
                        scratch[scratch_base + scratch_index] = value[0]
                        scratch[scratch_base + scratch_index + 1] = value[1]
                        scratch[scratch_base + scratch_index + 64] = value[2]
                        scratch[scratch_base + scratch_index + 65] = value[3]
                barrier()
                if warp_k == 0:
                    var reduced = InlineArray[
                        SIMD[DType.float32, 4], 8
                    ](fill=SIMD[DType.float32, 4](0.0))
                    comptime for local_m in range(2):
                        comptime for local_n in range(4):
                            comptime scratch_slot = local_m * 4 + local_n
                            scratch_index = group * 8 + tid4 * 2
                            comptime for k_part in range(2):
                                scratch_base = (
                                    (warp_n // 64 * 2 + k_part) * 8
                                    + scratch_slot
                                ) * 128
                                reduced[scratch_slot][0] += scratch[
                                    scratch_base + scratch_index
                                ]
                                reduced[scratch_slot][1] += scratch[
                                    scratch_base + scratch_index + 1
                                ]
                                reduced[scratch_slot][2] += scratch[
                                    scratch_base + scratch_index + 64
                                ]
                                reduced[scratch_slot][3] += scratch[
                                    scratch_base + scratch_index + 65
                                ]
                            reduced[scratch_slot] *= output_scale
                    _store_tile(
                        y,
                        reduced,
                        token0,
                        row0,
                        m_group * 32,
                        warp_n + n_group * 32,
                        group,
                        tid4,
                        n_rows,
                        n_tokens,
                    )
                barrier()
        barrier()
        tile += Int(grid_dim.x)
