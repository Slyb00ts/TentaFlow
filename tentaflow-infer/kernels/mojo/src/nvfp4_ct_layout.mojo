# =============================================================================
# Plik: nvfp4_ct_layout.mojo
# Opis: Definiuje naturalny układ compressed-tensors NVFP4 S0 N64/K128.
# Przykład: repack_nvfp4_ct_s0_n64k128_into zapisuje chunk do bufora wagi.
# =============================================================================

from std.gpu import block_idx, thread_idx
from std.memory import bitcast

comptime NVFP4_CT_TILE_ROWS = 64
comptime NVFP4_CT_TILE_COLS = 128
comptime NVFP4_CT_GROUP_COLS = 16
comptime NVFP4_CT_SCALE_BYTES = 512
comptime NVFP4_CT_CODE_BYTES = 4096
comptime NVFP4_CT_TILE_BYTES = 4608
comptime NVFP4_CT_K64_SCALE_BYTES = 256
comptime NVFP4_CT_K64_CODE_BYTES = 2048
comptime NVFP4_CT_K64_TILE_BYTES = 2304


def nvfp4_ct_s0_from_e4m3(raw: UInt8) -> UInt8:
    """Koduje legalną dodatnią E4M3 po kompensacji x128 do S0E5M3.

    Ujemne skale i NaN są niedozwolone w źródle. Zwracany dla nich kod 0xF9
    dekoduje się do half NaN, aby błąd nie mógł zostać ukryty jako liczba.
    """
    if (raw & 0x80) != 0 or raw == 0x7F:
        return UInt8(0xF9)
    exponent = (raw >> 3) & 0x0F
    mantissa = raw & 0x07
    if exponent == 0:
        if mantissa == 0:
            return UInt8(0)
        if mantissa == 1:
            return UInt8(0x68)
        if mantissa == 2:
            return UInt8(0x70)
        if mantissa == 3:
            return UInt8(0x74)
        if mantissa == 4:
            return UInt8(0x78)
        if mantissa == 5:
            return UInt8(0x7A)
        if mantissa == 6:
            return UInt8(0x7C)
        return UInt8(0x7E)
    return UInt8((exponent + 15) << 3 | mantissa)


def nvfp4_ct_decode_s0(raw: UInt8) -> Float16:
    """Odtwarza skalę bloku jako FP16 bez konwersji FP8."""
    bits = UInt16(raw) << 7
    return bitcast[DType.float16, 1](
        SIMD[DType.uint16, 1](bits)
    )[0]


def nvfp4_ct_decode_e2m1x4(
    raw0: UInt8,
    raw1: UInt8,
    encoded_scale: UInt8,
) -> SIMD[DType.float16, 4]:
    """Dekoduje cztery E2M1 metodą skip-flop i nakłada skalę S0."""
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
    return bitcast[DType.float16, 4](bits) * nvfp4_ct_decode_s0(
        encoded_scale
    )


def repack_nvfp4_ct_s0_n64k128_into(
    target: UnsafePointer[UInt8, MutAnyOrigin],
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[UInt8, MutAnyOrigin],
    n_cols: Int,
    source_rows: Int,
    target_row_offset: Int,
):
    """Przepakowuje pełne kafle wierszy z ograniczonego bufora uploadu."""
    tid = Int(thread_idx.x)
    stage = Int(block_idx.x)
    stages_per_row_tile = n_cols // NVFP4_CT_TILE_COLS
    source_row_tile = stage // stages_per_row_tile
    k_stage = stage % stages_per_row_tile
    row_in_tile = tid // 2
    part = tid % 2
    source_row = source_row_tile * NVFP4_CT_TILE_ROWS + row_in_tile
    if source_row >= source_rows:
        return

    target_row_tile = (
        target_row_offset // NVFP4_CT_TILE_ROWS + source_row_tile
    )
    target_stage = target_row_tile * stages_per_row_tile + k_stage
    target_base = target_stage * NVFP4_CT_TILE_BYTES
    comptime for local_group in range(4):
        group_in_stage = part * 4 + local_group
        source_group = k_stage * 8 + group_in_stage
        raw_scale = scales[
            source_row * (n_cols // NVFP4_CT_GROUP_COLS) + source_group
        ]
        target[
            target_base + row_in_tile * 8 + group_in_stage
        ] = nvfp4_ct_s0_from_e4m3(raw_scale)
        codes = (
            packed
            + source_row * (n_cols // 2)
            + source_group * 8
        ).load[width=8, alignment=8]()
        (
            target
            + target_base
            + NVFP4_CT_SCALE_BYTES
            + (row_in_tile * 8 + group_in_stage) * 8
        ).store[width=8, alignment=8](codes)


def repack_nvfp4_ct_s0_n64k64_into(
    target: UnsafePointer[UInt8, MutAnyOrigin],
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[UInt8, MutAnyOrigin],
    n_cols: Int,
    source_rows: Int,
    target_row_offset: Int,
):
    """Przepakowuje źródło do kafli N64/K64 o tym samym rozmiarze całkowitym."""
    row_in_tile = Int(thread_idx.x)
    if row_in_tile >= NVFP4_CT_TILE_ROWS:
        return
    stage = Int(block_idx.x)
    stages_per_row_tile = n_cols // 64
    source_row_tile = stage // stages_per_row_tile
    k_stage = stage % stages_per_row_tile
    source_row = source_row_tile * NVFP4_CT_TILE_ROWS + row_in_tile
    if source_row >= source_rows:
        return
    target_row_tile = (
        target_row_offset // NVFP4_CT_TILE_ROWS + source_row_tile
    )
    target_stage = target_row_tile * stages_per_row_tile + k_stage
    target_base = target_stage * NVFP4_CT_K64_TILE_BYTES
    comptime for local_group in range(4):
        source_group = k_stage * 4 + local_group
        raw_scale = scales[
            source_row * (n_cols // NVFP4_CT_GROUP_COLS) + source_group
        ]
        target[
            target_base + row_in_tile * 4 + local_group
        ] = nvfp4_ct_s0_from_e4m3(raw_scale)
        codes = (
            packed
            + source_row * (n_cols // 2)
            + source_group * 8
        ).load[width=8, alignment=8]()
        (
            target
            + target_base
            + NVFP4_CT_K64_SCALE_BYTES
            + (row_in_tile * 4 + local_group) * 8
        ).store[width=8, alignment=8](codes)
