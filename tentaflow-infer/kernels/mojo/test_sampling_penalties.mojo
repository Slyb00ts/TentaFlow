# =============================================================================
# Plik: test_sampling_penalties.mojo
# Opis: Sprawdza fused kary repetition/frequency/presence z argmax i top-k.
# Przykład: pixi run mojo test_sampling_penalties.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from src.sampling import penalized_argmax_f32, penalized_topk_f32


def float_from_bits(bits: UInt32) -> Float32:
    return UnsafePointer(to=bits).bitcast[Float32]()[0]


def main() raises:
    comptime vocab = 8
    var ctx = DeviceContext()
    var logits = ctx.enqueue_create_buffer[DType.float32](vocab)
    var ids = ctx.enqueue_create_buffer[DType.int32](2)
    var counts = ctx.enqueue_create_buffer[DType.int32](2)
    var out_id = ctx.enqueue_create_buffer[DType.int32](1)
    var out_lp = ctx.enqueue_create_buffer[DType.float32](1)

    with logits.map_to_host() as values:
        values[0] = 1.0
        values[1] = 8.0
        values[2] = -2.0
        values[3] = 4.0
        for i in range(4, vocab):
            values[i] = 0.0
    with ids.map_to_host() as values:
        values[0] = 1
        values[1] = 2
    with counts.map_to_host() as values:
        values[0] = 2
        values[1] = 1
    ctx.enqueue_function[penalized_argmax_f32](
        out_id.unsafe_ptr(), out_lp.unsafe_ptr(), logits.unsafe_ptr(),
        ids.unsafe_ptr(), counts.unsafe_ptr(), 2, vocab,
        Float32(2.0), Float32(0.5), Float32(0.25), grid_dim=1, block_dim=256,
    )
    ctx.synchronize()
    with out_id.map_to_host() as result, logits.map_to_host() as penalized:
        if result[0] != 3 or penalized[1] != 2.75 or penalized[2] != -4.75:
            raise Error("fused penalty argmax: FAIL")

    with logits.map_to_host() as values:
        values[0] = 1.0
        values[1] = 8.0
        values[2] = -2.0
        values[3] = 4.0
        for i in range(4, vocab):
            values[i] = 0.0
    ctx.enqueue_function[penalized_topk_f32](
        out_id.unsafe_ptr(), out_lp.unsafe_ptr(), logits.unsafe_ptr(),
        ids.unsafe_ptr(), counts.unsafe_ptr(), 2, vocab,
        Float32(2.0), Float32(0.5), Float32(0.25), 2,
        Float32(1.0), Float32(0.000001), Float32(0.0),
        UInt64(17), UInt64(3), grid_dim=1, block_dim=256,
    )
    ctx.synchronize()
    with out_id.map_to_host() as result:
        if result[0] != 3:
            raise Error("fused penalty top-k: FAIL")

    with logits.map_to_host() as values:
        values[0] = 1.0
        values[1] = 8.0
        values[2] = -2.0
        values[3] = 4.0
        for i in range(4, vocab):
            values[i] = 0.0
    ctx.enqueue_function[penalized_topk_f32](
        out_id.unsafe_ptr(), out_lp.unsafe_ptr(), logits.unsafe_ptr(),
        ids.unsafe_ptr(), counts.unsafe_ptr(), 2, vocab,
        Float32(2.0), Float32(0.5), Float32(0.25), 2,
        Float32(1.0), Float32(1.0), Float32(0.0),
        UInt64(0), UInt64(3), grid_dim=1, block_dim=256,
    )
    ctx.synchronize()
    with out_id.map_to_host() as result:
        if result[0] != 1:
            raise Error("fused penalty seeded golden: FAIL")

    with logits.map_to_host() as values:
        for i in range(vocab):
            values[i] = float_from_bits(UInt32(0xFF800000))
        values[1] = float_from_bits(UInt32(0x7FC00000))
        values[6] = float_from_bits(UInt32(0x7FC00000))
    ctx.enqueue_function[penalized_topk_f32](
        out_id.unsafe_ptr(), out_lp.unsafe_ptr(), logits.unsafe_ptr(),
        ids.unsafe_ptr(), counts.unsafe_ptr(), 2, vocab,
        Float32(1.0), Float32(0.0), Float32(0.0), vocab,
        Float32(1.0), Float32(1.0), Float32(0.0),
        UInt64(17), UInt64(3), grid_dim=1, block_dim=256,
    )
    ctx.synchronize()
    with out_id.map_to_host() as result, out_lp.map_to_host() as logprob:
        if result[0] != 0 or logprob[0] != 0.0:
            raise Error("fused penalty top-k invalid logits: FAIL")
    print("sampling penalties argmax/top-k: PASS")
