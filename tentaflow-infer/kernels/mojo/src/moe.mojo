# ===== File: moe.mojo — Mixture-of-Experts routing + accumulation kernels =====
# The router computes per-token expert logits (x · gate_inp), softmaxes over
# ALL experts, then selects the top-k and (optionally) renormalizes their
# weights — matching the HF reference (softmax-then-topk). The engine reads the
# selected expert ids back to the host to launch the per-expert quant GEMVs
# (existing gemm-row kernels, indexed by expert byte offset), then folds each
# expert's output back into the token with moe_scale_add_f16.

from std.gpu import block_dim, block_idx, thread_idx, global_idx
from std.gpu.sync import barrier
from std.gpu.primitives import warp
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import exp
from std.atomic import Atomic

# One block per token stages that token's hidden vector in shared f32; caps the
# supported hidden size and expert count (every current MoE model fits).
comptime MOE_MAX_HIDDEN = 8192
comptime MOE_MAX_EXPERTS = 256
comptime NEG_BIG: Float32 = -3.0e38


def moe_router_f16(
    ids_ptr: UnsafePointer[Int32, MutAnyOrigin],
    weights_ptr: UnsafePointer[Float32, MutAnyOrigin],
    x_ptr: UnsafePointer[Float16, MutAnyOrigin],
    w_ptr: UnsafePointer[Float16, MutAnyOrigin],
    counts_ptr: UnsafePointer[Int32, MutAnyOrigin],
    hidden: Int,
    n_expert: Int,
    top_k: Int,
    norm_topk: Int,
):
    """Route each token to its top-k experts.

    grid=(n_tokens,1,1), block=256. Writes ids[token, k] (Int32) and the
    softmax routing weights[token, k] (Float32). `norm_topk` renormalizes the
    selected weights to sum 1.

    `counts_ptr[e]` accumulates how often expert `e` was selected. That tally is
    what drives weight residency: the hot experts belong in VRAM and the cold
    ones in host memory, and nothing else on the device knows the routing. The
    add is atomic because a prefill launches one block per token.
    """
    token = Int(block_idx.x)
    tid = Int(thread_idx.x)
    nthreads = Int(block_dim.x)

    xs = stack_allocation[MOE_MAX_HIDDEN, Float32, address_space = AddressSpace.SHARED]()
    logits = stack_allocation[MOE_MAX_EXPERTS, Float32, address_space = AddressSpace.SHARED]()

    # Stage the token's hidden vector once (reused across every expert dot).
    xbase = token * hidden
    var i = tid
    while i < hidden:
        xs[i] = Float32(x_ptr[xbase + i])
        i += nthreads
    barrier()

    # Warp-per-expert dot products: warp `wid` sweeps experts wid, wid+n_warps…
    lane = tid % 32
    wid = tid // 32
    n_warps = nthreads // 32
    var e = wid
    while e < n_expert:
        wbase = e * hidden
        var acc: Float32 = 0.0
        var j = lane
        while j < hidden:
            acc += xs[j] * Float32(w_ptr[wbase + j])
            j += 32
        acc = warp.sum(acc)
        if lane == 0:
            logits[e] = acc
        e += n_warps
    barrier()

    # Softmax over all experts, then top-k selection, on a single thread: the
    # expert count is tiny (≤256) so a serial pass is cheaper than a parallel
    # reduction and keeps the tie-break order identical to a CPU reference.
    if tid == 0:
        var mx = logits[0]
        var n = 1
        while n < n_expert:
            if logits[n] > mx:
                mx = logits[n]
            n += 1
        var denom: Float32 = 0.0
        n = 0
        while n < n_expert:
            denom += exp(logits[n] - mx)
            n += 1
        n = 0
        while n < n_expert:
            logits[n] = exp(logits[n] - mx) / denom
            n += 1

        # Repeated argmax (top_k ≤ 8): pick the largest remaining probability,
        # masking selected experts. Ties resolve to the lowest index.
        var wsum: Float32 = 0.0
        var kk = 0
        while kk < top_k:
            var best_i = 0
            var best_v = NEG_BIG
            n = 0
            while n < n_expert:
                if logits[n] > best_v:
                    best_v = logits[n]
                    best_i = n
                n += 1
            ids_ptr[token * top_k + kk] = Int32(best_i)
            _ = Atomic.fetch_add(counts_ptr + best_i, Int32(1))
            weights_ptr[token * top_k + kk] = best_v
            wsum += best_v
            logits[best_i] = NEG_BIG
            kk += 1

        if norm_topk != 0 and wsum > 0.0:
            var inv = 1.0 / wsum
            kk = 0
            while kk < top_k:
                weights_ptr[token * top_k + kk] = weights_ptr[token * top_k + kk] * inv
                kk += 1


def moe_scale_add_f16(
    acc_ptr: UnsafePointer[Float16, MutAnyOrigin],
    src_ptr: UnsafePointer[Float16, MutAnyOrigin],
    n: Int,
    scale: Float32,
    init: Int,
):
    """acc += scale * src over n f16 elements (init != 0 overwrites instead).

    Folds one routed expert's output into the token's FFN accumulator, scaled
    by its router weight. `init` seeds the accumulator with the first expert so
    no separate zero-fill is needed.
    """
    i = Int(global_idx.x)
    if i < n:
        s = scale * Float32(src_ptr[i])
        if init != 0:
            acc_ptr[i] = Float16(s)
        else:
            acc_ptr[i] = Float16(Float32(acc_ptr[i]) + s)


def moe_scale_add_gidx_f16(
    acc_ptr: UnsafePointer[Float16, MutAnyOrigin],
    src_ptr: UnsafePointer[Float16, MutAnyOrigin],
    n: Int,
    weights_ptr: UnsafePointer[Float32, MutAnyOrigin],
    sel: Int,
    init: Int,
):
    """acc += weights_ptr[sel] * src over n f16 elements (init != 0 overwrites).

    Identical to moe_scale_add_f16 but the router weight is read ON DEVICE from
    `weights_ptr[sel]`, so folding a routed expert's output needs no host
    readback of the selection weights. For the shared expert, `weights_ptr`
    points at the device-resident sigmoid gate scale (sel = 0)."""
    i = Int(global_idx.x)
    if i < n:
        s = weights_ptr[sel] * Float32(src_ptr[i])
        if init != 0:
            acc_ptr[i] = Float16(s)
        else:
            acc_ptr[i] = Float16(Float32(acc_ptr[i]) + s)


def moe_sigmoid_f16_to_f32(
    out_ptr: UnsafePointer[Float32, MutAnyOrigin],
    in_ptr: UnsafePointer[Float16, MutAnyOrigin],
    n: Int,
):
    """out[i] = sigmoid(in[i]) over n shared-expert gate logits.

    The logits (f16, produced by the gate projection) become device-resident
    f32 scales, so folding the shared expert costs no host round-trip. `n` is
    the step's token count: one logit per token, and the whole step at once
    when its projections ran as one matrix."""
    i = Int(global_idx.x)
    if i < n:
        logit = Float32(in_ptr[i])
        out_ptr[i] = 1.0 / (1.0 + exp(-logit))


def moe_combine_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    src_ptr: UnsafePointer[Float16, MutAnyOrigin],
    slots_ptr: UnsafePointer[Int32, MutAnyOrigin],
    weights_ptr: UnsafePointer[Float32, MutAnyOrigin],
    cols: Int,
    top_k: Int,
    init: Int,
):
    """out[t] = sum_j weights[t*top_k+j] * src[slots[t*top_k+j]] over cols f16.

    The inverse of the gather that groups a step's selections by expert: once
    every expert has multiplied its own block of rows, each token's answer is
    scattered across `top_k` rows of `src` and this brings them back together.

    One block per token, and the sum walks `j` in the order the router chose,
    which is the order the per-token route accumulated in. It differs from that
    route in ONE respect and deliberately: the running sum stays f32 across all
    `top_k` and is rounded to f16 once, where folding expert by expert rounded
    after each. That is a step TOWARD the f32 reference, not away from it.

    `init` overwrites instead of accumulating. The shared expert folds on top
    of the routed sum through this same kernel with `top_k = 1` and identity
    slots: its per-token sigmoid gate IS a routing weight, and giving it a
    second kernel would mean two places to keep the rounding equal.
    """
    t = Int(block_idx.x)
    base = t * top_k
    i = Int(thread_idx.x)
    while i < cols:
        acc = Float32(0)
        for j in range(top_k):
            row = Int(slots_ptr[base + j])
            acc += weights_ptr[base + j] * Float32(src_ptr[row * cols + i])
        if init != 0:
            out_ptr[t * cols + i] = Float16(acc)
        else:
            out_ptr[t * cols + i] = Float16(Float32(out_ptr[t * cols + i]) + acc)
        i += Int(block_dim.x)


def moe_topk_f32(
    ids_ptr: UnsafePointer[Int32, MutAnyOrigin],
    weights_ptr: UnsafePointer[Float32, MutAnyOrigin],
    logits_ptr: UnsafePointer[Float32, MutAnyOrigin],
    counts_ptr: UnsafePointer[Int32, MutAnyOrigin],
    n_expert: Int,
    top_k: Int,
    norm_topk: Int,
):
    """Softmax i wybór top-k z GOTOWYCH logitów routera.

    `moe_router_f16` liczył iloczyny ekspertów i wybór w jednym bloku na token.
    Przy generacji token jest JEDEN, więc cała projekcja routera — dla 256
    ekspertów i 2048 kanałów milion bajtów wagi — szła przez jeden
    multiprocesor, i to była prawie jedna trzecia czasu kroku. Iloczyny są
    zwykłym GEMV, więc liczy je GEMV; tutaj zostaje wyłącznie to, co naprawdę
    jest jednym blokiem na token.

    Redukcje maksimum i sumy idą przez blok, a samo top-k pozostaje szeregowe na
    zerowym wątku: przy `n_expert <= 256` i `top_k <= 8` to kilka tysięcy
    iteracji, a kolejność rozstrzygania remisów zostaje ta sama co we wzorcu.
    """
    token = Int(block_idx.x)
    tid = Int(thread_idx.x)
    nthreads = Int(block_dim.x)

    logits = stack_allocation[
        MOE_MAX_EXPERTS, Float32, address_space = AddressSpace.SHARED
    ]()
    base = token * n_expert
    var i = tid
    while i < n_expert:
        logits[i] = logits_ptr[base + i]
        i += nthreads
    barrier()

    if tid == 0:
        var mx = logits[0]
        var n = 1
        while n < n_expert:
            if logits[n] > mx:
                mx = logits[n]
            n += 1
        var denom: Float32 = 0.0
        n = 0
        while n < n_expert:
            denom += exp(logits[n] - mx)
            n += 1
        n = 0
        while n < n_expert:
            logits[n] = exp(logits[n] - mx) / denom
            n += 1

        var wsum: Float32 = 0.0
        var kk = 0
        while kk < top_k:
            var best_i = 0
            var best_v = NEG_BIG
            n = 0
            while n < n_expert:
                if logits[n] > best_v:
                    best_v = logits[n]
                    best_i = n
                n += 1
            ids_ptr[token * top_k + kk] = Int32(best_i)
            _ = Atomic.fetch_add(counts_ptr + best_i, Int32(1))
            weights_ptr[token * top_k + kk] = best_v
            wsum += best_v
            logits[best_i] = NEG_BIG
            kk += 1

        if norm_topk != 0 and wsum > 0.0:
            var inv = 1.0 / wsum
            kk = 0
            while kk < top_k:
                weights_ptr[token * top_k + kk] = (
                    weights_ptr[token * top_k + kk] * inv
                )
                kk += 1
