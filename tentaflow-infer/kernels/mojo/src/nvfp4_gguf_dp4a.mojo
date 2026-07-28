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
from src.nvfp4_gguf_batch import _ue4m3_branchless

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
    # Tablica kodow e2m1 SKALOWANYCH PRZEZ DWA, wspolna dla calego bloku.
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
    _row_dot_store(y, weights, xq, xd, lut, n_cols, row, lane, output_scale)


@always_inline
def _row_dot_store(
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    lut: UnsafePointer[
        Int8, MutUntrackedOrigin, address_space = AddressSpace.SHARED
    ],
    n_cols: Int,
    row: Int,
    lane: Int,
    output_scale: Float32,
):
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
            weight_scale = _ue4m3_branchless(weights[weight_base + group])
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

def gemv_nvfp4_gguf_q8_1_group4_f16(
    y0: UnsafePointer[Float16, MutAnyOrigin],
    w0: UnsafePointer[UInt8, MutAnyOrigin],
    rows0: Int,
    scale0: Float32,
    y1: UnsafePointer[Float16, MutAnyOrigin],
    w1: UnsafePointer[UInt8, MutAnyOrigin],
    rows1: Int,
    scale1: Float32,
    y2: UnsafePointer[Float16, MutAnyOrigin],
    w2: UnsafePointer[UInt8, MutAnyOrigin],
    rows2: Int,
    scale2: Float32,
    y3: UnsafePointer[Float16, MutAnyOrigin],
    w3: UnsafePointer[UInt8, MutAnyOrigin],
    rows3: Int,
    scale3: Float32,
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
):
    """Do czterech projekcji NVFP4 na WSPOLNEJ aktywacji, jednym uruchomieniem.

    DeltaNet liczy cztery projekcje wejsciowe z tego samego znormalizowanego `x`,
    a FFN dwie — dotad kazda szla osobnym uruchomieniem. Poza samym narzutem
    uruchomienia bolal rozmiar siatki: waska projekcja mierzyla 425 GB/s wobec
    960 GB/s szerokiej, bo nie miala czym wypelnic karty. Zlozone razem daja
    siatke o sumie wierszy i pracuja blizej szerokiego przypadku.

    Nieuzyte sloty przekazuje sie z `rows = 0`. Siatka to suma `ceil(rows_i / 8)`.
    """
    tid = Int(thread_idx.x)
    lane = tid % WARP
    warp_id = tid // WARP

    lut = stack_allocation[16, Int8, address_space=AddressSpace.SHARED]()
    comptime values = SIMD[DType.int8, 16](
        0, 1, 2, 3, 4, 6, 8, 12,
        0, -1, -2, -3, -4, -6, -8, -12,
    )
    if tid < 16:
        lut[tid] = values[tid]
    barrier()

    # Blok wybiera swoja projekcje po skumulowanej liczbie blokow wierszy.
    var tile = Int(block_idx.x)
    blocks0 = (rows0 + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK
    blocks1 = (rows1 + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK
    blocks2 = (rows2 + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK

    if tile < blocks0:
        row0 = tile * ROWS_PER_BLOCK + warp_id
        if row0 < rows0:
            _row_dot_store(y0, w0, xq, xd, lut, n_cols, row0, lane, scale0)
        return
    tile -= blocks0
    if tile < blocks1:
        row1 = tile * ROWS_PER_BLOCK + warp_id
        if row1 < rows1:
            _row_dot_store(y1, w1, xq, xd, lut, n_cols, row1, lane, scale1)
        return
    tile -= blocks1
    if tile < blocks2:
        row2 = tile * ROWS_PER_BLOCK + warp_id
        if row2 < rows2:
            _row_dot_store(y2, w2, xq, xd, lut, n_cols, row2, lane, scale2)
        return
    tile -= blocks2
    row3 = tile * ROWS_PER_BLOCK + warp_id
    if row3 < rows3:
        _row_dot_store(y3, w3, xq, xd, lut, n_cols, row3, lane, scale3)

def gemv_nvfp4_gguf_q8_1_batch_impl[TOKENS: Int](
    y: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    xq: UnsafePointer[Int8, MutAnyOrigin],
    xd: UnsafePointer[Float32, MutAnyOrigin],
    n_cols: Int,
    n_rows: Int,
    n_tokens: Int,
    output_scale: Float32,
):
    """Do TOKENS tokenow naraz, JEDNA FALA NA WIERSZ, iloczyn calkowitoliczbowy.

    Weryfikacja MTP liczy 2-4 tokeny na przebieg. Rodzina `gemm_nvfp4_gguf_f16_b*`
    ma wlasciwa strukture (fala na wiersz), ale idzie sciezka f16: dekwantyzacja
    do float i mnozenie zmiennoprzecinkowe. Przy T=4 mierzyla 152 us wobec 63 us
    tego samego ksztaltu w GEMV int8 — 2,4x drozej za 4x tokenow, mimo ze czyta
    TE SAME wagi. Tutaj wagi sa dekodowane RAZ na fale i uzyte dla wszystkich
    tokenow, a iloczyn idzie przez `dp4a`.

    Aktywacja jest skwantowana przez `quantize_act_q8_1`: kody `xq` w ukladzie
    [T, K], skale `xd` blok-major [K/32, T].
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
    var acc = InlineArray[Float32, TOKENS](fill=0.0)
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
            # Bezgaleziowe dekodowanie skali i polowka wyniesiona PRZED petle
            # tokenow: to jedyna praca, ktora przy T>1 mnozy sie przez liczbe
            # tokenow, choc zalezy tylko od grupy.
            weight_scale = _ue4m3_branchless(weights[weight_base + group]) * 0.5
            comptime for token in range(TOKENS):
                base = token * n_cols + column
                activation_low = (xq + base).bitcast[Int32]().load[
                    width=2, alignment=4
                ]()
                activation_high = (xq + base + 8).bitcast[Int32]().load[
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
                acc[token] += (
                    Float32(integer_dot)
                    * xd[(column // 32) * n_tokens + token]
                    * weight_scale
                )
        block += WARP

    comptime for token in range(TOKENS):
        total = warp.sum(acc[token])
        if lane == 0 and token < n_tokens:
            y[token * n_rows + row] = Float16(total * output_scale)


comptime gemv_nvfp4_gguf_q8_1_b2_f16 = gemv_nvfp4_gguf_q8_1_batch_impl[2]
comptime gemv_nvfp4_gguf_q8_1_b4_f16 = gemv_nvfp4_gguf_q8_1_batch_impl[4]
comptime gemv_nvfp4_gguf_q8_1_b8_f16 = gemv_nvfp4_gguf_q8_1_batch_impl[8]

