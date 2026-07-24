# =============================================================================
# Plik: nvfp4_tile128_coop_decode.mojo
# Opis: Kooperacyjny decode DP4A z mikropaczkami K32 dla tile-major N128/K64.
# Przyklad: gemv_nvfp4_tile128_coop_q8_1_f16 liczy osiem wierszy na CTA.
# =============================================================================

from std.gpu import block_idx, thread_idx
from std.gpu.memory import AddressSpace
from std.gpu.primitives import warp
from std.gpu.sync import barrier
from std.memory import bitcast, stack_allocation
from src.decode_dp4a import _dp4a
from src.nvfp4_gguf_batch import _ue4m3_value

comptime WARP = 32
comptime ROWS_PER_CTA = 8
comptime M1_ROWS_PER_CTA = 4
comptime BLOCKS_PER_CHUNK = 32
comptime TILE_ROWS = 128
comptime TILE_BLOCK_BYTES = TILE_ROWS * 36
comptime STAGE_SCALE_BYTES = TILE_ROWS * 2
comptime STAGE_BYTES = TILE_ROWS * 18


def gemv_nvfp4_tile128_coop_q8_1_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    output_scale: Float32,
):
    """Zachowuje mapowanie lane->blok i drzewo redukcji row-major."""
    tid = Int(thread_idx.x)
    lane = tid % WARP
    warp_id = tid // WARP
    row0 = Int(block_idx.x) * M1_ROWS_PER_CTA
    row = row0 + warp_id
    blocks_per_row = n_cols // 64

    scales = stack_allocation[
        M1_ROWS_PER_CTA * BLOCKS_PER_CHUNK * 4,
        UInt8,
        address_space=AddressSpace.SHARED,
    ]()
    codes = stack_allocation[
        M1_ROWS_PER_CTA * BLOCKS_PER_CHUNK * 32,
        UInt8,
        alignment=16,
        address_space=AddressSpace.SHARED,
    ]()
    x_codes = stack_allocation[
        BLOCKS_PER_CHUNK * 64,
        Int8,
        alignment=32,
        address_space=AddressSpace.SHARED,
    ]()
    x_scales = stack_allocation[
        BLOCKS_PER_CHUNK * 2,
        Float32,
        address_space=AddressSpace.SHARED,
    ]()
    lut = stack_allocation[16, Int8, address_space=AddressSpace.SHARED]()
    comptime values = SIMD[DType.int8, 16](
        0, 1, 2, 3, 4, 6, 8, 12,
        0, -1, -2, -3, -4, -6, -8, -12,
    )
    if tid < 16:
        lut[tid] = values[tid]

    tile = row0 // TILE_ROWS
    tile_row0 = row0 % TILE_ROWS
    var acc: Float32 = 0.0
    var chunk = 0
    while chunk < blocks_per_row:
        block_local = tid // M1_ROWS_PER_CTA
        row_local = tid % M1_ROWS_PER_CTA
        block = chunk + block_local
        if block < blocks_per_row:
            layout_row = tile_row0 + row_local
            tile_block = (tile * blocks_per_row + block) * TILE_BLOCK_BYTES
            shared_unit = row_local * BLOCKS_PER_CHUNK + block_local
            comptime for stage in range(2):
                stage_base = tile_block + stage * STAGE_BYTES
                comptime for subblock in range(2):
                    group = stage * 2 + subblock
                    layout_thread = layout_row * 2 + subblock
                    scales[shared_unit * 4 + group] = weights[
                        stage_base + layout_thread
                    ]
                    raw = (
                        weights + stage_base + STAGE_SCALE_BYTES
                        + layout_thread * 8
                    ).load[width=8, alignment=8]()
                    (codes + shared_unit * 32 + group * 8).store[
                        width=8, alignment=8
                    ](raw)
        activation_offset = chunk * 64 + tid * 16
        if activation_offset < n_cols:
            raw_activation = (xq + activation_offset).load[
                width=16, alignment=16
            ]()
            (x_codes + tid * 16).store[width=16, alignment=16](raw_activation)
        if tid < BLOCKS_PER_CHUNK * 2:
            scale_index = chunk * 2 + tid
            if scale_index < n_cols // 32:
                x_scales[tid] = xd[scale_index]
        barrier()

        block = chunk + lane
        if row < n_rows and block < blocks_per_row:
            shared_unit = warp_id * BLOCKS_PER_CHUNK + lane
            comptime for group in range(4):
                raw = (codes + shared_unit * 32 + group * 8).load[
                    width=8, alignment=8
                ]()
                var low = SIMD[DType.int8, 8]()
                var high = SIMD[DType.int8, 8]()
                comptime for element in range(8):
                    low[element] = lut[Int(raw[element] & 0x0F)]
                    high[element] = lut[Int(raw[element] >> 4)]
                packed_low = bitcast[DType.int32, 2](low)
                packed_high = bitcast[DType.int32, 2](high)
                activation = (x_codes + lane * 64 + group * 16).bitcast[
                    Int32
                ]().load[width=4, alignment=4]()
                var integer_dot: Int32 = 0
                comptime for part in range(2):
                    integer_dot = _dp4a(
                        packed_low[part], activation[part], integer_dot
                    )
                    integer_dot = _dp4a(
                        packed_high[part], activation[2 + part], integer_dot
                    )
                acc += (
                    Float32(integer_dot)
                    * x_scales[lane * 2 + group // 2]
                    * _ue4m3_value(scales[shared_unit * 4 + group])
                    * 0.5
                )
        barrier()
        chunk += BLOCKS_PER_CHUNK

    total = warp.sum(acc)
    if lane == 0 and row < n_rows:
        y[row] = Float16(total * output_scale)
