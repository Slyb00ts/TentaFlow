# ===== File: test_sampling.mojo — on-GPU numeric checks for sampling kernels =====
# Gates: argmax bit-match vs a sequential CPU scan (ties -> lowest index),
# repetition-penalty math, top-k membership of every sampled id across 10k
# fixed-seed draws, and per-(seed, step) determinism.

from std.gpu.host import DeviceContext
from src.sampling import (
    penalize_f32,
    argmax_partial_f32,
    argmax_final_f32,
    topk_partial_f32,
    topk_final_f32,
)

comptime VOCAB = 151936
comptime CHUNK = 4096
comptime BLOCK = 256
comptime K = 40


def _logit(i: Int) -> Float32:
    # Deterministic pseudo-random logits in roughly [-12, 12].
    h = (i * 2654435761) % 4093
    return Float32(h - 2046) * 0.006


def main() raises:
    var ctx = DeviceContext()
    n_blocks = (VOCAB + CHUNK - 1) // CHUNK

    var logits = ctx.enqueue_create_buffer[DType.float32](VOCAB)
    var part_vals = ctx.enqueue_create_buffer[DType.float32](n_blocks * K)
    var part_idx = ctx.enqueue_create_buffer[DType.int32](n_blocks * K)
    var out_id = ctx.enqueue_create_buffer[DType.int32](1)
    var out_lp = ctx.enqueue_create_buffer[DType.float32](1)

    # --- argmax vs CPU, including a planted tie (lowest index must win) ---
    with logits.map_to_host() as lh:
        for i in range(VOCAB):
            lh[i] = _logit(i)
        # Planted tie above every generated logit: index 1234 must win.
        lh[1234] = 50.0
        lh[140000] = 50.0
    var cpu_best = 0
    var cpu_val: Float32 = -3.4e38
    for i in range(VOCAB):
        v = _logit(i)
        if i == 1234 or i == 140000:
            v = 50.0
        if v > cpu_val:
            cpu_val = v
            cpu_best = i
    ctx.enqueue_function[argmax_partial_f32](
        logits.unsafe_ptr(), part_vals.unsafe_ptr(), part_idx.unsafe_ptr(),
        VOCAB, CHUNK,
        grid_dim=n_blocks, block_dim=BLOCK,
    )
    ctx.enqueue_function[argmax_final_f32](
        out_id.unsafe_ptr(), out_lp.unsafe_ptr(),
        part_vals.unsafe_ptr(), part_idx.unsafe_ptr(), n_blocks,
        grid_dim=1, block_dim=BLOCK,
    )
    ctx.synchronize()
    with out_id.map_to_host() as oh:
        if Int(oh[0]) != cpu_best or cpu_best != 1234:
            raise Error("argmax mismatch: got " + String(Int(oh[0])) + " want " + String(cpu_best))
    print("argmax tie->lowest ok:", cpu_best)

    # --- argmax on plain random logits (no plant) ---
    with logits.map_to_host() as lh:
        lh[1234] = _logit(1234)
        lh[140000] = _logit(140000)
    cpu_best = 0
    cpu_val = -3.4e38
    for i in range(VOCAB):
        v = _logit(i)
        if v > cpu_val:
            cpu_val = v
            cpu_best = i
    ctx.enqueue_function[argmax_partial_f32](
        logits.unsafe_ptr(), part_vals.unsafe_ptr(), part_idx.unsafe_ptr(),
        VOCAB, CHUNK,
        grid_dim=n_blocks, block_dim=BLOCK,
    )
    ctx.enqueue_function[argmax_final_f32](
        out_id.unsafe_ptr(), out_lp.unsafe_ptr(),
        part_vals.unsafe_ptr(), part_idx.unsafe_ptr(), n_blocks,
        grid_dim=1, block_dim=BLOCK,
    )
    ctx.synchronize()
    with out_id.map_to_host() as oh:
        if Int(oh[0]) != cpu_best:
            raise Error("argmax mismatch: got " + String(Int(oh[0])) + " want " + String(cpu_best))
    print("argmax random ok:", cpu_best)

    # --- repetition penalty: positive divides, negative multiplies ---
    comptime N_PEN = 3
    var pen_ids = ctx.enqueue_create_buffer[DType.int32](N_PEN)
    with pen_ids.map_to_host() as ph:
        ph[0] = 7
        ph[1] = 90000
        ph[2] = 151935
    with logits.map_to_host() as lh:
        lh[7] = 4.0
        lh[90000] = -2.0
        lh[151935] = 0.0
    ctx.enqueue_function[penalize_f32](
        logits.unsafe_ptr(), pen_ids.unsafe_ptr(), N_PEN, Float32(1.25),
        grid_dim=1, block_dim=BLOCK,
    )
    ctx.synchronize()
    with logits.map_to_host() as lh:
        if abs(Float32(lh[7]) - 3.2) > 1e-5 or abs(Float32(lh[90000]) + 2.5) > 1e-5 or Float32(lh[151935]) != 0.0:
            raise Error("penalize_f32 mismatch")
        lh[7] = _logit(7)
        lh[90000] = _logit(90000)
        lh[151935] = _logit(151935)
    print("penalize ok")

    # --- top-k draw: 10k fixed-seed draws all land in the CPU top-k set ---
    # CPU allowed set: ids whose logit >= k-th largest value (tie superset).
    var thresh: Float32 = 3.4e38
    for _ in range(K):
        var best: Float32 = -3.4e38
        for i in range(VOCAB):
            v = _logit(i)
            if v < thresh and v > best:
                best = v
        thresh = best
    ctx.enqueue_function[topk_partial_f32](
        logits.unsafe_ptr(), part_vals.unsafe_ptr(), part_idx.unsafe_ptr(),
        VOCAB, CHUNK, K,
        grid_dim=n_blocks, block_dim=BLOCK,
    )
    ctx.synchronize()
    comptime N_DRAWS = 10000
    var first_draw = -1
    var distinct = 0
    var last = -1
    for d in range(N_DRAWS):
        ctx.enqueue_function[topk_final_f32](
            out_id.unsafe_ptr(), out_lp.unsafe_ptr(),
            part_vals.unsafe_ptr(), part_idx.unsafe_ptr(),
            n_blocks * K, K,
            Float32(1.0 / 0.7), Float32(0.95), Float32(0.0),
            UInt64(0xC0FFEE + d % 4), UInt64(d),
            grid_dim=1, block_dim=BLOCK,
        )
        ctx.synchronize()
        with out_id.map_to_host() as oh:
            tok = Int(oh[0])
            if tok < 0 or tok >= VOCAB or _logit(tok) < thresh:
                raise Error("draw " + String(d) + " out of top-k set: " + String(tok))
            if d == 0:
                first_draw = tok
            if tok != last:
                distinct += 1
                last = tok
    if distinct < 2:
        raise Error("10k draws produced a single token — draw is not sampling")
    print("topk membership ok over", N_DRAWS, "draws")

    # --- determinism: same (seed, step) reproduces the same token ---
    ctx.enqueue_function[topk_final_f32](
        out_id.unsafe_ptr(), out_lp.unsafe_ptr(),
        part_vals.unsafe_ptr(), part_idx.unsafe_ptr(),
        n_blocks * K, K,
        Float32(1.0 / 0.7), Float32(0.95), Float32(0.0),
        UInt64(0xC0FFEE), UInt64(0),
        grid_dim=1, block_dim=BLOCK,
    )
    ctx.synchronize()
    with out_id.map_to_host() as oh:
        if Int(oh[0]) != first_draw:
            raise Error("draw not deterministic per (seed, step)")
    print("determinism ok")

    # --- top_p -> 0 degenerates to argmax of the top-k set ---
    ctx.enqueue_function[topk_final_f32](
        out_id.unsafe_ptr(), out_lp.unsafe_ptr(),
        part_vals.unsafe_ptr(), part_idx.unsafe_ptr(),
        n_blocks * K, K,
        Float32(1.0 / 0.7), Float32(1e-9), Float32(0.0),
        UInt64(0xDEAD), UInt64(7),
        grid_dim=1, block_dim=BLOCK,
    )
    ctx.synchronize()
    cpu_best = 0
    cpu_val = -3.4e38
    for i in range(VOCAB):
        v = _logit(i)
        if v > cpu_val:
            cpu_val = v
            cpu_best = i
    with out_id.map_to_host() as oh:
        if Int(oh[0]) != cpu_best:
            raise Error("top_p->0 should pick argmax")
    print("top_p degenerate ok")
    print("ALL SAMPLING CHECKS PASSED")
