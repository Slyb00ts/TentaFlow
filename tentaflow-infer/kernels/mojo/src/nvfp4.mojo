# ===== File: nvfp4.mojo — fused NVFP4 (compressed-tensors) dequant GEMV =====
# Software path for GPUs without FP4 tensor cores (SPEC §4.2): dequantize
# e2m1 codes + FP8-E4M3 block scales inside the dot product. Layout matches
# forge-formats::nvfp4 (low nibble = even element, scales premultiplied by the
# global scale, so the host passes 1/global_scale).

from std.gpu import block_dim, block_idx, thread_idx
from src.reduce import block_reduce_sum

comptime GROUP = 16  # NVFP4 block size (elements per FP8 scale)


def _e2m1(nibble: UInt8) -> Float32:
    # Magnitude codebook 0,.5,1,1.5,2,3,4,6; bit 3 is the sign.
    m = Int(nibble & 0x07)
    var mag: Float32 = 0.0
    if m == 1:
        mag = 0.5
    elif m == 2:
        mag = 1.0
    elif m == 3:
        mag = 1.5
    elif m == 4:
        mag = 2.0
    elif m == 5:
        mag = 3.0
    elif m == 6:
        mag = 4.0
    elif m == 7:
        mag = 6.0
    if (nibble & 0x08) != 0:
        return -mag
    return mag


def _f8e4m3(b: UInt8) -> Float32:
    # FP8 E4M3FN: no infinities, 0x7F/0xFF are NaN (weights never contain
    # them, so NaN passthrough via the normal path is acceptable here).
    var sign: Float32 = 1.0
    if (b & 0x80) != 0:
        sign = -1.0
    e = Int((b >> 3) & 0x0F)
    man = Float32(Int(b & 0x07))
    if e == 0:
        return sign * man * (1.0 / 512.0)
    # 2^(e-7) via exponent bit assembly avoids pow() in the inner loop.
    bits = UInt32(e - 7 + 127) << 23
    scale = UnsafePointer(to=bits).bitcast[Float32]()[0]
    return sign * (1.0 + man / 8.0) * scale


def gemv_nvfp4_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    inv_global_scale: Float32,
):
    """y[row] = dot(dequant_nvfp4(row), x). Grid.x = rows.

    packed: [rows, n_cols/2] bytes; scales: [rows, n_cols/16] FP8-E4M3.
    """
    row = Int(block_idx.x)
    groups = n_cols // GROUP
    packed_row = row * (n_cols // 2)
    scales_row = row * groups

    var acc: Float32 = 0.0
    var g = Int(thread_idx.x)
    while g < groups:
        s = _f8e4m3(scales[scales_row + g]) * inv_global_scale
        base_p = packed_row + g * (GROUP // 2)
        base_x = g * GROUP
        var dot: Float32 = 0.0
        for k in range(GROUP // 2):
            byte = packed[base_p + k]
            dot += _e2m1(byte & 0x0F) * Float32(x[base_x + 2 * k])
            dot += _e2m1((byte >> 4) & 0x0F) * Float32(x[base_x + 2 * k + 1])
        acc += s * dot
        g += Int(block_dim.x)

    total = block_reduce_sum(acc)
    if Int(thread_idx.x) == 0:
        y[row] = Float16(total)
