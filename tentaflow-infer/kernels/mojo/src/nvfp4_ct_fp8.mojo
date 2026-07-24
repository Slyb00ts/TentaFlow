# =============================================================================
# Plik: nvfp4_ct_fp8.mojo
# Opis: Pakuje okna naturalnego układu S0 N64/K128 do wierszy E4M3.
# Przykład: pack_nvfp4_ct_s0_fp8 obsługuje projekcje prefill bez kopii wag.
# =============================================================================

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.memory import AddressSpace
from std.gpu.sync import barrier
from std.memory import bitcast, stack_allocation
from src.nvfp4_ct_layout import (
    NVFP4_CT_CODE_BYTES,
    NVFP4_CT_GROUP_COLS,
    NVFP4_CT_SCALE_BYTES,
    NVFP4_CT_TILE_BYTES,
    NVFP4_CT_TILE_COLS,
    NVFP4_CT_TILE_ROWS,
    nvfp4_ct_decode_s0,
)


def _decode_e2m1(raw: UInt8) -> Float32:
    magnitude = Int(raw & 0x07)
    var value = Float32(0.0)
    if magnitude == 1:
        value = 0.5
    elif magnitude == 2:
        value = 1.0
    elif magnitude == 3:
        value = 1.5
    elif magnitude == 4:
        value = 2.0
    elif magnitude == 5:
        value = 3.0
    elif magnitude == 6:
        value = 4.0
    elif magnitude == 7:
        value = 6.0
    return -value if (raw & 0x08) != 0 else value


def _weight_value(
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    row: Int,
    col: Int,
    n_cols: Int,
    inv_global_scale: Float32,
) -> Float32:
    stages_per_row_tile = n_cols // NVFP4_CT_TILE_COLS
    stage = (
        (row // NVFP4_CT_TILE_ROWS) * stages_per_row_tile
        + col // NVFP4_CT_TILE_COLS
    )
    row_in_tile = row % NVFP4_CT_TILE_ROWS
    group_in_stage = (col % NVFP4_CT_TILE_COLS) // NVFP4_CT_GROUP_COLS
    group_offset = row_in_tile * 8 + group_in_stage
    base = stage * NVFP4_CT_TILE_BYTES
    encoded_scale = resident[base + group_offset]
    code = resident[
        base
        + NVFP4_CT_SCALE_BYTES
        + group_offset * 8
        + (col % NVFP4_CT_GROUP_COLS) // 2
    ]
    nibble = code & 0x0F if col % 2 == 0 else (code >> 4) & 0x0F
    scale = Float32(nvfp4_ct_decode_s0(encoded_scale)) * (1.0 / 128.0)
    return _decode_e2m1(nibble) * scale * inv_global_scale


def pack_nvfp4_ct_s0_fp8(
    output: UnsafePointer[Int8, MutAnyOrigin],
    output_scales: UnsafePointer[Float32, MutAnyOrigin],
    resident: UnsafePointer[UInt8, MutAnyOrigin],
    n_cols: Int,
    source_row_offset: Int,
    n_rows: Int,
    inv_global_scale: Float32,
):
    """Kwantyzuje wskazane wiersze do E4M3 z jedną skalą FP32 na wiersz."""
    row = Int(block_idx.x)
    if row >= n_rows:
        return
    tid = Int(thread_idx.x)
    nthreads = Int(block_dim.x)
    source_row = source_row_offset + row

    var local = Float32(0.0)
    var col = tid
    while col < n_cols:
        local = max(
            local,
            abs(
                _weight_value(
                    resident, source_row, col, n_cols, inv_global_scale
                )
            ),
        )
        col += nthreads

    reduction = stack_allocation[
        256, Float32, address_space=AddressSpace.SHARED
    ]()
    reduction[tid] = local
    barrier()
    var stride = nthreads // 2
    while stride > 0:
        if tid < stride:
            reduction[tid] = max(reduction[tid], reduction[tid + stride])
        barrier()
        stride //= 2

    amax = reduction[0]
    if tid == 0:
        output_scales[row] = amax / 448.0 if amax != 0.0 else 0.0
    inv = 448.0 / amax if amax != 0.0 else 0.0
    col = tid
    while col < n_cols:
        encoded = Scalar[DType.float8_e4m3fn](
            _weight_value(
                resident, source_row, col, n_cols, inv_global_scale
            ) * inv
        )
        output[row * n_cols + col] = bitcast[DType.int8, 1](encoded)
        col += nthreads

