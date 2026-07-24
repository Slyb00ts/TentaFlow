# =============================================================================
# Plik: nvfp4_ct_direct.mojo
# Opis: Bezpośredni W4A16 BM16 i BM32 dla naturalnego układu S0 N64/K128.
# Przykład: builder kompiluje warianty M4..M16 (BM16) oraz M24/M32 (BM32).
# =============================================================================

from std.gpu import WARP_SIZE, block_idx, thread_idx
from std.gpu.compute.mma import ld_matrix, mma
from std.gpu.memory import (
    AddressSpace,
    async_copy,
    async_copy_commit_group,
    async_copy_wait_group,
)
from std.gpu.sync import barrier
from std.memory import bitcast, stack_allocation

comptime BM = 16
comptime BN = 64
comptime BK = 128
comptime LDK = 136
comptime XTILE = BM * LDK
comptime PIPE_STAGES = 4
comptime RESIDENT_BK = 128
comptime RESIDENT_BN = 64
comptime RESIDENT_SCALE_BYTES = RESIDENT_BN * (RESIDENT_BK // 16)
comptime RESIDENT_CODE_BYTES = RESIDENT_BN * (RESIDENT_BK // 2)
comptime RESIDENT_STAGE_BYTES = RESIDENT_SCALE_BYTES + RESIDENT_CODE_BYTES
comptime GATE_STAGES = 3
comptime OUTPUT_TOKEN_MAJOR = 0
comptime OUTPUT_QKV_SEGMENTS = 1
comptime OUTPUT_GATE_UP_SEGMENTS = 2


def _output_index[
    output_layout: Int,
    n_rows: Int,
    stored_tokens: Int,
](token: Int, row: Int) -> Int:
    comptime if output_layout == OUTPUT_QKV_SEGMENTS:
        comptime assert n_rows == 6144
        comptime q_rows = 4096
        comptime kv_rows = 1024
        if row < q_rows:
            return token * q_rows + row
        if row < q_rows + kv_rows:
            return stored_tokens * q_rows + token * kv_rows + row - q_rows
        return (
            stored_tokens * (q_rows + kv_rows)
            + token * kv_rows
            + row
            - q_rows
            - kv_rows
        )
    else:
        comptime if output_layout == OUTPUT_GATE_UP_SEGMENTS:
            comptime assert n_rows % 2 == 0
            comptime segment_rows = n_rows // 2
            if row < segment_rows:
                return token * segment_rows + row
            return (
                stored_tokens * segment_rows
                + token * segment_rows
                + row
                - segment_rows
            )
        else:
            return token * n_rows + row


def nvfp4_ct_split_pipeline_supported[
    total_stages: Int,
    parts: Int,
    pipeline_stages: Int,
]() -> Bool:
    comptime if total_stages <= 0 or parts <= 0 or pipeline_stages <= 0:
        return False
    comptime span = (total_stages + parts - 1) // parts
    comptime for part in range(parts):
        comptime start = part * span
        comptime finish = min(start + span, total_stages)
        comptime if start >= finish or finish - start < pipeline_stages:
            return False
    return True


def _decode_fragment(
    raw0: UInt8,
    raw1: UInt8,
    encoded_scale: UInt8,
) -> SIMD[DType.float16, 4]:
    packed = UInt32(raw0) << 8 | UInt32(raw1) << 24
    odd = (packed & 0x80008000) | ((packed & 0x70007000) >> 3)
    shifted = packed << 4
    even = (shifted & 0x80008000) | ((shifted & 0x70007000) >> 3)
    bits = SIMD[DType.uint16, 4](
        UInt16(even),
        UInt16(odd),
        UInt16(even >> 16),
        UInt16(odd >> 16),
    )
    values = bitcast[DType.float16, 4](bits)
    scale_bits = UInt16(encoded_scale) << 7
    scale = (
        bitcast[DType.float16, 1](
            SIMD[DType.uint16, 1](scale_bits)
        )[0]
    )
    return values * scale


def gemm_nvfp4_ct_direct_down_bm16_bn64_bk128[
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    valid_tokens: Int,
    stored_tokens: Int,
    outer_split_k: Int,
    output_layout: Int,
](
    partial: UnsafePointer[Float32, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy części K bez materializacji zdekodowanych wag w shared."""
    comptime assert n_tokens == BM, "kernel wymaga fizycznego BM16"
    comptime assert 0 < valid_tokens <= BM, "valid_tokens musi należeć do 1..16"
    comptime assert stored_tokens == BM, "scratch musi przechować fizyczne BM16"
    comptime assert outer_split_k > 0, "split-K musi być dodatni"
    comptime assert nvfp4_ct_split_pipeline_supported[
        n_cols // BK, outer_split_k, PIPE_STAGES
    ](), "każda część split-K musi mieć co najmniej cztery etapy"
    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    warp_id = tid // WARP_SIZE
    block = Int(block_idx.x)
    split_id = block % outer_split_k
    row0 = (block // outer_split_k) * BN
    row_tile = row0 // BN

    xs = stack_allocation[
        PIPE_STAGES * XTILE, Float16, address_space=AddressSpace.SHARED
    ]()
    bs = stack_allocation[
        PIPE_STAGES * RESIDENT_CODE_BYTES,
        UInt8,
        address_space=AddressSpace.SHARED,
    ]()
    ss = stack_allocation[
        PIPE_STAGES * RESIDENT_SCALE_BYTES,
        UInt8,
        address_space=AddressSpace.SHARED,
    ]()
    x_row = tid // 8
    x_col8 = (tid % 8) * 8
    var source_token = x_row
    if source_token >= valid_tokens:
        source_token = 0
    x_source = (x + source_token * n_cols + x_col8).address_space_cast[
        AddressSpace.GLOBAL
    ]()
    x_target = xs + x_row * LDK + x_col8
    x_valid_bytes = Int32(16) if x_row < valid_tokens else Int32(0)

    lane4 = lane & 3
    lane_row = lane // 4
    sub = lane // 8
    lane8 = lane % 8
    a_base = (
        xs
        + ((sub % 2) * 8 + lane8) * LDK
        + (sub // 2) * 8
    )
    var accumulators = InlineArray[SIMD[DType.float32, 4], 8](
        fill=SIMD[DType.float32, 4](0.0)
    )

    total_stages = n_cols // BK
    stages_per_split = (
        total_stages + outer_split_k - 1
    ) // outer_split_k
    stage_start = split_id * stages_per_split
    stage_finish = min(stage_start + stages_per_split, total_stages)

    def issue_stage(
        stage: Int,
        resident: UnsafePointer[UInt8, MutAnyOrigin],
        row_tile: Int,
        total_stages: Int,
        tid: Int,
        valid_bytes: Int32,
        bs: UnsafePointer[
            UInt8, MutUntrackedOrigin, address_space=AddressSpace.SHARED
        ],
        ss: UnsafePointer[
            UInt8, MutUntrackedOrigin, address_space=AddressSpace.SHARED
        ],
        x_source: UnsafePointer[
            Float16, MutAnyOrigin, address_space=AddressSpace.GLOBAL
        ],
        x_target: UnsafePointer[
            Float16, MutUntrackedOrigin, address_space=AddressSpace.SHARED
        ],
    ):
        slot = stage % PIPE_STAGES
        offset = stage * BK
        if valid_bytes == 16:
            async_copy[16](
                x_source + offset,
                x_target + slot * XTILE,
            )
            async_copy[16](
                x_source + offset + 64,
                x_target + slot * XTILE + 64,
            )
        else:
            async_copy[16, fill=Float16(0)](
                x_source + offset,
                x_target + slot * XTILE,
                Int32(0),
            )
            async_copy[16, fill=Float16(0)](
                x_source + offset + 64,
                x_target + slot * XTILE + 64,
                Int32(0),
            )
        resident_base = (
            resident
            + (row_tile * total_stages + stage) * RESIDENT_STAGE_BYTES
        ).address_space_cast[AddressSpace.GLOBAL]()
        code_source = resident_base + RESIDENT_SCALE_BYTES
        code_target = bs + slot * RESIDENT_CODE_BYTES + tid * 16
        async_copy[16](code_source + tid * 16, code_target)
        async_copy[16](
            code_source + tid * 16 + 2048,
            code_target + 2048,
        )
        if tid < 32:
            async_copy[16](
                resident_base + tid * 16,
                ss + slot * RESIDENT_SCALE_BYTES + tid * 16,
            )
        async_copy_commit_group()

    comptime for preload in range(PIPE_STAGES):
        issue_stage(
            min(stage_start + preload, stage_finish - 1),
            resident,
            row_tile,
            total_stages,
            tid,
            x_valid_bytes,
            bs,
            ss,
            x_source,
            x_target,
        )
    async_copy_wait_group(2)
    barrier()

    var stage = stage_start
    while stage < stage_finish:
        slot = stage % PIPE_STAGES
        comptime for local_k16 in range(2):
            group_in_stage = warp_id * 2 + local_k16
            comptime for n8 in range(8):
                shared_row = n8 * 8 + lane_row
                encoded_scale = ss[
                    slot * RESIDENT_SCALE_BYTES
                    + shared_row * 8
                    + group_in_stage
                ]
                code_base = (
                    bs
                    + slot * RESIDENT_CODE_BYTES
                    + shared_row * 64
                    + group_in_stage * 8
                )
                b = _decode_fragment(
                    code_base[lane4],
                    code_base[lane4 + 4],
                    encoded_scale,
                )
                a = ld_matrix[8](
                    a_base
                    + slot * XTILE
                    + warp_id * 32
                    + local_k16 * 16
                )
                mma(accumulators[n8], a, b, accumulators[n8])

        barrier()
        if stage + PIPE_STAGES < stage_finish:
            issue_stage(
                stage + PIPE_STAGES,
                resident,
                row_tile,
                total_stages,
                tid,
                x_valid_bytes,
                bs,
                ss,
                x_source,
                x_target,
            )
        remaining = stage_finish - stage - 2
        if remaining >= 2:
            async_copy_wait_group(2)
        elif remaining == 1:
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()
        stage += 1

    async_copy_wait_group(0)
    barrier()
    group = lane >> 2
    part_base = split_id * stored_tokens * n_rows
    output_scale = inv_global_scale * 128.0
    scratch = bs.bitcast[Float32]()
    comptime for tile_group in range(2):
        comptime for local_n8 in range(4):
            comptime n8 = tile_group * 4 + local_n8
            scratch_base = (warp_id * 4 + local_n8) * 128
            scratch_index = group * 8 + lane4 * 2
            scratch[scratch_base + scratch_index] = accumulators[n8][0]
            scratch[scratch_base + scratch_index + 1] = accumulators[n8][1]
            scratch[scratch_base + scratch_index + 64] = accumulators[n8][2]
            scratch[scratch_base + scratch_index + 65] = accumulators[n8][3]
        barrier()
        if warp_id == 0:
            comptime for local_n8 in range(4):
                comptime n8 = tile_group * 4 + local_n8
                scratch_index = group * 8 + lane4 * 2
                var reduced = SIMD[DType.float32, 4](0.0)
                comptime for warp_k in range(4):
                    scratch_base = (warp_k * 4 + local_n8) * 128
                    reduced[0] += scratch[scratch_base + scratch_index]
                    reduced[1] += scratch[scratch_base + scratch_index + 1]
                    reduced[2] += scratch[scratch_base + scratch_index + 64]
                    reduced[3] += scratch[scratch_base + scratch_index + 65]
                output_row = row0 + n8 * 8 + lane4 * 2
                if group < stored_tokens and output_row < n_rows:
                    output_index = _output_index[
                        output_layout, n_rows, stored_tokens
                    ](group, output_row)
                    partial[
                        part_base + output_index
                    ] = reduced[0] * output_scale
                    if output_row + 1 < n_rows:
                        partial[
                            part_base + output_index + 1
                        ] = reduced[1] * output_scale
                if group + 8 < stored_tokens and output_row < n_rows:
                    output_index = _output_index[
                        output_layout, n_rows, stored_tokens
                    ](group + 8, output_row)
                    partial[
                        part_base + output_index
                    ] = reduced[2] * output_scale
                    if output_row + 1 < n_rows:
                        partial[
                            part_base + output_index + 1
                        ] = reduced[3] * output_scale
        barrier()


def reduce_nvfp4_direct_down(
    y: UnsafePointer[Float16, MutAnyOrigin],
    partial: UnsafePointer[Float32, MutAnyOrigin],
    n_rows: Int,
    n_tokens: Int,
    parts: Int,
):
    """Redukuje części warp-K i outer split-K."""
    index = Int(block_idx.x) * 256 + Int(thread_idx.x)
    elements = n_rows * n_tokens
    if index >= elements:
        return
    var total = Float32(0)
    for part in range(parts):
        total += partial[part * elements + index]
    y[index] = Float16(total)


@always_inline
def _gemm_nvfp4_ct_direct_gate_tile[
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    valid_tokens: Int,
    stored_tokens: Int,
    outer_split_k: Int,
    output_layout: Int,
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
    xs: UnsafePointer[
        Float16, MutUntrackedOrigin, address_space=AddressSpace.SHARED
    ],
    bs: UnsafePointer[
        UInt8, MutUntrackedOrigin, address_space=AddressSpace.SHARED
    ],
    ss: UnsafePointer[
        UInt8, MutUntrackedOrigin, address_space=AddressSpace.SHARED
    ],
    row0: Int,
    split_id: Int,
    stage_start: Int,
    stage_finish: Int,
):
    """Liczy gate BN128/BK128 z trzema etapami skompresowanych wag."""
    comptime gate_stages = GATE_STAGES
    comptime gate_bn = 128
    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    warp_id = tid // WARP_SIZE
    warp_col = warp_id % 2
    warp_k = warp_id // 2
    row_tile64 = row0 // RESIDENT_BN

    x_row = tid // 16
    x_col8 = (tid % 16) * 8
    var source_token = x_row
    if source_token >= valid_tokens:
        source_token = 0
    x_source = (x + source_token * n_cols + x_col8).address_space_cast[
        AddressSpace.GLOBAL
    ]()
    x_target = xs + x_row * LDK + x_col8
    x_valid_bytes = Int32(16) if x_row < valid_tokens else Int32(0)

    lane4 = lane & 3
    lane_row = lane // 4
    sub = lane // 8
    lane8 = lane % 8
    a_base = (
        xs
        + ((sub % 2) * 8 + lane8) * LDK
        + (sub // 2) * 8
    )
    var accumulators = InlineArray[SIMD[DType.float32, 4], 8](
        fill=SIMD[DType.float32, 4](0.0)
    )
    total_stages = n_cols // BK

    def issue_gate_stage(
        stage: Int,
        resident: UnsafePointer[UInt8, MutAnyOrigin],
        row_tile64: Int,
        total_stages: Int,
        tid: Int,
        valid_bytes: Int32,
        bs: UnsafePointer[
            UInt8, MutUntrackedOrigin, address_space=AddressSpace.SHARED
        ],
        ss: UnsafePointer[
            UInt8, MutUntrackedOrigin, address_space=AddressSpace.SHARED
        ],
        x_source: UnsafePointer[
            Float16, MutAnyOrigin, address_space=AddressSpace.GLOBAL
        ],
        x_target: UnsafePointer[
            Float16, MutUntrackedOrigin, address_space=AddressSpace.SHARED
        ],
    ):
        slot = stage % gate_stages
        if valid_bytes == 16:
            async_copy[16](
                x_source + stage * BK,
                x_target + slot * XTILE,
            )
        else:
            async_copy[16, fill=Float16(0)](
                x_source + stage * BK,
                x_target + slot * XTILE,
                Int32(0),
            )
        resident_half = tid // 128
        resident_tid = tid % 128
        resident_base = (
            resident
            + (
                (row_tile64 + resident_half) * total_stages
                + stage
            ) * RESIDENT_STAGE_BYTES
        ).address_space_cast[AddressSpace.GLOBAL]()
        code_source = resident_base + RESIDENT_SCALE_BYTES
        code_target = (
            bs
            + slot * 2 * RESIDENT_CODE_BYTES
            + resident_half * RESIDENT_CODE_BYTES
            + resident_tid * 16
        )
        async_copy[16](code_source + resident_tid * 16, code_target)
        async_copy[16](
            code_source + resident_tid * 16 + 2048,
            code_target + 2048,
        )
        if tid < 64:
            scale_half = tid // 32
            scale_tid = tid % 32
            scale_source = (
                resident
                + (
                    (row_tile64 + scale_half) * total_stages
                    + stage
                ) * RESIDENT_STAGE_BYTES
            ).address_space_cast[AddressSpace.GLOBAL]()
            async_copy[16](
                scale_source + scale_tid * 16,
                ss
                + slot * 2 * RESIDENT_SCALE_BYTES
                + scale_half * RESIDENT_SCALE_BYTES
                + scale_tid * 16,
            )
        async_copy_commit_group()

    comptime for preload in range(gate_stages):
        issue_gate_stage(
            min(stage_start + preload, stage_finish - 1),
            resident,
            row_tile64,
            total_stages,
            tid,
            x_valid_bytes,
            bs,
            ss,
            x_source,
            x_target,
        )
    async_copy_wait_group(1)
    barrier()

    var stage = stage_start
    while stage < stage_finish:
        slot = stage % gate_stages
        comptime for local_k16 in range(2):
            group_in_stage = warp_k * 2 + local_k16
            comptime for n8 in range(8):
                shared_row = n8 * 8 + lane_row
                encoded_scale = ss[
                    slot * 2 * RESIDENT_SCALE_BYTES
                    + warp_col * RESIDENT_SCALE_BYTES
                    + shared_row * 8
                    + group_in_stage
                ]
                code_base = (
                    bs
                    + slot * 2 * RESIDENT_CODE_BYTES
                    + warp_col * RESIDENT_CODE_BYTES
                    + shared_row * 64
                    + group_in_stage * 8
                )
                b = _decode_fragment(
                    code_base[lane4],
                    code_base[lane4 + 4],
                    encoded_scale,
                )
                a = ld_matrix[8](
                    a_base
                    + slot * XTILE
                    + warp_k * 32
                    + local_k16 * 16
                )
                mma(accumulators[n8], a, b, accumulators[n8])

        barrier()
        if stage + gate_stages < stage_finish:
            issue_gate_stage(
                stage + gate_stages,
                resident,
                row_tile64,
                total_stages,
                tid,
                x_valid_bytes,
                bs,
                ss,
                x_source,
                x_target,
            )
        remaining = stage_finish - stage - 2
        if remaining >= 1:
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()
        stage += 1

    async_copy_wait_group(0)
    barrier()
    group = lane >> 2
    output_scale = inv_global_scale * 128.0
    scratch = bs.bitcast[Float32]()
    comptime for tile_group in range(2):
        comptime for local_n8 in range(4):
            comptime n8 = tile_group * 4 + local_n8
            scratch_base = (
                (warp_col * 4 + warp_k) * 4 + local_n8
            ) * 128
            scratch_index = group * 8 + lane4 * 2
            scratch[scratch_base + scratch_index] = accumulators[n8][0]
            scratch[scratch_base + scratch_index + 1] = accumulators[n8][1]
            scratch[scratch_base + scratch_index + 64] = accumulators[n8][2]
            scratch[scratch_base + scratch_index + 65] = accumulators[n8][3]
        barrier()
        if warp_k == 0:
            comptime for local_n8 in range(4):
                comptime n8 = tile_group * 4 + local_n8
                scratch_index = group * 8 + lane4 * 2
                var reduced = SIMD[DType.float32, 4](0.0)
                comptime for k_part in range(4):
                    scratch_base = (
                        (warp_col * 4 + k_part) * 4 + local_n8
                    ) * 128
                    reduced[0] += scratch[scratch_base + scratch_index]
                    reduced[1] += scratch[scratch_base + scratch_index + 1]
                    reduced[2] += scratch[scratch_base + scratch_index + 64]
                    reduced[3] += scratch[scratch_base + scratch_index + 65]
                output_row = row0 + warp_col * 64 + n8 * 8 + lane4 * 2
                comptime if outer_split_k == 1:
                    if group < stored_tokens and output_row < n_rows:
                        output_index = _output_index[
                            output_layout, n_rows, stored_tokens
                        ](group, output_row)
                        y[output_index] = Float16(
                            reduced[0] * output_scale
                        )
                        if output_row + 1 < n_rows:
                            y[output_index + 1] = Float16(
                                reduced[1] * output_scale
                            )
                    if group + 8 < stored_tokens and output_row < n_rows:
                        output_index = _output_index[
                            output_layout, n_rows, stored_tokens
                        ](group + 8, output_row)
                        y[output_index] = Float16(
                            reduced[2] * output_scale
                        )
                        if output_row + 1 < n_rows:
                            y[output_index + 1] = Float16(
                                reduced[3] * output_scale
                            )
                else:
                    workspace = y.bitcast[Float32]()
                    part_base = split_id * stored_tokens * n_rows
                    if group < stored_tokens and output_row < n_rows:
                        output_index = _output_index[
                            output_layout, n_rows, stored_tokens
                        ](group, output_row)
                        workspace[
                            part_base + output_index
                        ] = reduced[0] * output_scale
                        if output_row + 1 < n_rows:
                            workspace[
                                part_base + output_index + 1
                            ] = reduced[1] * output_scale
                    if group + 8 < stored_tokens and output_row < n_rows:
                        output_index = _output_index[
                            output_layout, n_rows, stored_tokens
                        ](group + 8, output_row)
                        workspace[
                            part_base + output_index
                        ] = reduced[2] * output_scale
                        if output_row + 1 < n_rows:
                            workspace[
                                part_base + output_index + 1
                            ] = reduced[3] * output_scale
        barrier()


def gemm_nvfp4_ct_direct_gate_bm16_bn128_bk128[
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    valid_tokens: Int,
    stored_tokens: Int,
    outer_split_k: Int,
    output_layout: Int,
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy równomierny podział K dla jednego kafla wyjścia."""
    comptime assert n_tokens == BM, "kernel wymaga fizycznego BM16"
    comptime assert 0 < valid_tokens <= BM, "valid_tokens musi należeć do 1..16"
    comptime assert stored_tokens == BM, "scratch musi przechować fizyczne BM16"
    comptime assert outer_split_k > 0, "split-K musi być dodatni"
    comptime assert nvfp4_ct_split_pipeline_supported[
        n_cols // BK, outer_split_k, GATE_STAGES
    ](), "każda część split-K musi mieć co najmniej trzy etapy"
    block = Int(block_idx.x)
    split_id = block % outer_split_k
    row0 = (block // outer_split_k) * 128
    total_stages = n_cols // BK
    stages_per_split = (
        total_stages + outer_split_k - 1
    ) // outer_split_k
    stage_start = split_id * stages_per_split
    stage_finish = min(stage_start + stages_per_split, total_stages)
    xs = stack_allocation[
        GATE_STAGES * XTILE, Float16, address_space=AddressSpace.SHARED
    ]()
    bs = stack_allocation[
        GATE_STAGES * 2 * RESIDENT_CODE_BYTES,
        UInt8,
        address_space=AddressSpace.SHARED,
    ]()
    ss = stack_allocation[
        GATE_STAGES * 2 * RESIDENT_SCALE_BYTES,
        UInt8,
        address_space=AddressSpace.SHARED,
    ]()
    _gemm_nvfp4_ct_direct_gate_tile[
        n_cols, n_rows, n_tokens, valid_tokens, stored_tokens, outer_split_k,
        output_layout,
    ](
        y,
        resident,
        x,
        inv_global_scale,
        xs,
        bs,
        ss,
        row0,
        split_id,
        stage_start,
        stage_finish,
    )


def gemm_nvfp4_ct_highm_bm16_bn128_bk128[
    n_cols: Int,
    n_rows: Int,
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_tokens: Int,
    inv_global_scale: Float32,
):
    """Liczy pełne kafle M16 bez split-K dla prefill będącego wielokrotnością 16."""
    comptime assert n_cols % BK == 0
    comptime assert n_rows % 128 == 0
    comptime n_tiles = n_rows // 128
    block = Int(block_idx.x)
    token_tile = block // n_tiles
    row0 = (block % n_tiles) * 128
    token0 = token_tile * BM
    if token0 >= n_tokens:
        return
    xs = stack_allocation[
        GATE_STAGES * XTILE, Float16, address_space=AddressSpace.SHARED
    ]()
    bs = stack_allocation[
        GATE_STAGES * 2 * RESIDENT_CODE_BYTES,
        UInt8,
        address_space=AddressSpace.SHARED,
    ]()
    ss = stack_allocation[
        GATE_STAGES * 2 * RESIDENT_SCALE_BYTES,
        UInt8,
        address_space=AddressSpace.SHARED,
    ]()
    _gemm_nvfp4_ct_direct_gate_tile[
        n_cols, n_rows, BM, BM, BM, 1, OUTPUT_TOKEN_MAJOR,
    ](
        y + token0 * n_rows,
        resident,
        x + token0 * n_cols,
        inv_global_scale,
        xs,
        bs,
        ss,
        row0,
        0,
        0,
        n_cols // BK,
    )


def gemm_nvfp4_ct_direct_gateup_striped[
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    workspace: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Równoważy 176 kafli N przez 128 CTA i selektywny split-K ogona."""
    block = Int(block_idx.x)
    xs = stack_allocation[
        GATE_STAGES * XTILE, Float16, address_space=AddressSpace.SHARED
    ]()
    bs = stack_allocation[
        GATE_STAGES * 2 * RESIDENT_CODE_BYTES,
        UInt8,
        address_space=AddressSpace.SHARED,
    ]()
    ss = stack_allocation[
        GATE_STAGES * 2 * RESIDENT_SCALE_BYTES,
        UInt8,
        address_space=AddressSpace.SHARED,
    ]()
    _gemm_nvfp4_ct_direct_gate_tile[
        n_cols, n_rows, n_tokens, n_tokens, n_tokens, 1,
        OUTPUT_TOKEN_MAJOR,
    ](
        y,
        resident,
        x,
        inv_global_scale,
        xs,
        bs,
        ss,
        block * 128,
        0,
        0,
        n_cols // BK,
    )

    total_stages = n_cols // BK
    stripe_start = block * 12
    stripe_finish = min(stripe_start + 12, 48 * total_stages)
    tail_tile = stripe_start // total_stages
    stage_start = stripe_start % total_stages
    stage_finish = min(total_stages, stage_start + 12)
    first_boundary = (
        tail_tile * total_stages // 12 + 1
    ) * 12
    var split_id = 0
    if stripe_start != tail_tile * total_stages:
        split_id = 1 + (stripe_start - first_boundary) // 12
    _gemm_nvfp4_ct_direct_gate_tile[
        n_cols, n_rows, n_tokens, n_tokens, n_tokens, 4,
        OUTPUT_TOKEN_MAJOR,
    ](
        workspace,
        resident,
        x,
        inv_global_scale,
        xs,
        bs,
        ss,
        (128 + tail_tile) * 128,
        split_id,
        stage_start,
        stage_finish,
    )
    if stripe_finish > (tail_tile + 1) * total_stages:
        tail_tile += 1
        second_finish = stripe_finish - tail_tile * total_stages
        _gemm_nvfp4_ct_direct_gate_tile[
            n_cols, n_rows, n_tokens, n_tokens, n_tokens, 4,
            OUTPUT_TOKEN_MAJOR,
        ](
            workspace,
            resident,
            x,
            inv_global_scale,
            xs,
            bs,
            ss,
            (128 + tail_tile) * 128,
            0,
            0,
            second_finish,
        )


def reduce_nvfp4_gate_split(
    y: UnsafePointer[Float16, MutAnyOrigin],
    workspace_bytes: UnsafePointer[Float16, MutAnyOrigin],
    n_rows: Int,
    n_tokens: Int,
    parts: Int,
):
    """Redukuje części K zapisane jako FP32 w buforze typowanym FP16."""
    index = Int(block_idx.x) * 256 + Int(thread_idx.x)
    elements = n_rows * n_tokens
    if index >= elements:
        return
    workspace = workspace_bytes.bitcast[Float32]()
    var total = Float32(0.0)
    for part in range(parts):
        total += workspace[part * elements + index]
    y[index] = Float16(total)


def reduce_nvfp4_gateup_striped[
    n_rows: Int,
    n_tokens: Int,
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    workspace_bytes: UnsafePointer[Float16, MutAnyOrigin],
):
    """Redukuje tylko 48 końcowych kafli selektywnego split-K."""
    tail_rows = n_rows - 128 * 128
    index = Int(block_idx.x) * 256 + Int(thread_idx.x)
    if index >= tail_rows * n_tokens:
        return
    token = index // tail_rows
    tail_row = index % tail_rows
    row = 128 * 128 + tail_row
    workspace = workspace_bytes.bitcast[Float32]()
    var total = Float32(0.0)
    for part in range(4):
        total += workspace[
            part * n_tokens * n_rows + token * n_rows + row
        ]
    y[token * n_rows + row] = Float16(total)


def zero_nvfp4_gateup_workspace[
    n_rows: Int,
    n_tokens: Int,
](
    workspace_bytes: UnsafePointer[Float16, MutAnyOrigin],
):
    """Zeruje cztery sloty FP32 używane tylko przez 48 kafli ogona."""
    tail_rows = n_rows - 128 * 128
    index = Int(block_idx.x) * 256 + Int(thread_idx.x)
    elements = tail_rows * n_tokens
    if index >= elements * 4:
        return
    part = index // elements
    local = index % elements
    token = local // tail_rows
    row = 128 * 128 + local % tail_rows
    workspace = workspace_bytes.bitcast[Float32]()
    workspace[part * n_tokens * n_rows + token * n_rows + row] = 0.0


def gemm_nvfp4_ct_bm16_qkv_m16(
    workspace: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy QKV 4096x6144 z trzema częściami K."""
    gemm_nvfp4_ct_direct_gate_bm16_bn128_bk128[
        4096, 6144, 16, 16, 16, 3, OUTPUT_QKV_SEGMENTS
    ](
        workspace, resident, x, inv_global_scale
    )


def gemm_nvfp4_ct_bm16_o_m16(
    workspace: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy projekcję wyjściową 4096x4096 z czterema częściami K."""
    gemm_nvfp4_ct_direct_gate_bm16_bn128_bk128[
        4096, 4096, 16, 16, 16, 4, OUTPUT_TOKEN_MAJOR
    ](
        workspace, resident, x, inv_global_scale
    )


def gemm_nvfp4_ct_bm16_gateup_m16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy gate i up 4096x22528 bez zewnętrznego split-K."""
    gemm_nvfp4_ct_direct_gate_bm16_bn128_bk128[
        4096, 22528, 16, 16, 16, 1, OUTPUT_GATE_UP_SEGMENTS
    ](
        y, resident, x, inv_global_scale
    )


def gemm_nvfp4_ct_bm16_down_m16(
    workspace: UnsafePointer[Float32, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy down 11264x4096 z czterema częściami K."""
    gemm_nvfp4_ct_direct_down_bm16_bn64_bk128[
        11264, 4096, 16, 16, 16, 4, OUTPUT_TOKEN_MAJOR
    ](
        workspace, resident, x, inv_global_scale
    )


def gemm_nvfp4_ct_bm16_qkv_m4(
    workspace: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy QKV dla czterech logicznych tokenów wewnątrz BM16."""
    gemm_nvfp4_ct_direct_gate_bm16_bn128_bk128[
        4096, 6144, 16, 4, 16, 3, OUTPUT_QKV_SEGMENTS
    ](
        workspace, resident, x, inv_global_scale
    )


def gemm_nvfp4_ct_bm16_o_m4(
    workspace: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy projekcję wyjściową dla czterech logicznych tokenów."""
    gemm_nvfp4_ct_direct_gate_bm16_bn128_bk128[
        4096, 4096, 16, 4, 16, 4, OUTPUT_TOKEN_MAJOR
    ](
        workspace, resident, x, inv_global_scale
    )


def gemm_nvfp4_ct_bm16_gateup_m4(
    y: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy gate i up dla czterech logicznych tokenów."""
    gemm_nvfp4_ct_direct_gate_bm16_bn128_bk128[
        4096, 22528, 16, 4, 16, 1, OUTPUT_GATE_UP_SEGMENTS
    ](
        y, resident, x, inv_global_scale
    )


def gemm_nvfp4_ct_bm16_down_m4(
    workspace: UnsafePointer[Float32, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy down dla czterech logicznych tokenów wewnątrz BM16."""
    gemm_nvfp4_ct_direct_down_bm16_bn64_bk128[
        11264, 4096, 16, 4, 16, 4, OUTPUT_TOKEN_MAJOR
    ](
        workspace, resident, x, inv_global_scale
    )


def gemm_nvfp4_ct_bm16_qkv_m8(
    workspace: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy QKV dla ośmiu logicznych tokenów wewnątrz BM16."""
    gemm_nvfp4_ct_direct_gate_bm16_bn128_bk128[
        4096, 6144, 16, 8, 16, 3, OUTPUT_QKV_SEGMENTS
    ](
        workspace, resident, x, inv_global_scale
    )


def gemm_nvfp4_ct_bm16_o_m8(
    workspace: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy projekcję wyjściową dla ośmiu logicznych tokenów."""
    gemm_nvfp4_ct_direct_gate_bm16_bn128_bk128[
        4096, 4096, 16, 8, 16, 4, OUTPUT_TOKEN_MAJOR
    ](
        workspace, resident, x, inv_global_scale
    )


def gemm_nvfp4_ct_bm16_gateup_m8(
    y: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy gate i up dla ośmiu logicznych tokenów."""
    gemm_nvfp4_ct_direct_gate_bm16_bn128_bk128[
        4096, 22528, 16, 8, 16, 1, OUTPUT_GATE_UP_SEGMENTS
    ](
        y, resident, x, inv_global_scale
    )


def gemm_nvfp4_ct_bm16_down_m8(
    workspace: UnsafePointer[Float32, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy down dla ośmiu logicznych tokenów wewnątrz BM16."""
    gemm_nvfp4_ct_direct_down_bm16_bn64_bk128[
        11264, 4096, 16, 8, 16, 4, OUTPUT_TOKEN_MAJOR
    ](
        workspace, resident, x, inv_global_scale
    )


comptime BM32 = 32
comptime XTILE32 = BM32 * LDK
comptime BM32_STAGES = 3


def gemm_nvfp4_ct_direct_bm32_bn64_bk128[
    n_cols: Int,
    n_rows: Int,
    valid_tokens: Int,
    outer_split_k: Int,
    output_layout: Int,
](
    y: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy kafel BN64 dla 32 wierszy tokenów bez materializacji wag."""
    comptime assert 0 < valid_tokens <= BM32, "valid_tokens musi należeć do 1..32"
    comptime assert outer_split_k > 0, "split-K musi być dodatni"
    comptime assert n_cols % BK == 0, "K musi być wielokrotnością 128"
    comptime assert n_rows % BN == 0, "N musi być wielokrotnością 64"
    comptime assert nvfp4_ct_split_pipeline_supported[
        n_cols // BK, outer_split_k, BM32_STAGES
    ](), "każda część split-K musi mieć co najmniej trzy etapy"
    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    warp_id = tid // WARP_SIZE
    block = Int(block_idx.x)
    split_id = block % outer_split_k
    row0 = (block // outer_split_k) * BN
    row_tile = row0 // BN

    xs = stack_allocation[
        BM32_STAGES * XTILE32, Float16, address_space=AddressSpace.SHARED
    ]()
    bs = stack_allocation[
        BM32_STAGES * RESIDENT_CODE_BYTES,
        UInt8,
        address_space=AddressSpace.SHARED,
    ]()
    ss = stack_allocation[
        BM32_STAGES * RESIDENT_SCALE_BYTES,
        UInt8,
        address_space=AddressSpace.SHARED,
    ]()

    x_row = tid // 8
    x_col8 = (tid % 8) * 8
    var source_low = x_row
    if source_low >= valid_tokens:
        source_low = 0
    var source_high = x_row + 16
    if source_high >= valid_tokens:
        source_high = 0
    x_source_low = (x + source_low * n_cols + x_col8).address_space_cast[
        AddressSpace.GLOBAL
    ]()
    x_source_high = (x + source_high * n_cols + x_col8).address_space_cast[
        AddressSpace.GLOBAL
    ]()
    x_target_low = xs + x_row * LDK + x_col8
    x_target_high = xs + (x_row + 16) * LDK + x_col8
    x_valid_low = Int32(16) if x_row < valid_tokens else Int32(0)
    x_valid_high = Int32(16) if x_row + 16 < valid_tokens else Int32(0)

    lane4 = lane & 3
    lane_row = lane // 4
    sub = lane // 8
    lane8 = lane % 8
    a_base = xs + ((sub % 2) * 8 + lane8) * LDK + (sub // 2) * 8
    var accumulators = InlineArray[SIMD[DType.float32, 4], 16](
        fill=SIMD[DType.float32, 4](0.0)
    )

    total_stages = n_cols // BK
    stages_per_split = (total_stages + outer_split_k - 1) // outer_split_k
    stage_start = split_id * stages_per_split
    stage_finish = min(stage_start + stages_per_split, total_stages)

    def issue_stage(
        stage: Int,
        resident: UnsafePointer[UInt8, MutAnyOrigin],
        row_tile: Int,
        total_stages: Int,
        tid: Int,
        valid_low: Int32,
        valid_high: Int32,
        bs: UnsafePointer[
            UInt8, MutUntrackedOrigin, address_space=AddressSpace.SHARED
        ],
        ss: UnsafePointer[
            UInt8, MutUntrackedOrigin, address_space=AddressSpace.SHARED
        ],
        x_source_low: UnsafePointer[
            Float16, MutAnyOrigin, address_space=AddressSpace.GLOBAL
        ],
        x_source_high: UnsafePointer[
            Float16, MutAnyOrigin, address_space=AddressSpace.GLOBAL
        ],
        x_target_low: UnsafePointer[
            Float16, MutUntrackedOrigin, address_space=AddressSpace.SHARED
        ],
        x_target_high: UnsafePointer[
            Float16, MutUntrackedOrigin, address_space=AddressSpace.SHARED
        ],
    ):
        slot = stage % BM32_STAGES
        offset = stage * BK
        if valid_low == 16:
            async_copy[16](
                x_source_low + offset, x_target_low + slot * XTILE32
            )
            async_copy[16](
                x_source_low + offset + 64,
                x_target_low + slot * XTILE32 + 64,
            )
        else:
            async_copy[16, fill=Float16(0)](
                x_source_low + offset,
                x_target_low + slot * XTILE32,
                Int32(0),
            )
            async_copy[16, fill=Float16(0)](
                x_source_low + offset + 64,
                x_target_low + slot * XTILE32 + 64,
                Int32(0),
            )
        if valid_high == 16:
            async_copy[16](
                x_source_high + offset, x_target_high + slot * XTILE32
            )
            async_copy[16](
                x_source_high + offset + 64,
                x_target_high + slot * XTILE32 + 64,
            )
        else:
            async_copy[16, fill=Float16(0)](
                x_source_high + offset,
                x_target_high + slot * XTILE32,
                Int32(0),
            )
            async_copy[16, fill=Float16(0)](
                x_source_high + offset + 64,
                x_target_high + slot * XTILE32 + 64,
                Int32(0),
            )
        resident_base = (
            resident
            + (row_tile * total_stages + stage) * RESIDENT_STAGE_BYTES
        ).address_space_cast[AddressSpace.GLOBAL]()
        code_source = resident_base + RESIDENT_SCALE_BYTES
        code_target = bs + slot * RESIDENT_CODE_BYTES + tid * 16
        async_copy[16](code_source + tid * 16, code_target)
        async_copy[16](code_source + tid * 16 + 2048, code_target + 2048)
        if tid < 32:
            async_copy[16](
                resident_base + tid * 16,
                ss + slot * RESIDENT_SCALE_BYTES + tid * 16,
            )
        async_copy_commit_group()

    comptime for preload in range(BM32_STAGES):
        issue_stage(
            min(stage_start + preload, stage_finish - 1),
            resident,
            row_tile,
            total_stages,
            tid,
            x_valid_low,
            x_valid_high,
            bs,
            ss,
            x_source_low,
            x_source_high,
            x_target_low,
            x_target_high,
        )
    async_copy_wait_group(1)
    barrier()

    var stage = stage_start
    while stage < stage_finish:
        slot = stage % BM32_STAGES
        comptime for local_k16 in range(2):
            group_in_stage = warp_id * 2 + local_k16
            comptime for n8 in range(8):
                shared_row = n8 * 8 + lane_row
                encoded_scale = ss[
                    slot * RESIDENT_SCALE_BYTES
                    + shared_row * 8
                    + group_in_stage
                ]
                code_base = (
                    bs
                    + slot * RESIDENT_CODE_BYTES
                    + shared_row * 64
                    + group_in_stage * 8
                )
                b = _decode_fragment(
                    code_base[lane4],
                    code_base[lane4 + 4],
                    encoded_scale,
                )
                a_low = ld_matrix[8](
                    a_base + slot * XTILE32 + warp_id * 32 + local_k16 * 16
                )
                mma(accumulators[n8], a_low, b, accumulators[n8])
                a_high = ld_matrix[8](
                    a_base
                    + slot * XTILE32
                    + 16 * LDK
                    + warp_id * 32
                    + local_k16 * 16
                )
                mma(accumulators[8 + n8], a_high, b, accumulators[8 + n8])

        barrier()
        if stage + BM32_STAGES < stage_finish:
            issue_stage(
                stage + BM32_STAGES,
                resident,
                row_tile,
                total_stages,
                tid,
                x_valid_low,
                x_valid_high,
                bs,
                ss,
                x_source_low,
                x_source_high,
                x_target_low,
                x_target_high,
            )
        remaining = stage_finish - stage - 2
        if remaining >= 1:
            async_copy_wait_group(1)
        else:
            async_copy_wait_group(0)
        barrier()
        stage += 1

    async_copy_wait_group(0)
    barrier()
    group = lane >> 2
    output_scale = inv_global_scale * 128.0
    scratch = bs.bitcast[Float32]()
    comptime for m_half in range(2):
        comptime for tile_group in range(2):
            comptime for local_n8 in range(4):
                comptime n8 = m_half * 8 + tile_group * 4 + local_n8
                scratch_base = (warp_id * 4 + local_n8) * 128
                scratch_index = group * 8 + lane4 * 2
                scratch[scratch_base + scratch_index] = accumulators[n8][0]
                scratch[scratch_base + scratch_index + 1] = accumulators[n8][1]
                scratch[scratch_base + scratch_index + 64] = accumulators[n8][2]
                scratch[scratch_base + scratch_index + 65] = accumulators[n8][3]
            barrier()
            if warp_id == 0:
                comptime for local_n8 in range(4):
                    comptime tile_n8 = tile_group * 4 + local_n8
                    scratch_index = group * 8 + lane4 * 2
                    var reduced = SIMD[DType.float32, 4](0.0)
                    comptime for warp_k in range(4):
                        scratch_base = (warp_k * 4 + local_n8) * 128
                        reduced[0] += scratch[scratch_base + scratch_index]
                        reduced[1] += scratch[scratch_base + scratch_index + 1]
                        reduced[2] += scratch[scratch_base + scratch_index + 64]
                        reduced[3] += scratch[scratch_base + scratch_index + 65]
                    output_row = row0 + tile_n8 * 8 + lane4 * 2
                    token_low = m_half * 16 + group
                    token_high = m_half * 16 + group + 8
                    comptime if outer_split_k == 1:
                        if output_row < n_rows:
                            output_index = _output_index[
                                output_layout, n_rows, BM32
                            ](token_low, output_row)
                            y[output_index] = Float16(
                                reduced[0] * output_scale
                            )
                            if output_row + 1 < n_rows:
                                y[output_index + 1] = Float16(
                                    reduced[1] * output_scale
                                )
                            output_index = _output_index[
                                output_layout, n_rows, BM32
                            ](token_high, output_row)
                            y[output_index] = Float16(
                                reduced[2] * output_scale
                            )
                            if output_row + 1 < n_rows:
                                y[output_index + 1] = Float16(
                                    reduced[3] * output_scale
                                )
                    else:
                        workspace = y.bitcast[Float32]()
                        part_base = split_id * BM32 * n_rows
                        if output_row < n_rows:
                            output_index = _output_index[
                                output_layout, n_rows, BM32
                            ](token_low, output_row)
                            workspace[
                                part_base + output_index
                            ] = reduced[0] * output_scale
                            if output_row + 1 < n_rows:
                                workspace[
                                    part_base + output_index + 1
                                ] = reduced[1] * output_scale
                            output_index = _output_index[
                                output_layout, n_rows, BM32
                            ](token_high, output_row)
                            workspace[
                                part_base + output_index
                            ] = reduced[2] * output_scale
                            if output_row + 1 < n_rows:
                                workspace[
                                    part_base + output_index + 1
                                ] = reduced[3] * output_scale
            barrier()


def gemm_nvfp4_ct_bm32_qkv_m32(
    workspace: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy QKV 4096x6144 dla 32 tokenów z trzema częściami K."""
    gemm_nvfp4_ct_direct_bm32_bn64_bk128[
        4096, 6144, 32, 3, OUTPUT_QKV_SEGMENTS
    ](workspace, resident, x, inv_global_scale)


def gemm_nvfp4_ct_bm32_o_m32(
    workspace: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy projekcję wyjściową 4096x4096 dla 32 tokenów."""
    gemm_nvfp4_ct_direct_bm32_bn64_bk128[
        4096, 4096, 32, 4, OUTPUT_TOKEN_MAJOR
    ](workspace, resident, x, inv_global_scale)


def gemm_nvfp4_ct_bm32_gateup_m32(
    y: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy gate i up 4096x22528 dla 32 tokenów bez split-K."""
    gemm_nvfp4_ct_direct_bm32_bn64_bk128[
        4096, 22528, 32, 1, OUTPUT_GATE_UP_SEGMENTS
    ](y, resident, x, inv_global_scale)


def gemm_nvfp4_ct_bm32_down_m32(
    workspace: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy down 11264x4096 dla 32 tokenów z czterema częściami K."""
    gemm_nvfp4_ct_direct_bm32_bn64_bk128[
        11264, 4096, 32, 4, OUTPUT_TOKEN_MAJOR
    ](workspace, resident, x, inv_global_scale)


def gemm_nvfp4_ct_bm32_qkv_m24(
    workspace: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy QKV dla 24 logicznych tokenów wewnątrz BM32."""
    gemm_nvfp4_ct_direct_bm32_bn64_bk128[
        4096, 6144, 24, 3, OUTPUT_QKV_SEGMENTS
    ](workspace, resident, x, inv_global_scale)


def gemm_nvfp4_ct_bm32_o_m24(
    workspace: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy projekcję wyjściową dla 24 logicznych tokenów."""
    gemm_nvfp4_ct_direct_bm32_bn64_bk128[
        4096, 4096, 24, 4, OUTPUT_TOKEN_MAJOR
    ](workspace, resident, x, inv_global_scale)


def gemm_nvfp4_ct_bm32_gateup_m24(
    y: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy gate i up dla 24 logicznych tokenów."""
    gemm_nvfp4_ct_direct_bm32_bn64_bk128[
        4096, 22528, 24, 1, OUTPUT_GATE_UP_SEGMENTS
    ](y, resident, x, inv_global_scale)


def gemm_nvfp4_ct_bm32_down_m24(
    workspace: UnsafePointer[Float16, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    inv_global_scale: Float32,
):
    """Liczy down dla 24 logicznych tokenów wewnątrz BM32."""
    gemm_nvfp4_ct_direct_bm32_bn64_bk128[
        11264, 4096, 24, 4, OUTPUT_TOKEN_MAJOR
    ](workspace, resident, x, inv_global_scale)
