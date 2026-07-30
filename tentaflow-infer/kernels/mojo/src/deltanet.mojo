# ===== File: deltanet.mojo — Gated-DeltaNet linear-attention kernels (qwen35moe) =====
# Recurrent (per-token) path: depthwise causal conv + SiLU, per-head L2 norm,
# the rank-1 gated-delta state scan, and the output gated-RMSNorm. Each kernel
# is a bit-for-bit intent match of forge-formats/src/deltanet.rs (the CPU
# oracle) so the engine's DeltaNet layer can be validated per kernel before the
# full hybrid forward is trusted. State (conv window + d_state x d_state matrix)
# is resident per sequence and updated in place by these kernels.

from std.gpu import block_dim, block_idx, thread_idx, grid_dim
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import rsqrt, exp, log, fma
from src.reduce import block_reduce_sum


def _silu(v: Float32) -> Float32:
    return v / (1.0 + exp(-v))


def deltanet_conv_silu_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    win_io: UnsafePointer[Float16, MutAnyOrigin],
    x_new: UnsafePointer[Float16, MutAnyOrigin],
    weight: UnsafePointer[Float16, MutAnyOrigin],
    conv_dim: Int,
    d_conv: Int,
):
    """Depthwise causal 1-D conv (kernel width `d_conv`) + SiLU, one decode step.

    Layouts:
      win_io: [conv_dim, d_conv-1] f16, oldest sample first per channel.
      x_new:  [conv_dim] f16, the current input.
      weight: [conv_dim, d_conv] f16 — ggml ssm_conv1d {d_conv, conv_dim}
              flattened; tap `d_conv-1` multiplies the newest sample.
      out:    [conv_dim] f16 = silu(conv).
    Grid-stride over channels; the window is rotated in place afterwards
    (drop oldest, append x_new) so the next token reads the advanced state.
    """
    var c = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    stride = Int(grid_dim.x) * Int(block_dim.x)
    win_n = d_conv - 1
    while c < conv_dim:
        wbase = c * d_conv
        sbase = c * win_n
        var acc: Float32 = 0.0
        for j in range(win_n):
            acc += Float32(weight[wbase + j]) * Float32(win_io[sbase + j])
        xn = Float32(x_new[c])
        acc += Float32(weight[wbase + win_n]) * xn
        out_ptr[c] = Float16(_silu(acc))
        # Advance window: drop oldest, append newest.
        for j in range(win_n - 1):
            win_io[sbase + j] = win_io[sbase + j + 1]
        if win_n > 0:
            win_io[sbase + win_n - 1] = Float16(xn)
        c += stride


def l2norm_heads_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    x_in: UnsafePointer[Float16, MutAnyOrigin],
    d_state: Int,
    eps: Float32,
):
    """Per-head L2 normalization: out = x / sqrt(sum(x^2) + eps).

    One block per head (grid = n_heads), block covers `d_state` columns. Mirrors
    ggml_l2_norm applied to the conv q/k heads (deltanet.rs `l2_norm`): the
    denominator is the raw sum of squares (NOT divided by d_state).
    """
    head = Int(block_idx.x)
    base = head * d_state
    var ss: Float32 = 0.0
    var j = Int(thread_idx.x)
    while j < d_state:
        v = Float32(x_in[base + j])
        ss += v * v
        j += Int(block_dim.x)
    total = block_reduce_sum(ss)
    inv = rsqrt(total + eps)
    j = Int(thread_idx.x)
    while j < d_state:
        out_ptr[base + j] = Float16(Float32(x_in[base + j]) * inv)
        j += Int(block_dim.x)


def deltanet_gated_step_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    state_io: UnsafePointer[Float32, MutAnyOrigin],
    q_in: UnsafePointer[Float16, MutAnyOrigin],
    k_in: UnsafePointer[Float16, MutAnyOrigin],
    v_in: UnsafePointer[Float16, MutAnyOrigin],
    g_in: UnsafePointer[Float32, MutAnyOrigin],
    beta_in: UnsafePointer[Float32, MutAnyOrigin],
    d_state: Int,
):
    """One Gated-DeltaNet recurrence step per value-head (grid = n_v_heads).

    `state_io` is [n_v_heads, d_state, d_state] f32, row-major (i = key index,
    j = value index). q/k are already L2-normalized and repeated to n_v_heads;
    g is the per-head log-decay, beta the per-head write gate. Block = d_state
    threads, thread j owns value-column j entirely (all key rows i), so the
    whole step is column-parallel with a single barrier after staging k/q.

      decay      = exp(g)
      S[i,j]    *= decay
      kv[j]      = Σ_i k[i]·S[i,j]
      d[j]       = beta·(v[j] − kv[j])
      S[i,j]    += k[i]·d[j]
      o[j]       = Σ_i (q[i]/√d_state)·S[i,j]

    f32 state matches the CPU oracle; only q/k/v/out cross f16.
    """
    head = Int(block_idx.x)
    j = Int(thread_idx.x)
    if j >= d_state:
        return

    sk = stack_allocation[1024, Float32, address_space = AddressSpace.SHARED]()
    sq = stack_allocation[1024, Float32, address_space = AddressSpace.SHARED]()
    hbase = head * d_state
    sk[j] = Float32(k_in[hbase + j])
    sq[j] = Float32(q_in[hbase + j])
    barrier()

    decay = exp(g_in[head])
    beta = beta_in[head]
    inv_sqrt = rsqrt(Float32(d_state))
    sbase = head * d_state * d_state

    # Decay column j and accumulate kv_pred[j].
    var kv: Float32 = 0.0
    for i in range(d_state):
        idx = sbase + i * d_state + j
        s = fma(state_io[idx], decay, 0.0)
        kv += sk[i] * s
    dj = beta * (Float32(v_in[hbase + j]) - kv)

    # Rank-1 update of column j, then query the updated column.
    var o: Float32 = 0.0
    for i in range(d_state):
        idx = sbase + i * d_state + j
        decayed = fma(state_io[idx], decay, 0.0)
        s = decayed + sk[i] * dj
        state_io[idx] = s
        o += sq[i] * s
    out_ptr[hbase + j] = Float16(o * inv_sqrt)


def deltanet_gated_rmsnorm_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    o_in: UnsafePointer[Float16, MutAnyOrigin],
    z_in: UnsafePointer[Float16, MutAnyOrigin],
    weight: UnsafePointer[Float16, MutAnyOrigin],
    d_state: Int,
    eps: Float32,
):
    """Output gated RMSNorm per value-head: out = rmsnorm(o, weight)·silu(z).

    One block per head (grid = n_v_heads), block covers `d_state`. Matches
    deltanet.rs `gated_rmsnorm`: ss = mean(o^2), inv = 1/sqrt(ss+eps),
    out = o·inv·weight·silu(z).
    """
    head = Int(block_idx.x)
    base = head * d_state
    var ss: Float32 = 0.0
    var j = Int(thread_idx.x)
    while j < d_state:
        v = Float32(o_in[base + j])
        ss += v * v
        j += Int(block_dim.x)
    total = block_reduce_sum(ss)
    inv = rsqrt(total / Float32(d_state) + eps)
    j = Int(thread_idx.x)
    while j < d_state:
        normed = Float32(o_in[base + j]) * inv * Float32(weight[j])
        out_ptr[base + j] = Float16(normed * _silu(Float32(z_in[base + j])))
        j += Int(block_dim.x)


def deltanet_log_decay_f32(
    g_out: UnsafePointer[Float32, MutAnyOrigin],
    alpha_in: UnsafePointer[Float16, MutAnyOrigin],
    dt_bias: UnsafePointer[Float16, MutAnyOrigin],
    a_scale: UnsafePointer[Float16, MutAnyOrigin],
    n_v_heads: Int,
):
    """Per-head log-decay g = softplus(alpha + dt_bias)·a for the delta step.

    `alpha_in` is the per-head alpha projection output ([n_v_heads]); `dt_bias`
    and `a_scale` are the loaded ssm_dt.bias / ssm_a vectors ([n_v_heads]).
    Mirrors deltanet.rs `delta_log_decay` (softplus stable above 20).
    """
    h = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    if h >= n_v_heads:
        return
    x = Float32(alpha_in[h]) + Float32(dt_bias[h])
    var sp: Float32
    if x > 20.0:
        sp = x
    else:
        sp = log(1.0 + exp(x))
    g_out[h] = sp * Float32(a_scale[h])


def deltanet_beta_sigmoid_f32(
    beta_out: UnsafePointer[Float32, MutAnyOrigin],
    beta_in: UnsafePointer[Float16, MutAnyOrigin],
    n_v_heads: Int,
):
    """Per-head write gate beta = sigmoid(ssm_beta·x). `beta_in` is the loaded
    beta projection output ([n_v_heads]); result feeds the delta step."""
    h = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    if h >= n_v_heads:
        return
    beta_out[h] = 1.0 / (1.0 + exp(-Float32(beta_in[h])))


def deltanet_repeat_qk_f16(
    q_dst: UnsafePointer[Float16, MutAnyOrigin],
    k_dst: UnsafePointer[Float16, MutAnyOrigin],
    q_src: UnsafePointer[Float16, MutAnyOrigin],
    k_src: UnsafePointer[Float16, MutAnyOrigin],
    n_elems: Int,
    rep: Int,
):
    """Powtarza bloki q/k głowic K `rep` razy, żeby pokryć głowice V (GQA).

    Skan gated-delta indeksuje q/k numerem głowicy V, więc bloki głowic K muszą
    leżeć powtórzone. Robił to `rep` par kopii D2D na warstwę — osiem uruchomień
    przy `rep = 4`, czyli 384 na token dla 48 warstw DeltaNet. Zmierzone na
    R9700: sama kopia trwa 1,1 us, ale każde uruchomienie kosztuje jeszcze ~3,8 us
    przestoju, więc liczy się ICH LICZBA, nie przenoszone bajty.
    """
    index = Int(block_idx.x) * Int(block_dim.x) + Int(thread_idx.x)
    if index >= n_elems * rep:
        return
    source = index % n_elems
    q_dst[index] = q_src[source]
    k_dst[index] = k_src[source]
