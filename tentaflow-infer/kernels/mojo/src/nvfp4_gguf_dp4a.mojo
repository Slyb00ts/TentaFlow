# =============================================================================
# Plik: nvfp4_gguf_dp4a.mojo
# Opis: Weight-stationary GEMV surowego GGUF NVFP4 z aktywacją Q8_1 i
#       całkowitoliczbowym iloczynem dp4a.
# Przykład: gemv_nvfp4_gguf_q8_1_f16 liczy jeden wektor dekodu bez repacku wag.
# =============================================================================

from std.gpu import block_idx, thread_idx
from std.gpu.memory import AddressSpace
from std.gpu.primitives import warp
from std.gpu.sync import barrier
from std.memory import bitcast, stack_allocation
from src.decode_dp4a import _dp4a
from src.nvfp4_gguf_batch import _ue4m3_value

comptime WARP = 32
comptime ROWS_PER_BLOCK = 8


def gemv_nvfp4_gguf_q8_1_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    output_scale: Float32,
):
    """Liczy y[row] = dot(W[row], dequant_q8_1(x)) bez zmiany layoutu wag.

    Wagi zachowują bloki GGUF `[4 skale UE4M3 | 32 B kodów E2M1]` na 64
    kolumny. Kody E2M1 są skalowane przez dwa do dokładnych int8
    `{0,1,2,3,4,6,8,12}`, więc końcowy iloczyn dostaje współczynnik 0.5.
    `xd` zawiera jedną skalę Q8_1 na 32 kolumny.
    """
    tid = Int(thread_idx.x)
    lane = tid % WARP
    warp_id = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + warp_id

    lut = stack_allocation[16, Int8, address_space=AddressSpace.SHARED]()
    comptime values = SIMD[DType.int8, 16](
        0, 1, 2, 3, 4, 6, 8, 12,
        0, -1, -2, -3, -4, -6, -8, -12,
    )
    if tid < 16:
        lut[tid] = values[tid]
    barrier()
    if row >= n_rows:
        return

    blocks_per_row = n_cols // 64
    row_base = row * blocks_per_row * 36
    var acc: Float32 = 0.0
    var block = lane
    while block < blocks_per_row:
        weight_base = row_base + block * 36
        comptime for group in range(4):
            codes = (weights + weight_base + 4 + group * 8).load[
                width=8, alignment=4
            ]()
            var low = SIMD[DType.int8, 8]()
            var high = SIMD[DType.int8, 8]()
            comptime for element in range(8):
                low[element] = lut[Int(codes[element] & 0x0F)]
                high[element] = lut[Int(codes[element] >> 4)]
            packed_low = bitcast[DType.int32, 2](low)
            packed_high = bitcast[DType.int32, 2](high)
            column = block * 64 + group * 16
            activation_low = (xq + column).bitcast[Int32]().load[
                width=2, alignment=4
            ]()
            activation_high = (xq + column + 8).bitcast[Int32]().load[
                width=2, alignment=4
            ]()
            var integer_dot: Int32 = 0
            comptime for part in range(2):
                integer_dot = _dp4a(
                    packed_low[part], activation_low[part], integer_dot
                )
                integer_dot = _dp4a(
                    packed_high[part], activation_high[part], integer_dot
                )
            activation_scale = xd[column // 32]
            weight_scale = _ue4m3_value(weights[weight_base + group])
            acc += (
                Float32(integer_dot)
                * activation_scale
                * weight_scale
                * 0.5
            )
        block += WARP

    total = warp.sum(acc)
    if lane == 0:
        y[row] = Float16(total * output_scale)
