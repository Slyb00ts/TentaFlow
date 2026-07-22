# =============================================================================
# Plik: q8_0_batch.mojo
# Opis: Weight-stationary MMQ Q8_0 x Q8_1 dla krotkich batchy weryfikatora.
# Przyklad: gemm_q8_0_i8mma_b3 liczy trzy tokeny jednym odczytem wag.
# =============================================================================

from std.gpu import WARP_SIZE, block_idx, thread_idx
from std.gpu.primitives import warp
from std.memory import bitcast
from src.decode_dp4a import _dp4a

comptime ROWS_PER_BLOCK = 8


def gemm_q8_0_small_impl[output_type: DType, token_tile: Int](
    y: UnsafePointer[Scalar[output_type], MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Liczy T=2/3/4, dekodujac kazdy blok Q8_0 raz na wszystkie tokeny."""
    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    warp_id = tid // WARP_SIZE
    row = Int(block_idx.x) * ROWS_PER_BLOCK + warp_id
    if row >= n_rows:
        return

    blocks_per_row = n_cols // 32
    row_base = row * blocks_per_row * 34
    var acc = InlineArray[Float32, token_tile](fill=0.0)
    var block = lane
    while block < blocks_per_row:
        offset = row_base + block * 34
        scale = Float32((weights + offset).bitcast[Float16]()[0])
        packed = (weights + offset + 2).bitcast[UInt16]().load[width=16]()
        weight_quant = bitcast[DType.int8, 32](packed).cast[DType.int32]()
        column = block * 32
        comptime for token in range(token_tile):
            if token < n_tokens:
                activation_quant = (xq + token * n_cols + column).load[
                    width=32, alignment=32
                ]().cast[DType.int32]()
                activation_scale = xd[block * n_tokens + token]
                acc[token] += scale * activation_scale * (
                    weight_quant * activation_quant
                ).reduce_add().cast[DType.float32]()[0]
        block += WARP_SIZE

    comptime for token in range(token_tile):
        total = warp.sum(acc[token])
        if lane == 0 and token < n_tokens:
            y[token * n_rows + row] = Scalar[output_type](total)


def gemm_q8_0_small_dp4a_impl[output_type: DType, token_tile: Int](
    y: UnsafePointer[Scalar[output_type], MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Wariant NVIDIA liczacy 32 iloczyny int8 osmioma instrukcjami DP4A."""
    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    warp_id = tid // WARP_SIZE
    row = Int(block_idx.x) * 4 + warp_id
    if row >= n_rows:
        return

    blocks_per_row = n_cols // 32
    row_base = row * blocks_per_row * 34
    var acc = InlineArray[Float32, token_tile](fill=0.0)
    var block = lane
    while block < blocks_per_row:
        offset = row_base + block * 34
        scale = Float32((weights + offset).bitcast[Float16]()[0])
        packed = (weights + offset + 2).bitcast[UInt16]().load[width=16]()
        weight_quant = bitcast[DType.int32, 8](packed)
        column = block * 32
        comptime for token in range(token_tile):
            activation_quant = (xq + token * n_cols + column).bitcast[Int32]().load[
                width=8, alignment=32
            ]()
            var integer_dot: Int32 = 0
            comptime for word in range(8):
                integer_dot = _dp4a(weight_quant[word], activation_quant[word], integer_dot)
            activation_scale = xd[block * n_tokens + token]
            acc[token] += scale * activation_scale * Float32(integer_dot)
        block += WARP_SIZE

    comptime for token in range(token_tile):
        total = warp.sum(acc[token])
        if lane == 0 and token < n_tokens:
            y[token * n_rows + row] = Scalar[output_type](total)


def gemm_q8_0_f16_exact_impl[output_type: DType, token_tile: Int, rows_per_block: Int](
    y: UnsafePointer[Scalar[output_type], MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Współdzieli odczyt Q8_0 dla T=3/4 bez kwantyzacji aktywacji F16."""
    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    warp_id = tid // WARP_SIZE
    row = Int(block_idx.x) * rows_per_block + warp_id
    if row >= n_rows:
        return

    blocks_per_row = n_cols // 32
    row_base = row * blocks_per_row * 34
    var acc = InlineArray[Float32, token_tile](fill=0.0)
    var block = lane
    while block < blocks_per_row:
        offset = row_base + block * 34
        scale = Float32((weights + offset).bitcast[Float16]()[0])
        packed = (weights + offset + 2).bitcast[UInt16]().load[width=16]()
        weight_quant = bitcast[DType.int8, 32](packed).cast[DType.float32]()
        column = block * 32
        comptime for token in range(token_tile):
            if token < n_tokens:
                activation = (x + token * n_cols + column).load[
                    width=32, alignment=64
                ]().cast[DType.float32]()
                acc[token] += scale * (weight_quant * activation).reduce_add()
        block += WARP_SIZE

    comptime for token in range(token_tile):
        total = warp.sum(acc[token])
        if lane == 0 and token < n_tokens:
            y[token * n_rows + row] = Scalar[output_type](total)


comptime gemm_q8_0_i8mma_b2 = gemm_q8_0_small_impl[DType.float16, 2]
comptime gemm_q8_0_i8mma_b3 = gemm_q8_0_small_impl[DType.float16, 3]
comptime gemm_q8_0_i8mma_b4 = gemm_q8_0_small_impl[DType.float16, 4]
comptime gemm_q8_0_i8mma_b8 = gemm_q8_0_small_impl[DType.float16, 8]
comptime gemm_q8_0_i8mma_out_f32_b3 = gemm_q8_0_small_impl[DType.float32, 3]
comptime gemm_q8_0_i8mma_out_f32_b4 = gemm_q8_0_small_impl[DType.float32, 4]
comptime gemm_q8_0_dp4a_b3_nvidia = gemm_q8_0_small_dp4a_impl[DType.float16, 3]
comptime gemm_q8_0_dp4a_b4_nvidia = gemm_q8_0_small_dp4a_impl[DType.float16, 4]
comptime gemm_q8_0_dp4a_out_f32_b3_nvidia = gemm_q8_0_small_dp4a_impl[DType.float32, 3]
comptime gemm_q8_0_dp4a_out_f32_b4_nvidia = gemm_q8_0_small_dp4a_impl[DType.float32, 4]
comptime gemm_q8_0_f16_exact_out_f32_b3 = gemm_q8_0_f16_exact_impl[DType.float32, 3, 8]
comptime gemm_q8_0_f16_exact_out_f32_b4 = gemm_q8_0_f16_exact_impl[DType.float32, 4, 8]
comptime gemm_q8_0_f16_exact_out_f32_b8 = gemm_q8_0_f16_exact_impl[DType.float32, 8, 8]
