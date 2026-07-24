# =============================================================================
# Plik: nvfp4_ct_decode.mojo
# Opis: Liczy małe batche z naturalnego układu S0 N64/K128 zgodnie z row GEMV.
# Przykład: gemv_batch_nvfp4_ct_s0_n64k128_f16_b4 liczy do czterech tokenów.
# =============================================================================

from std.gpu import block_idx, thread_idx
from std.gpu.primitives import warp
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from src.nvfp4_ct_layout import (
    NVFP4_CT_SCALE_BYTES,
    NVFP4_CT_TILE_BYTES,
    NVFP4_CT_TILE_COLS,
    NVFP4_CT_TILE_ROWS,
    nvfp4_ct_decode_s0,
)

comptime WARP_SIZE = 32
comptime ROWS_PER_BLOCK = 8


def gemv_batch_nvfp4_ct_s0_n64k128_f16_impl[batch_bucket: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    source_row_offset: Int,
    inv_global_scale: Float32,
):
    """Zachowuje kolejność arytmetyki row-major, zmieniając tylko adres S0."""
    tid = Int(thread_idx.x)
    lut = stack_allocation[
        16, Float32, address_space = AddressSpace.SHARED
    ]()
    comptime e2m1_vals = SIMD[DType.float32, 16](
        0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0,
        -0.0, -0.5, -1.0, -1.5, -2.0, -3.0, -4.0, -6.0,
    )
    if tid < 16:
        lut[tid] = e2m1_vals[tid]
    barrier()

    lane = tid % WARP_SIZE
    warp_id = tid // WARP_SIZE
    output_row = Int(block_idx.x) * ROWS_PER_BLOCK + warp_id
    if output_row >= n_rows:
        return

    physical_row = source_row_offset + output_row
    row_tile = physical_row // NVFP4_CT_TILE_ROWS
    tile_row = physical_row % NVFP4_CT_TILE_ROWS
    stages = n_cols // NVFP4_CT_TILE_COLS
    groups = n_cols // 16
    var acc = InlineArray[Float32, batch_bucket](fill=0.0)
    var group = lane
    while group < groups:
        stage = group // 8
        group_in_stage = group % 8
        tile_base = (row_tile * stages + stage) * NVFP4_CT_TILE_BYTES
        encoded_scale = weights[
            tile_base + tile_row * 8 + group_in_stage
        ]
        scale = Float32(nvfp4_ct_decode_s0(encoded_scale)) * (
            inv_global_scale / 128.0
        )
        qv = (
            weights
            + tile_base
            + NVFP4_CT_SCALE_BYTES
            + tile_row * 64
            + group_in_stage * 8
        ).load[width=8, alignment=8]()
        var lov = SIMD[DType.float32, 8]()
        var hiv = SIMD[DType.float32, 8]()
        comptime for j in range(8):
            lov[j] = lut[Int(qv[j] & 0x0F)]
            hiv[j] = lut[Int(qv[j] >> 4)]

        comptime for token in range(batch_bucket):
            var source_token = token
            if source_token > n_tokens - 1:
                source_token = n_tokens - 1
            xv = (x + source_token * n_cols + group * 16).load[
                width=16, alignment=32
            ]().cast[DType.float32]()
            x_even, x_odd = xv.deinterleave()
            acc[token] += scale * (
                (lov * x_even).reduce_add() + (hiv * x_odd).reduce_add()
            )
        group += WARP_SIZE

    comptime for token in range(batch_bucket):
        total = warp.sum(acc[token])
        if lane == 0 and token < n_tokens:
            y[token * n_rows + output_row] = Float16(total)


comptime gemv_nvfp4_ct_s0_n64k128_f16 = (
    gemv_batch_nvfp4_ct_s0_n64k128_f16_impl[1]
)
comptime gemv_batch_nvfp4_ct_s0_n64k128_f16_b4 = (
    gemv_batch_nvfp4_ct_s0_n64k128_f16_impl[4]
)
comptime gemv_batch_nvfp4_ct_s0_n64k128_f16_b8 = (
    gemv_batch_nvfp4_ct_s0_n64k128_f16_impl[8]
)
comptime gemv_batch_nvfp4_ct_s0_n64k128_f16_b16 = (
    gemv_batch_nvfp4_ct_s0_n64k128_f16_impl[16]
)
