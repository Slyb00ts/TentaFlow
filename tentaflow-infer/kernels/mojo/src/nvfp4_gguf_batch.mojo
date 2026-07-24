# =============================================================================
# Plik: nvfp4_gguf_batch.mojo
# Opis: Natywny kafelkowany GEMM dla blokow GGUF NVFP4 36 B / 64 wartosci.
# Przyklad: gemm_nvfp4_gguf_f16_b3 wspoldzieli odczyt wag dla trzech tokenow MTP.
# =============================================================================

from std.gpu import WARP_SIZE, block_idx, thread_idx
from std.gpu.primitives import warp
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import bitcast, stack_allocation
from src.gemv2 import _e2m1x8

comptime RAW_TILE_BLOCKS = 8


def _e2m1_value(code: UInt8) -> Float32:
    magnitude_code = Int(code & 0x07)
    var magnitude: Float32 = 0.0
    if magnitude_code == 1:
        magnitude = 0.5
    elif magnitude_code == 2:
        magnitude = 1.0
    elif magnitude_code == 3:
        magnitude = 1.5
    elif magnitude_code == 4:
        magnitude = 2.0
    elif magnitude_code == 5:
        magnitude = 3.0
    elif magnitude_code == 6:
        magnitude = 4.0
    elif magnitude_code == 7:
        magnitude = 6.0
    if (code & 0x08) != 0:
        return -magnitude
    return magnitude


def _ue4m3_value(code: UInt8) -> Float32:
    """Dekoduje dodatnia skale UE4M3 uzywana przez blok GGUF NVFP4."""
    if code == 0 or code == 0x7F:
        return 0.0
    exponent = Int((code >> 3) & 0x0F)
    mantissa = Int(code & 0x07)
    if exponent == 0:
        return Float32(mantissa) * (1.0 / 512.0)
    bits = UInt32((exponent + 120) << 23 | mantissa << 20)
    return bitcast[DType.float32, 1](SIMD[DType.uint32, 1](bits))[0]


def gemm_nvfp4_gguf_f16_impl[token_tile: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    output_scale: Float32,
):
    """Liczy Y[T,N] = X[T,K] * W[N,K]^T bez repacku i pelnej dekwantyzacji.

    Grid ma ksztalt (n_rows, ceil(n_tokens / token_tile)), a blok
    token_tile*WARP_SIZE. Kazdy warp liczy jeden token, natomiast caly blok
    wspoldzieli kafel surowych wag. WARP_SIZE pochodzi z docelowego backendu.
    """
    row = Int(block_idx.x)
    token_base = Int(block_idx.y) * token_tile
    if row >= n_rows or token_base >= n_tokens:
        return

    tid = Int(thread_idx.x)
    lane = tid % WARP_SIZE
    token = tid // WARP_SIZE
    target_token = token_base + token
    var source_token = target_token
    if source_token >= n_tokens:
        source_token = n_tokens - 1
    blocks_per_row = n_cols // 64
    row_base = row * blocks_per_row * 36
    shared_weights = stack_allocation[
        RAW_TILE_BLOCKS * 36, UInt8, address_space=AddressSpace.SHARED
    ]()
    var acc: Float32 = 0.0

    var tile = 0
    while tile < blocks_per_row:
        var tile_blocks = blocks_per_row - tile
        if tile_blocks > RAW_TILE_BLOCKS:
            tile_blocks = RAW_TILE_BLOCKS
        var byte = tid
        while byte < tile_blocks * 36:
            shared_weights[byte] = weights[row_base + tile * 36 + byte]
            byte += token_tile * WARP_SIZE
        barrier()

        if target_token < n_tokens:
            var group = lane
            while group < tile_blocks * 4:
                block = group // 4
                subblock = group % 4
                base = block * 36
                codes = (shared_weights + base + 4 + subblock * 8).load[
                    width=8, alignment=4
                ]()
                low = _e2m1x8(codes & 0x0F)
                high = _e2m1x8(codes >> 4)
                x_base = (
                    source_token * n_cols
                    + (tile + block) * 64
                    + subblock * 16
                )
                x_low = (x + x_base).load[width=8, alignment=16]().cast[
                    DType.float32
                ]()
                x_high = (x + x_base + 8).load[width=8, alignment=16]().cast[
                    DType.float32
                ]()
                acc += _ue4m3_value(shared_weights[base + subblock]) * (
                    (low * x_low).reduce_add() + (high * x_high).reduce_add()
                )
                group += WARP_SIZE
        barrier()
        tile += RAW_TILE_BLOCKS

    total = warp.sum(acc)
    if lane == 0 and target_token < n_tokens:
        y[target_token * n_rows + row] = Float16(total * output_scale)


def gemm_nvfp4_gguf_f16_small_impl[token_tile: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    output_scale: Float32,
):
    """Liczy dokladny batch T=2/3/4 jednym odczytem surowych wag na warp."""
    row = Int(block_idx.x)
    if row >= n_rows:
        return

    lane = Int(thread_idx.x)
    blocks_per_row = n_cols // 64
    row_base = row * blocks_per_row * 36
    var acc = InlineArray[Float32, token_tile](fill=0.0)
    var group = lane
    while group < blocks_per_row * 4:
        block = group // 4
        subblock = group % 4
        base = row_base + block * 36
        codes = (weights + base + 4 + subblock * 8).load[
            width=8, alignment=4
        ]()
        low = _e2m1x8(codes & 0x0F)
        high = _e2m1x8(codes >> 4)
        scale = _ue4m3_value(weights[base + subblock])
        column = block * 64 + subblock * 16

        comptime for token in range(token_tile):
            x_low = (x + token * n_cols + column).load[
                width=8, alignment=16
            ]().cast[DType.float32]()
            x_high = (x + token * n_cols + column + 8).load[
                width=8, alignment=16
            ]().cast[DType.float32]()
            acc[token] += scale * (
                (low * x_low).reduce_add() + (high * x_high).reduce_add()
            )
        group += WARP_SIZE

    comptime for token in range(token_tile):
        total = warp.sum(acc[token])
        if lane == 0 and token < n_tokens:
            y[token * n_rows + row] = Float16(total * output_scale)


def gemm_nvfp4_gguf_out_f32_b2(
    y: UnsafePointer[Float32, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    output_scale: Float32,
):
    """Liczy dwa wiersze logitow F32 jednym odczytem wag na warp."""
    row = Int(block_idx.x)
    if row >= n_rows:
        return

    lane = Int(thread_idx.x)
    blocks_per_row = n_cols // 64
    row_base = row * blocks_per_row * 36
    var acc0: Float32 = 0.0
    var acc1: Float32 = 0.0
    var group = lane
    while group < blocks_per_row * 4:
        block = group // 4
        subblock = group % 4
        base = row_base + block * 36
        codes = (weights + base + 4 + subblock * 8).load[
            width=8, alignment=4
        ]()
        low = _e2m1x8(codes & 0x0F)
        high = _e2m1x8(codes >> 4)
        scale = _ue4m3_value(weights[base + subblock])
        column = block * 64 + subblock * 16
        x0_low = (x + column).load[width=8, alignment=16]().cast[
            DType.float32
        ]()
        x0_high = (x + column + 8).load[width=8, alignment=16]().cast[
            DType.float32
        ]()
        x1_low = (x + n_cols + column).load[width=8, alignment=16]().cast[
            DType.float32
        ]()
        x1_high = (x + n_cols + column + 8).load[
            width=8, alignment=16
        ]().cast[DType.float32]()
        acc0 += scale * (
            (low * x0_low).reduce_add() + (high * x0_high).reduce_add()
        )
        acc1 += scale * (
            (low * x1_low).reduce_add() + (high * x1_high).reduce_add()
        )
        group += WARP_SIZE

    total0 = warp.sum(acc0)
    total1 = warp.sum(acc1)
    if lane == 0:
        y[row] = total0 * output_scale
        if n_tokens > 1:
            y[n_rows + row] = total1 * output_scale


def gemm_nvfp4_gguf_out_f32_small_impl[token_tile: Int](
    y: UnsafePointer[Float32, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    output_scale: Float32,
):
    """Liczy kilka wierszy logitów F32 jednym odczytem wag na warp."""
    row = Int(block_idx.x)
    if row >= n_rows:
        return

    lane = Int(thread_idx.x)
    blocks_per_row = n_cols // 64
    row_base = row * blocks_per_row * 36
    var acc = InlineArray[Float32, token_tile](fill=0.0)
    var group = lane
    while group < blocks_per_row * 4:
        block = group // 4
        subblock = group % 4
        base = row_base + block * 36
        codes = (weights + base + 4 + subblock * 8).load[
            width=8, alignment=4
        ]()
        low = _e2m1x8(codes & 0x0F)
        high = _e2m1x8(codes >> 4)
        scale = _ue4m3_value(weights[base + subblock])
        column = block * 64 + subblock * 16
        comptime for token in range(token_tile):
            if token < n_tokens:
                x_low = (x + token * n_cols + column).load[
                    width=8, alignment=16
                ]().cast[DType.float32]()
                x_high = (x + token * n_cols + column + 8).load[
                    width=8, alignment=16
                ]().cast[DType.float32]()
                acc[token] += scale * (
                    (low * x_low).reduce_add() + (high * x_high).reduce_add()
                )
        group += WARP_SIZE

    comptime for token in range(token_tile):
        total = warp.sum(acc[token])
        if lane == 0 and token < n_tokens:
            y[token * n_rows + row] = total * output_scale


def gemm_nvfp4_gguf_f16_nvidia_impl[token_tile: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    output_scale: Float32,
):
    """Wariant NVIDIA z LUT E2M1, szerokim odczytem i dwoma warpami CTA."""
    tid = Int(thread_idx.x)
    lut = stack_allocation[16, Float32, address_space=AddressSpace.SHARED]()
    comptime values = SIMD[DType.float32, 16](
        0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0,
        -0.0, -0.5, -1.0, -1.5, -2.0, -3.0, -4.0, -6.0,
    )
    if tid < 16:
        lut[tid] = values[tid]
    barrier()

    warp_id = tid // WARP_SIZE
    lane = tid % WARP_SIZE
    row = Int(block_idx.x) * 2 + warp_id
    if row >= n_rows:
        return
    blocks_per_row = n_cols // 64
    row_base = row * blocks_per_row * 36
    var acc = InlineArray[Float32, token_tile](fill=0.0)
    var group = lane
    while group < blocks_per_row * 4:
        block = group // 4
        subblock = group % 4
        base = row_base + block * 36
        codes = (weights + base + 4 + subblock * 8).load[width=8, alignment=4]()
        var low = SIMD[DType.float32, 8]()
        var high = SIMD[DType.float32, 8]()
        comptime for element in range(8):
            low[element] = lut[Int(codes[element] & 0x0F)]
            high[element] = lut[Int(codes[element] >> 4)]
        scale = _ue4m3_value(weights[base + subblock])
        column = block * 64 + subblock * 16
        comptime for token in range(token_tile):
            if token < n_tokens:
                activation = (x + token * n_cols + column).load[
                    width=16, alignment=32
                ]().cast[DType.float32]()
                acc[token] += scale * (
                    (low * activation.slice[8, offset=0]()).reduce_add()
                    + (high * activation.slice[8, offset=8]()).reduce_add()
                )
        group += WARP_SIZE

    comptime for token in range(token_tile):
        total = warp.sum(acc[token])
        if lane == 0 and token < n_tokens:
            y[token * n_rows + row] = Float16(total * output_scale)


def gemv_nvfp4_gguf_out_f32_nvidia(
    y: UnsafePointer[Float32, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    output_scale: Float32,
):
    """Wariant B1 NVIDIA zapisujacy logity F32 dla argmax MTP."""
    tid = Int(thread_idx.x)
    lut = stack_allocation[16, Float32, address_space=AddressSpace.SHARED]()
    comptime values = SIMD[DType.float32, 16](
        0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0,
        -0.0, -0.5, -1.0, -1.5, -2.0, -3.0, -4.0, -6.0,
    )
    if tid < 16:
        lut[tid] = values[tid]
    barrier()

    warp_id = tid // WARP_SIZE
    lane = tid % WARP_SIZE
    row = Int(block_idx.x) * 2 + warp_id
    if row >= n_rows:
        return
    blocks_per_row = n_cols // 64
    row_base = row * blocks_per_row * 36
    var acc: Float32 = 0.0
    var group = lane
    while group < blocks_per_row * 4:
        block = group // 4
        subblock = group % 4
        base = row_base + block * 36
        codes = (weights + base + 4 + subblock * 8).load[width=8, alignment=4]()
        var low = SIMD[DType.float32, 8]()
        var high = SIMD[DType.float32, 8]()
        comptime for element in range(8):
            low[element] = lut[Int(codes[element] & 0x0F)]
            high[element] = lut[Int(codes[element] >> 4)]
        scale = _ue4m3_value(weights[base + subblock])
        column = block * 64 + subblock * 16
        activation = (x + column).load[width=16, alignment=32]().cast[DType.float32]()
        acc += scale * (
            (low * activation.slice[8, offset=0]()).reduce_add()
            + (high * activation.slice[8, offset=8]()).reduce_add()
        )
        group += WARP_SIZE

    total = warp.sum(acc)
    if lane == 0:
        y[row] = total * output_scale
comptime gemm_nvfp4_gguf_f16_b2 = gemm_nvfp4_gguf_f16_small_impl[2]
comptime gemm_nvfp4_gguf_f16_b3 = gemm_nvfp4_gguf_f16_small_impl[3]
comptime gemm_nvfp4_gguf_f16_b4 = gemm_nvfp4_gguf_f16_small_impl[4]
comptime gemm_nvfp4_gguf_f16_b1_nvidia = gemm_nvfp4_gguf_f16_nvidia_impl[1]
comptime gemm_nvfp4_gguf_out_f32_b1_nvidia = gemv_nvfp4_gguf_out_f32_nvidia
comptime gemm_nvfp4_gguf_f16_b3_nvidia = gemm_nvfp4_gguf_f16_nvidia_impl[3]
comptime gemm_nvfp4_gguf_f16_b4_nvidia = gemm_nvfp4_gguf_f16_nvidia_impl[4]
comptime gemm_nvfp4_gguf_f16_b8_nvidia = gemm_nvfp4_gguf_f16_nvidia_impl[8]
comptime gemm_nvfp4_gguf_f16_b8 = gemm_nvfp4_gguf_f16_impl[8]
comptime gemm_nvfp4_gguf_f16_b16 = gemm_nvfp4_gguf_f16_impl[16]
comptime gemm_nvfp4_gguf_out_f32_b4 = gemm_nvfp4_gguf_out_f32_small_impl[4]
comptime gemm_nvfp4_gguf_out_f32_b8 = gemm_nvfp4_gguf_out_f32_small_impl[8]
comptime gemm_nvfp4_gguf_out_f32_b16 = gemm_nvfp4_gguf_out_f32_small_impl[16]
