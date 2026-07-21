# =============================================================================
# Plik: nvfp4_batch.mojo
# Opis: Weight-stationary GEMV NVFP4 dla małych batchy dekodu.
# Przykład: gemv_batch_nvfp4_f16_b4 liczy do czterech sekwencji jednym odczytem wag.
# =============================================================================

from std.gpu import block_idx, thread_idx
from std.gpu.primitives import warp
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from src.gemv2 import _f8e4m3s

comptime WARP = 32
comptime ROWS_PER_BLOCK = 8


def gemv_batch_nvfp4_f16_impl[batch_bucket: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    inv_global_scale: Float32,
):
    """Oblicza Y[B,N] = X[B,K] * W[N,K]^T dla B <= batch_bucket.

    Jeden warp obsługuje jeden wiersz W. Kody i skala wagi są odczytywane
    raz, a następnie wykorzystywane przez wszystkie wiersze aktywacji batcha.
    """
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

    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return

    groups = n_cols // 16
    packed_row = row * (n_cols // 2)
    scales_row = row * groups
    var acc = InlineArray[Float32, batch_bucket](fill=0.0)
    var g = lane
    while g < groups:
        s = _f8e4m3s(scales[scales_row + g]) * inv_global_scale
        qv = (packed + packed_row + g * 8).load[width=8, alignment=8]()
        var lov = SIMD[DType.float32, 8]()
        var hiv = SIMD[DType.float32, 8]()
        comptime for j in range(8):
            lov[j] = lut[Int(qv[j] & 0x0F)]
            hiv[j] = lut[Int(qv[j] >> 4)]

        comptime for token in range(batch_bucket):
            var source_token = token
            if source_token > n_tokens - 1:
                source_token = n_tokens - 1
            xv = (x + source_token * n_cols + g * 16).load[
                width=16, alignment=32
            ]().cast[DType.float32]()
            x_even, x_odd = xv.deinterleave()
            acc[token] += s * (
                (lov * x_even).reduce_add() + (hiv * x_odd).reduce_add()
            )
        g += WARP

    comptime for token in range(batch_bucket):
        total = warp.sum(acc[token])
        if lane == 0 and token < n_tokens:
            y[token * n_rows + row] = Float16(total)


comptime gemv_batch_nvfp4_f16_b4 = gemv_batch_nvfp4_f16_impl[4]
comptime gemv_batch_nvfp4_f16_b8 = gemv_batch_nvfp4_f16_impl[8]
comptime gemv_batch_nvfp4_f16_b16 = gemv_batch_nvfp4_f16_impl[16]


def gemv_batch_f16_out_f32_impl[batch_bucket: Int](
    y: UnsafePointer[Float32, MutAnyOrigin],
    w: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
):
    """Oblicza logity FP32 dla małego batcha z jednokrotnym odczytem wag."""
    tid = Int(thread_idx.x)
    lane = tid % WARP
    wid = tid // WARP
    row = Int(block_idx.x) * ROWS_PER_BLOCK + wid
    if row >= n_rows:
        return

    base = row * n_cols
    var acc = InlineArray[Float32, batch_bucket](fill=0.0)
    var i = lane * 8
    stride = WARP * 8
    while i + 8 <= n_cols:
        wv = (w + base + i).load[width=8, alignment=16]().cast[DType.float32]()
        comptime for token in range(batch_bucket):
            var source_token = token
            if source_token > n_tokens - 1:
                source_token = n_tokens - 1
            xv = (x + source_token * n_cols + i).load[
                width=8, alignment=16
            ]().cast[DType.float32]()
            acc[token] += (wv * xv).reduce_add()
        i += stride

    comptime for token in range(batch_bucket):
        total = warp.sum(acc[token])
        if lane == 0 and token < n_tokens:
            y[token * n_rows + row] = total


comptime gemv_batch_f16_out_f32_b4 = gemv_batch_f16_out_f32_impl[4]
comptime gemv_batch_f16_out_f32_b8 = gemv_batch_f16_out_f32_impl[8]
