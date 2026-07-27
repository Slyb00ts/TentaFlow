# ===== File: nvfp4.mojo — fused NVFP4 (compressed-tensors) dequant GEMV =====
# Software path for GPUs without FP4 tensor cores (SPEC §4.2): dequantize
# e2m1 codes + FP8-E4M3 block scales inside the dot product. Layout matches
# forge-formats::nvfp4 (low nibble = even element, scales premultiplied by the
# global scale, so the host passes 1/global_scale).

from std.gpu import block_dim, block_idx, thread_idx, WARP_SIZE
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import bitcast, stack_allocation
from src.reduce import block_reduce_sum
from src.gemv2 import _e2m1x8, _e2m1x8_f16
from src.nvfp4_gguf_batch import _ue4m3_branchless
from src.kv_fp8 import _e4m3x2_to_f16x2
from std.gpu.primitives import warp

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
    # FP8 E4M3FN -> f32 via the exact sm_89 hardware pair cvt (single
    # instruction vs ~15-op generic float8 emulation). 0x7F/0xFF widen to NaN;
    # real NVFP4 block scales never contain them.
    return Float32(_e4m3x2_to_f16x2(b, 0)[0])


def _ue4m3_portable(b: UInt8) -> Float32:
    """Dekoduje dodatnią skalę UE4M3 bez instrukcji zależnych od producenta GPU."""
    if b == 0 or b == 0x7F:
        return 0.0
    exponent = Int((b >> 3) & 0x0F)
    mantissa = Int(b & 0x07)
    if exponent == 0:
        return Float32(mantissa) * (1.0 / 512.0)
    bits = UInt32((exponent + 120) << 23 | mantissa << 20)
    return bitcast[DType.float32, 1](SIMD[DType.uint32, 1](bits))[0]


def pack_nvfp4_fp8(
    output: UnsafePointer[Int8, MutAnyOrigin],
    output_scales: UnsafePointer[Float32, MutAnyOrigin],
    packed: UnsafePointer[UInt8, MutAnyOrigin],
    scales: UnsafePointer[UInt8, MutAnyOrigin],
    n_cols: Int,
    source_row_offset: Int,
    n_rows: Int,
    inv_global_scale: Float32,
):
    """Przepakowuje wybrany zakres wierszy NVFP4 do E4M3 z jedną skalą na wiersz."""
    row = Int(block_idx.x)
    if row >= n_rows:
        return
    tid = Int(thread_idx.x)
    nthreads = Int(block_dim.x)
    source_row = source_row_offset + row
    groups = n_cols // GROUP
    packed_row = source_row * (n_cols // 2)
    scales_row = source_row * groups

    var local: Float32 = 0.0
    var c = tid
    while c < n_cols:
        byte = packed[packed_row + c // 2]
        nibble = byte & 0x0F if c % 2 == 0 else (byte >> 4) & 0x0F
        value = _e2m1(nibble) * _f8e4m3(scales[scales_row + c // GROUP]) * inv_global_scale
        magnitude = abs(value)
        if magnitude > local:
            local = magnitude
        c += nthreads

    reduction = stack_allocation[256, Float32, address_space = AddressSpace.SHARED]()
    reduction[tid] = local
    barrier()
    var stride = nthreads // 2
    while stride > 0:
        if tid < stride and reduction[tid + stride] > reduction[tid]:
            reduction[tid] = reduction[tid + stride]
        barrier()
        stride //= 2

    amax = reduction[0]
    if tid == 0:
        output_scales[row] = amax / 448.0 if amax != 0.0 else 0.0
    inv = 448.0 / amax if amax != 0.0 else 0.0
    var q = tid
    while q < n_cols:
        byte = packed[packed_row + q // 2]
        nibble = byte & 0x0F if q % 2 == 0 else (byte >> 4) & 0x0F
        value = _e2m1(nibble) * _f8e4m3(scales[scales_row + q // GROUP]) * inv_global_scale
        encoded = Scalar[DType.float8_e4m3fn](value * inv)
        output[row * n_cols + q] = bitcast[DType.int8, 1](encoded)
        q += nthreads


def pack_f16_fp8(
    output: UnsafePointer[Int8, MutAnyOrigin],
    output_scales: UnsafePointer[Float32, MutAnyOrigin],
    source: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
):
    """Przepakowuje macierz F16 do E4M3 z jedną skalą na wiersz."""
    row = Int(block_idx.x)
    if row >= n_rows:
        return
    tid = Int(thread_idx.x)
    nthreads = Int(block_dim.x)
    base = row * n_cols

    var local: Float32 = 0.0
    var c = tid
    while c < n_cols:
        magnitude = abs(Float32(source[base + c]))
        if magnitude > local:
            local = magnitude
        c += nthreads

    reduction = stack_allocation[256, Float32, address_space = AddressSpace.SHARED]()
    reduction[tid] = local
    barrier()
    var stride = nthreads // 2
    while stride > 0:
        if tid < stride and reduction[tid + stride] > reduction[tid]:
            reduction[tid] = reduction[tid + stride]
        barrier()
        stride //= 2

    amax = reduction[0]
    if tid == 0:
        output_scales[row] = amax / 448.0 if amax != 0.0 else 0.0
    inv = 448.0 / amax if amax != 0.0 else 0.0
    var q = tid
    while q < n_cols:
        encoded = Scalar[DType.float8_e4m3fn](Float32(source[base + q]) * inv)
        output[base + q] = bitcast[DType.int8, 1](encoded)
        q += nthreads


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


def gemv_nvfp4_gguf_f16(
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    output_scale: Float32,
):
    """Liczy y[row] = dot(W[row], x) bezpośrednio z bloków GGUF NVFP4.

    Każdy blok przechowuje cztery skale UE4M3, a po nich cztery grupy po
    osiem bajtów E2M1. Dolna połówka bajtu opisuje element j, a górna j+8.
    """
    row = Int(block_idx.x)
    groups = n_cols // GROUP
    row_base = row * (n_cols // 64) * 36

    var acc: Float32 = 0.0
    var group = Int(thread_idx.x)
    while group < groups:
        block = group // 4
        subblock = group % 4
        block_base = row_base + block * 36
        packed_base = block_base + 4 + subblock * 8
        x_base = group * GROUP
        scale = _ue4m3_portable(weights[block_base + subblock])
        var dot: Float32 = 0.0
        for j in range(8):
            code = weights[packed_base + j]
            dot += _e2m1(code & 0x0F) * Float32(x[x_base + j])
            dot += _e2m1((code >> 4) & 0x0F) * Float32(x[x_base + j + 8])
        acc += scale * dot
        group += Int(block_dim.x)

    total = block_reduce_sum(acc)
    if Int(thread_idx.x) == 0:
        y[row] = Float16(total * output_scale)


comptime GEMV_WAVE_ROWS = 8  # wierszy na blok: osiem fal po 32 linie


def gemv_nvfp4_gguf_f16_wave(
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    output_scale: Float32,
):
    """`y[row] = dot(W[row], x)` z bloków GGUF NVFP4, JEDNA FALA NA WIERSZ.

    Wariant blokowy (`gemv_nvfp4_gguf_f16`) dawał jeden workgroup 256 wątków na
    wiersz, a wiersz to zaledwie kilka kilobajtów — na każdy wątek wypadało
    kilkanaście bajtów, po czym redukcja przez CAŁY blok kosztowała tyle, co
    samo liczenie. Do tego wagi i aktywacje czytał BAJT PO BAJCIE.

    Tutaj wiersz obsługuje jedna fala: redukcja jest wewnątrz fali (bez LDS i
    bez bariery), a odczyty są ośmioelementowe. Fala czyta w jednej iteracji
    osiem sąsiadujących bloków, czyli 288 ciągłych bajtów.
    """
    var lane = Int(thread_idx.x) % WARP_SIZE
    var wave = Int(thread_idx.x) // WARP_SIZE
    var row = Int(block_idx.x) * GEMV_WAVE_ROWS + wave
    if row >= n_rows:
        return

    # Para podblokow lezy w bloku obok siebie (bajty 4..19 i 20..35), wiec
    # jednym 16-bajtowym odczytem bierzemy dwa naraz. Dwa akumulatory trzymaja
    # oba lancuchy niezalezne — VALU RDNA ma kilkutaktowa latencje wyniku.
    var pairs = n_cols // (2 * GROUP)
    var row_base = row * (n_cols // 64) * 36
    var acc0: Float32 = 0.0
    var acc1: Float32 = 0.0
    var pair = lane
    while pair < pairs:
        block_base = row_base + (pair // 2) * 36
        half = (pair % 2) * 2
        codes = (weights + block_base + 4 + half * 8).load[width=16, alignment=1]()
        column = pair * 2 * GROUP
        lo = codes.slice[8]()
        hi = codes.slice[8, offset=8]()
        x0 = (x + column).load[width=8, alignment=2]()
        x1 = (x + column + 8).load[width=8, alignment=2]()
        x2 = (x + column + 16).load[width=8, alignment=2]()
        x3 = (x + column + 24).load[width=8, alignment=2]()
        acc0 += _ue4m3_branchless(weights[block_base + half]) * (
            (_e2m1x8_f16(lo & 0x0F) * x0).cast[DType.float32]().reduce_add()
            + (_e2m1x8_f16(lo >> 4) * x1).cast[DType.float32]().reduce_add()
        )
        acc1 += _ue4m3_branchless(weights[block_base + half + 1]) * (
            (_e2m1x8_f16(hi & 0x0F) * x2).cast[DType.float32]().reduce_add()
            + (_e2m1x8_f16(hi >> 4) * x3).cast[DType.float32]().reduce_add()
        )
        pair += WARP_SIZE

    total = warp.sum(acc0 + acc1)
    if lane == 0:
        y[row] = Float16(total * output_scale)


def gemv_nvfp4_gguf_out_f32(
    y: UnsafePointer[Float32, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
    output_scale: Float32,
):
    """Liczy logity F32 bezposrednio z blokow GGUF NVFP4."""
    row = Int(block_idx.x)
    groups = n_cols // GROUP
    row_base = row * (n_cols // 64) * 36

    var acc: Float32 = 0.0
    var group = Int(thread_idx.x)
    while group < groups:
        block = group // 4
        subblock = group % 4
        block_base = row_base + block * 36
        packed_base = block_base + 4 + subblock * 8
        x_base = group * GROUP
        scale = _ue4m3_portable(weights[block_base + subblock])
        var dot: Float32 = 0.0
        for j in range(8):
            code = weights[packed_base + j]
            dot += _e2m1(code & 0x0F) * Float32(x[x_base + j])
            dot += _e2m1((code >> 4) & 0x0F) * Float32(x[x_base + j + 8])
        acc += scale * dot
        group += Int(block_dim.x)

    total = block_reduce_sum(acc)
    if Int(thread_idx.x) == 0:
        y[row] = total * output_scale


def _fp32_to_ue4m3(value: Float32) -> UInt8:
    """Koduje dodatnia skale tak samo jak referencyjny kwantyzator GGUF."""
    if not (value > 0.0):
        return 0
    var x = value
    if x > 448.0:
        x = 448.0
    bits = bitcast[DType.uint32, 1](SIMD[DType.float32, 1](x))[0]
    fp32_exp = Int((bits >> 23) & 0xFF) - 127
    fp32_man = Int((bits >> 20) & 0x07)
    var ue4m3_exp = fp32_exp + 7
    if ue4m3_exp <= 0:
        var mantissa = Int(x * 512.0 + 0.5)
        if mantissa > 7:
            mantissa = 7
        if mantissa < 1:
            return 0
        return UInt8(mantissa)
    if ue4m3_exp >= 15:
        return 0x7E
    round_bit = Int((bits >> 19) & 1)
    var ue4m3_man = fp32_man + round_bit
    if ue4m3_man > 7:
        ue4m3_man = 0
        ue4m3_exp += 1
        if ue4m3_exp >= 15:
            return 0x7E
    return UInt8((ue4m3_exp << 3) | ue4m3_man)


def _best_e2m1(value: Float32, scale: Float32) -> UInt8:
    comptime values = SIMD[DType.float32, 16](
        0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0,
        -0.0, -0.5, -1.0, -1.5, -2.0, -3.0, -4.0, -6.0,
    )
    var best = 0
    var best_error = abs(value)
    for code in range(1, 16):
        error = abs(values[code] * scale - value)
        if error < best_error:
            best = code
            best_error = error
    return UInt8(best)


def _pack_q8_0_nvfp4_gguf_warp32(
    output: UnsafePointer[UInt8, MutAnyOrigin],
    source: UnsafePointer[UInt8, MutAnyOrigin],
    n_blocks: Int,
):
    """Przepakowuje Q8_0 do blokow GGUF NVFP4 bez bufora posredniego."""
    tid = Int(thread_idx.x)
    warp_id = tid // 32
    lane = tid % 32
    nv_block = Int(block_idx.x) * 2 + warp_id // 4
    subblock = warp_id % 4
    if nv_block >= n_blocks:
        return

    q8_block = nv_block * 2 + subblock // 2
    q8_base = q8_block * 34
    scale_q8 = Float32((source + q8_base).bitcast[Float16]()[0])
    var value: Float32 = 0.0
    if lane < 16:
        q_index = (subblock % 2) * 16 + lane
        value = Float32((source + q8_base + 2).bitcast[Int8]()[q_index]) * scale_q8

    amax = warp.max(abs(value))
    scale_code = _fp32_to_ue4m3(amax * (1.0 / 6.0))
    scale = _ue4m3_portable(scale_code)
    output_base = nv_block * 36
    if lane == 0:
        output[output_base + subblock] = scale_code
    if lane < 8:
        low = _best_e2m1(value, scale)
        high_value = Float32(
            (source + q8_base + 2).bitcast[Int8]()[
                (subblock % 2) * 16 + lane + 8
            ]
        ) * scale_q8
        high = _best_e2m1(high_value, scale)
        output[output_base + 4 + subblock * 8 + lane] = low | (high << 4)


def _pack_q8_0_nvfp4_gguf_portable(
    output: UnsafePointer[UInt8, MutAnyOrigin],
    source: UnsafePointer[UInt8, MutAnyOrigin],
    n_blocks: Int,
):
    """Redukuje cztery logiczne grupy po 16 niezaleznie od szerokosci warpa."""
    nv_block = Int(block_idx.x)
    if nv_block >= n_blocks:
        return
    tid = Int(thread_idx.x)
    subblock = tid // 16
    lane = tid % 16
    q8_block = nv_block * 2 + subblock // 2
    q8_base = q8_block * 34
    scale_q8 = Float32((source + q8_base).bitcast[Float16]()[0])
    q_index = (subblock % 2) * 16 + lane
    value = Float32((source + q8_base + 2).bitcast[Int8]()[q_index]) * scale_q8
    values = stack_allocation[64, Float32, address_space=AddressSpace.SHARED]()
    maxima = stack_allocation[64, Float32, address_space=AddressSpace.SHARED]()
    values[tid] = value
    maxima[tid] = abs(value)
    barrier()

    var stride = 8
    while stride > 0:
        if lane < stride and maxima[tid + stride] > maxima[tid]:
            maxima[tid] = maxima[tid + stride]
        barrier()
        stride //= 2

    scale_code = _fp32_to_ue4m3(maxima[subblock * 16] * (1.0 / 6.0))
    scale = _ue4m3_portable(scale_code)
    output_base = nv_block * 36
    if lane == 0:
        output[output_base + subblock] = scale_code
    if lane < 8:
        low = _best_e2m1(values[subblock * 16 + lane], scale)
        high = _best_e2m1(values[subblock * 16 + lane + 8], scale)
        output[output_base + 4 + subblock * 8 + lane] = low | (high << 4)


def pack_q8_0_nvfp4_gguf(
    output: UnsafePointer[UInt8, MutAnyOrigin],
    source: UnsafePointer[UInt8, MutAnyOrigin],
    n_blocks: Int,
):
    """Wybiera szybka redukcje warp32 albo przenosna redukcje grup logicznych."""
    comptime if WARP_SIZE == 32:
        _pack_q8_0_nvfp4_gguf_warp32(output, source, n_blocks)
    else:
        _pack_q8_0_nvfp4_gguf_portable(output, source, n_blocks)
