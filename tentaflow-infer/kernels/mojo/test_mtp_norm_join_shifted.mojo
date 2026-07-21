# =============================================================================
# Plik: test_mtp_norm_join_shifted.mojo
# Opis: Sprawdza batchowe laczenie embeddingu z przesunietym target hidden
#       wzgledem CPU i dotychczasowego mtp_prepare_f16.
# Przyklad: pixi run mojo test_mtp_norm_join_shifted.mojo
# =============================================================================

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.host import DeviceContext
from std.gpu.memory import AddressSpace
from std.gpu.sync import barrier
from std.math import sqrt
from std.memory import bitcast, stack_allocation
from src.mtp import mtp_prepare_f16, mtp_norm_join_shifted_f16


comptime HIDDEN = 64
comptime BLOCK = 256
comptime ROWS_PER_BLOCK = 8
comptime LANES_PER_ROW = 32
comptime EPS: Float32 = 0.00001


def _project_joined_q8_f16(
    output: UnsafePointer[Float16, MutAnyOrigin],
    joined: UnsafePointer[Float16, MutAnyOrigin],
    weights: UnsafePointer[UInt8, MutAnyOrigin],
    hidden_size: Int,
    n_tokens: Int,
):
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


def _fill_vectors(
    embeddings: UnsafePointer[Float16, MutUntrackedOrigin],
    target: UnsafePointer[Float16, MutUntrackedOrigin],
    initial: UnsafePointer[Float16, MutUntrackedOrigin],
    enorm: UnsafePointer[Float16, MutUntrackedOrigin],
    hnorm: UnsafePointer[Float16, MutUntrackedOrigin],
    steps: Int,
):
    for i in range(steps * HIDDEN):
        embeddings[i] = Float16(Float32((i * 7 + 3) % 31 - 15) * 0.031)
        target[i] = Float16(Float32((i * 11 + 5) % 37 - 18) * 0.027)
    for i in range(HIDDEN):
        initial[i] = Float16(Float32((i * 13 + 9) % 29 - 14) * 0.023)
        enorm[i] = Float16(0.75 + Float32(i % 9) * 0.035)
        hnorm[i] = Float16(0.82 + Float32(i % 7) * 0.041)


def _fill_q8(weights: UnsafePointer[UInt8, MutUntrackedOrigin]):
    blocks_per_row = (2 * HIDDEN) // 32
    for row in range(HIDDEN):
        for block in range(blocks_per_row):
            offset = (row * blocks_per_row + block) * 34
            (weights + offset).bitcast[Float16]()[0] = Float16(
                0.015625 + Float32((row + block) % 5) * 0.00390625
            )
            codes = (weights + offset + 2).bitcast[Int8]()
            for i in range(32):
                codes[i] = Int8((row * 5 + block * 3 + i * 7) % 23 - 11)


def _case(ctx: DeviceContext, steps: Int) raises:
    var embeddings = ctx.enqueue_create_buffer[DType.float16](steps * HIDDEN)
    var target = ctx.enqueue_create_buffer[DType.float16](steps * HIDDEN)
    var initial = ctx.enqueue_create_buffer[DType.float16](HIDDEN)
    var enorm = ctx.enqueue_create_buffer[DType.float16](HIDDEN)
    var hnorm = ctx.enqueue_create_buffer[DType.float16](HIDDEN)
    var joined = ctx.enqueue_create_buffer[DType.float16](steps * 2 * HIDDEN)
    var weights = ctx.enqueue_create_buffer[DType.uint8](HIDDEN * ((2 * HIDDEN) // 32) * 34)
    var serial = ctx.enqueue_create_buffer[DType.float16](steps * HIDDEN)
    var projected = ctx.enqueue_create_buffer[DType.float16](steps * HIDDEN)

    with embeddings.map_to_host() as embedding_h, target.map_to_host() as target_h, initial.map_to_host() as initial_h, enorm.map_to_host() as enorm_h, hnorm.map_to_host() as hnorm_h:
        _fill_vectors(embedding_h.unsafe_ptr(), target_h.unsafe_ptr(), initial_h.unsafe_ptr(), enorm_h.unsafe_ptr(), hnorm_h.unsafe_ptr(), steps)
    with weights.map_to_host() as weights_h:
        _fill_q8(weights_h.unsafe_ptr())

    ctx.enqueue_function[mtp_norm_join_shifted_f16](
        joined.unsafe_ptr(), embeddings.unsafe_ptr(), target.unsafe_ptr(),
        initial.unsafe_ptr(), enorm.unsafe_ptr(), hnorm.unsafe_ptr(),
        steps, HIDDEN, EPS, grid_dim=steps, block_dim=BLOCK,
    )
    for token in range(steps):
        hidden = initial.unsafe_ptr() if token == 0 else target.unsafe_ptr() + (token - 1) * HIDDEN
        ctx.enqueue_function[mtp_prepare_f16](
            serial.unsafe_ptr() + token * HIDDEN,
            embeddings.unsafe_ptr() + token * HIDDEN,
            hidden, enorm.unsafe_ptr(), hnorm.unsafe_ptr(), weights.unsafe_ptr(),
            HIDDEN, EPS, grid_dim=(HIDDEN + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK,
            block_dim=BLOCK,
        )
    ctx.enqueue_function[_project_joined_q8_f16](
        projected.unsafe_ptr(), joined.unsafe_ptr(), weights.unsafe_ptr(), HIDDEN, steps,
        grid_dim=((HIDDEN + ROWS_PER_BLOCK - 1) // ROWS_PER_BLOCK, steps),
        block_dim=BLOCK,
    )
    ctx.synchronize()

    var max_cpu_error: Float32 = 0.0
    with embeddings.map_to_host() as embedding_h, target.map_to_host() as target_h, initial.map_to_host() as initial_h, enorm.map_to_host() as enorm_h, hnorm.map_to_host() as hnorm_h, joined.map_to_host() as joined_h, serial.map_to_host() as serial_h, projected.map_to_host() as projected_h:
        for token in range(steps):
            var embedding_sum: Float32 = 0.0
            var hidden_sum: Float32 = 0.0
            for i in range(HIDDEN):
                embedding = Float32(embedding_h[token * HIDDEN + i])
                var hidden: Float32
                if token == 0:
                    hidden = Float32(initial_h[i])
                else:
                    hidden = Float32(target_h[(token - 1) * HIDDEN + i])
                embedding_sum += embedding * embedding
                hidden_sum += hidden * hidden
            embedding_inv = 1.0 / sqrt(embedding_sum / Float32(HIDDEN) + EPS)
            hidden_inv = 1.0 / sqrt(hidden_sum / Float32(HIDDEN) + EPS)
            for i in range(HIDDEN):
                embedding_expected = Float16(
                    Float32(embedding_h[token * HIDDEN + i]) * embedding_inv * Float32(enorm_h[i])
                )
                var hidden_value: Float32
                if token == 0:
                    hidden_value = Float32(initial_h[i])
                else:
                    hidden_value = Float32(target_h[(token - 1) * HIDDEN + i])
                hidden_expected = Float16(hidden_value * hidden_inv * Float32(hnorm_h[i]))
                max_cpu_error = max(
                    max_cpu_error,
                    abs(Float32(joined_h[token * 2 * HIDDEN + i]) - Float32(embedding_expected)),
                )
                max_cpu_error = max(
                    max_cpu_error,
                    abs(Float32(joined_h[token * 2 * HIDDEN + HIDDEN + i]) - Float32(hidden_expected)),
                )
            for row in range(HIDDEN):
                if serial_h[token * HIDDEN + row].to_bits() != projected_h[token * HIDDEN + row].to_bits():
                    raise Error("batch norm join nie zachowuje bit parity z serial prepare dla T=" + String(steps))

    if max_cpu_error > 0.001:
        raise Error("batch norm join przekracza tolerancje CPU dla T=" + String(steps))
    print("PASS T=", steps, " cpu=", max_cpu_error, " serial prepare=bit parity", sep="")


def _chunk_invariance(ctx: DeviceContext) raises:
    comptime STEPS = 32
    comptime HALF = 16
    var embeddings = ctx.enqueue_create_buffer[DType.float16](STEPS * HIDDEN)
    var target = ctx.enqueue_create_buffer[DType.float16](STEPS * HIDDEN)
    var initial = ctx.enqueue_create_buffer[DType.float16](HIDDEN)
    var enorm = ctx.enqueue_create_buffer[DType.float16](HIDDEN)
    var hnorm = ctx.enqueue_create_buffer[DType.float16](HIDDEN)
    var full = ctx.enqueue_create_buffer[DType.float16](STEPS * 2 * HIDDEN)
    var chunks = ctx.enqueue_create_buffer[DType.float16](STEPS * 2 * HIDDEN)
    var carry = ctx.enqueue_create_buffer[DType.float16](HIDDEN)
    with embeddings.map_to_host() as embedding_h, target.map_to_host() as target_h, initial.map_to_host() as initial_h, enorm.map_to_host() as enorm_h, hnorm.map_to_host() as hnorm_h:
        _fill_vectors(embedding_h.unsafe_ptr(), target_h.unsafe_ptr(), initial_h.unsafe_ptr(), enorm_h.unsafe_ptr(), hnorm_h.unsafe_ptr(), STEPS)

    ctx.enqueue_function[mtp_norm_join_shifted_f16](
        full.unsafe_ptr(), embeddings.unsafe_ptr(), target.unsafe_ptr(), initial.unsafe_ptr(),
        enorm.unsafe_ptr(), hnorm.unsafe_ptr(), STEPS, HIDDEN, EPS,
        grid_dim=STEPS, block_dim=BLOCK,
    )
    ctx.enqueue_function[mtp_norm_join_shifted_f16](
        chunks.unsafe_ptr(), embeddings.unsafe_ptr(), target.unsafe_ptr(), initial.unsafe_ptr(),
        enorm.unsafe_ptr(), hnorm.unsafe_ptr(), HALF, HIDDEN, EPS,
        grid_dim=HALF, block_dim=BLOCK,
    )
    ctx.synchronize()
    with target.map_to_host() as target_h, carry.map_to_host() as carry_h:
        for i in range(HIDDEN):
            carry_h[i] = target_h[(HALF - 1) * HIDDEN + i]
    ctx.enqueue_function[mtp_norm_join_shifted_f16](
        chunks.unsafe_ptr() + HALF * 2 * HIDDEN,
        embeddings.unsafe_ptr() + HALF * HIDDEN,
        target.unsafe_ptr() + HALF * HIDDEN,
        carry.unsafe_ptr(),
        enorm.unsafe_ptr(), hnorm.unsafe_ptr(), HALF, HIDDEN, EPS,
        grid_dim=HALF, block_dim=BLOCK,
    )
    ctx.synchronize()
    with full.map_to_host() as full_h, chunks.map_to_host() as chunks_h:
        for i in range(len(full_h)):
            if full_h[i].to_bits() != chunks_h[i].to_bits():
                raise Error("podzial 32 na 2x16 zmienia wynik norm join")
    print("PASS chunk invariance 32=2x16: bit parity")


def main() raises:
    var ctx = DeviceContext()
    for steps in [1, 2, 31, 32, 33]:
        _case(ctx, steps)
    _chunk_invariance(ctx)
