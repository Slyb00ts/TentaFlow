# =============================================================================
# Plik: mtp.mojo
# Opis: Scalone przygotowanie wejścia warstwy MTP bez bufora pośredniego 2H.
# Przykład: mtp_prepare_f16(out, embedding_row, hidden, enorm, hnorm, eh_proj, h, eps)
# =============================================================================

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.memory import AddressSpace
from std.gpu.sync import barrier
from std.math import rsqrt
from std.memory import bitcast, stack_allocation
from src.nvfp4 import _e2m1, _ue4m3_portable

comptime MTP_MAX_HIDDEN = 5120
comptime ROWS_PER_BLOCK = 8
comptime LANES_PER_ROW = 32


def _block_sum_portable(value: Float32) -> Float32:
    """Redukuje cały CTA przez shared memory bez założenia szerokości wave."""
    tid = Int(thread_idx.x)
    values = stack_allocation[256, Float32, address_space=AddressSpace.SHARED]()
    values[tid] = value
    barrier()
    var stride = Int(block_dim.x) // 2
    while stride > 0:
        if tid < stride:
            values[tid] += values[tid + stride]
        barrier()
        stride //= 2
    total = values[0]
    barrier()
    return total


def gather_q8_0_row_f16(
    output: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    token: UnsafePointer[Int32, MutAnyOrigin],
    status: UnsafePointer[Int32, MutAnyOrigin],
    vocab_size: Int,
    hidden_size: Int,
):
    """Dekwantyzuje jeden wiersz tied embeddingu Q8_0 wskazany na GPU."""
    row = Int(token[0])
    blocks_per_row = hidden_size // 32
    var element = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    if row < 0 or row >= vocab_size:
        if element < hidden_size:
            output[element] = 0.0
        if element == 0:
            status[0] = 1
    elif element < hidden_size:
        block = element // 32
        offset = (row * blocks_per_row + block) * 34
        scale = Float32((weights + offset).bitcast[Float16]()[0])
        code = (weights + offset + 2).bitcast[Int8]()[element % 32]
        output[element] = Float16(scale * Float32(code))


def gather_f16_row_f16(
    output: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[Float16, MutAnyOrigin],
    token: UnsafePointer[Int32, MutAnyOrigin],
    status: UnsafePointer[Int32, MutAnyOrigin],
    vocab_size: Int,
    hidden_size: Int,
):
    """Kopiuje jeden wiersz dedykowanego embeddingu F16 wskazany na GPU."""
    row = Int(token[0])
    var element = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    if row < 0 or row >= vocab_size:
        if element < hidden_size:
            output[element] = 0.0
        if element == 0:
            status[0] = 1
    elif element < hidden_size:
        output[element] = weights[row * hidden_size + element]


def gather_nvfp4_gguf_row_f16(
    output: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    token: UnsafePointer[Int32, MutAnyOrigin],
    status: UnsafePointer[Int32, MutAnyOrigin],
    vocab_size: Int,
    hidden_size: Int,
    output_scale: Float32,
):
    """Dekwantyzuje jeden wiersz tied embeddingu GGUF NVFP4 wskazany na GPU."""
    row = Int(token[0])
    var element = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    if row < 0 or row >= vocab_size:
        if element < hidden_size:
            output[element] = 0.0
        if element == 0:
            status[0] = 1
    elif element < hidden_size:
        block = element // 64
        subblock = (element % 64) // 16
        within = element % 16
        block_base = (row * (hidden_size // 64) + block) * 36
        packed = weights[block_base + 4 + subblock * 8 + within % 8]
        code = packed & 0x0F if within < 8 else (packed >> 4) & 0x0F
        scale = _ue4m3_portable(weights[block_base + subblock]) * output_scale
        output[element] = Float16(_e2m1(code) * scale)


def mtp_prepare_f16(
    output: UnsafePointer[Float16, MutAnyOrigin],
    embedding_row: UnsafePointer[Float16, MutAnyOrigin],
    target_hidden: UnsafePointer[Float16, MutAnyOrigin],
    enorm: UnsafePointer[Float16, MutAnyOrigin],
    hnorm: UnsafePointer[Float16, MutAnyOrigin],
    eh_proj: UnsafePointer[UInt8, MutAnyOrigin],
    hidden_size: Int,
    eps: Float32,
):
    """Normalizuje staged embedding i target hidden, po czym wykonuje Q8_0.

    Logiczny wektor projekcji ma kolejność embedding, target hidden. Każdy CTA
    współdzieli jego kopię F16 między ośmioma grupami obliczającymi wiersze.
    """
    tid = Int(thread_idx.x)
    joined = stack_allocation[
        2 * MTP_MAX_HIDDEN,
        Float16,
        alignment=64,
        address_space=AddressSpace.SHARED,
    ]()

    var embed_sq: Float32 = 0.0
    var hidden_sq: Float32 = 0.0
    var i = tid
    while i < hidden_size:
        e = Float32(embedding_row[i])
        h = Float32(target_hidden[i])
        embed_sq += e * e
        hidden_sq += h * h
        i += Int(block_dim.x)

    embed_inv = rsqrt(_block_sum_portable(embed_sq) / Float32(hidden_size) + eps)
    hidden_inv = rsqrt(_block_sum_portable(hidden_sq) / Float32(hidden_size) + eps)

    i = tid
    while i < hidden_size:
        joined[i] = Float16(Float32(embedding_row[i]) * embed_inv * Float32(enorm[i]))
        joined[hidden_size + i] = Float16(
            Float32(target_hidden[i]) * hidden_inv * Float32(hnorm[i])
        )
        i += Int(block_dim.x)
    barrier()

    lane = tid % LANES_PER_ROW
    row_in_block = tid // LANES_PER_ROW
    row = Int(block_idx.x) * ROWS_PER_BLOCK + row_in_block
    partials = stack_allocation[
        ROWS_PER_BLOCK * LANES_PER_ROW,
        Float32,
        address_space=AddressSpace.SHARED,
    ]()
    var acc: Float32 = 0.0
    if row < hidden_size:
        n_cols = 2 * hidden_size
        blocks_per_row = n_cols // 32
        row_base = row * blocks_per_row * 34
        var block = lane
        while block < blocks_per_row:
            offset = row_base + block * 34
            scale = Float32((eh_proj + offset).bitcast[Float16]()[0])
            packed = (eh_proj + offset + 2).bitcast[UInt16]().load[width=16]()
            weights = bitcast[DType.int8, 32](packed).cast[DType.float32]()
            values = (joined + block * 32).load[width=32, alignment=64]().cast[DType.float32]()
            acc += scale * (weights * values).reduce_add()
            block += LANES_PER_ROW

    partials[row_in_block * LANES_PER_ROW + lane] = acc
    barrier()
    var stride = LANES_PER_ROW // 2
    while stride > 0:
        if lane < stride:
            partials[row_in_block * LANES_PER_ROW + lane] += partials[
                row_in_block * LANES_PER_ROW + lane + stride
            ]
        barrier()
        stride //= 2
    if lane == 0 and row < hidden_size:
        output[row] = Float16(partials[row_in_block * LANES_PER_ROW])


def mtp_norm_join_shifted_f16(
    output: UnsafePointer[Float16, MutAnyOrigin],
    embeddings: UnsafePointer[Float16, MutAnyOrigin],
    target_hidden: UnsafePointer[Float16, MutAnyOrigin],
    initial_hidden: UnsafePointer[Float16, MutAnyOrigin],
    enorm: UnsafePointer[Float16, MutAnyOrigin],
    hnorm: UnsafePointer[Float16, MutAnyOrigin],
    n_tokens: Int,
    hidden_size: Int,
    eps: Float32,
):
    """Tworzy znormalizowane [embedding, poprzedni target hidden] dla batcha.

    Pierwszy wiersz korzysta z carry sprzed chunka, a pozostale z poprzedniego
    wiersza targetu. Grid.x odpowiada tokenom, a blok ma 256 watkow.
    """
    token = Int(block_idx.x)
    if token >= n_tokens:
        return
    tid = Int(thread_idx.x)
    embedding_base = token * hidden_size
    hidden_base = (token - 1) * hidden_size

    var embed_sq: Float32 = 0.0
    var hidden_sq: Float32 = 0.0
    var i = tid
    while i < hidden_size:
        embedding = Float32(embeddings[embedding_base + i])
        hidden = Float32(
            initial_hidden[i] if token == 0 else target_hidden[hidden_base + i]
        )
        embed_sq += embedding * embedding
        hidden_sq += hidden * hidden
        i += Int(block_dim.x)

    embed_inv = rsqrt(_block_sum_portable(embed_sq) / Float32(hidden_size) + eps)
    hidden_inv = rsqrt(_block_sum_portable(hidden_sq) / Float32(hidden_size) + eps)
    output_base = token * 2 * hidden_size
    i = tid
    while i < hidden_size:
        embedding = Float32(embeddings[embedding_base + i])
        hidden = Float32(
            initial_hidden[i] if token == 0 else target_hidden[hidden_base + i]
        )
        output[output_base + i] = Float16(
            embedding * embed_inv * Float32(enorm[i])
        )
        output[output_base + hidden_size + i] = Float16(
            hidden * hidden_inv * Float32(hnorm[i])
        )
        i += Int(block_dim.x)


def mtp_project_joined_q8_f16(
    output: UnsafePointer[Float16, MutAnyOrigin],
    joined: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    hidden_size: Int,
    n_tokens: Int,
):
    """Projektuje batch z Q8_0 w kolejności redukcji zgodnej z mtp_prepare."""
    token = Int(block_idx.y)
    if token >= n_tokens:
        return
    tid = Int(thread_idx.x)
    lane = tid % LANES_PER_ROW
    row_in_block = tid // LANES_PER_ROW
    row = Int(block_idx.x) * ROWS_PER_BLOCK + row_in_block
    partials = stack_allocation[
        ROWS_PER_BLOCK * LANES_PER_ROW,
        Float32,
        address_space=AddressSpace.SHARED,
    ]()
    var acc: Float32 = 0.0
    if row < hidden_size:
        n_cols = 2 * hidden_size
        blocks_per_row = n_cols // 32
        row_base = row * blocks_per_row * 34
        var block = lane
        while block < blocks_per_row:
            offset = row_base + block * 34
            scale = Float32((weights + offset).bitcast[Float16]()[0])
            packed = (weights + offset + 2).bitcast[UInt16]().load[width=16]()
            codes = bitcast[DType.int8, 32](packed).cast[DType.float32]()
            values = (joined + token * n_cols + block * 32).load[
                width=32, alignment=64
            ]().cast[DType.float32]()
            acc += scale * (codes * values).reduce_add()
            block += LANES_PER_ROW
    partials[row_in_block * LANES_PER_ROW + lane] = acc
    barrier()
    var stride = LANES_PER_ROW // 2
    while stride > 0:
        if lane < stride:
            partials[row_in_block * LANES_PER_ROW + lane] += partials[
                row_in_block * LANES_PER_ROW + lane + stride
            ]
        barrier()
        stride //= 2
    if lane == 0 and row < hidden_size:
        output[token * hidden_size + row] = Float16(
            partials[row_in_block * LANES_PER_ROW]
        )


def mtp_stage_step(
    position_out: UnsafePointer[Int32, MutAnyOrigin],
    seq_len_out: UnsafePointer[Int32, MutAnyOrigin],
    page_table: UnsafePointer[Int32, MutAnyOrigin],
    position: Int,
    seq_len: Int,
    logical_page: Int,
    physical_page: Int,
):
    """Ustawia metadane kroku i opcjonalnie dopisuje nową stronę logiczną."""
    if Int(block_idx.x) == 0 and Int(thread_idx.x) == 0:
        position_out[0] = Int32(position)
        seq_len_out[0] = Int32(seq_len)
        if logical_page >= 0:
            page_table[logical_page] = Int32(physical_page)


def mtp_verify_decide(
    decision: UnsafePointer[Int32, MutAnyOrigin],
    predictions: UnsafePointer[Int32, MutAnyOrigin],
    input_ids: UnsafePointer[Int32, MutAnyOrigin],
    n_tokens: Int,
):
    """Wyznacza długość zaakceptowanego draftu i token korekty na GPU."""
    if Int(block_idx.x) == 0 and Int(thread_idx.x) == 0:
        var accepted = 0
        while accepted + 1 < n_tokens and predictions[accepted] == input_ids[accepted + 1]:
            accepted += 1
        decision[0] = Int32(accepted + 1)
        decision[1] = predictions[accepted]


def mtp_select_row_f16(
    output: UnsafePointer[Float16, MutAnyOrigin],
    rows: UnsafePointer[Float16, MutAnyOrigin],
    decision: UnsafePointer[Int32, MutAnyOrigin],
    row_size: Int,
):
    """Kopiuje wiersz F16 wskazany wynikiem akceptacji na GPU."""
    row = Int(decision[0]) - 1
    var element = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    if element < row_size:
        output[element] = rows[row * row_size + element]


def mtp_select_row_f32(
    output: UnsafePointer[Float32, MutAnyOrigin],
    rows: UnsafePointer[Float32, MutAnyOrigin],
    decision: UnsafePointer[Int32, MutAnyOrigin],
    row_size: Int,
):
    """Kopiuje wiersz F32 wskazany wynikiem akceptacji na GPU."""
    row = Int(decision[0]) - 1
    var element = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    if element < row_size:
        output[element] = rows[row * row_size + element]
