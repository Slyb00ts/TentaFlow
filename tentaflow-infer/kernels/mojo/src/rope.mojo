# ===== File: rope.mojo — rotary position embedding (neox pair layout) =====
# Qwen/Llama-family RoPE: within each head, element i pairs with i + head_dim/2.
# In-place rotation keeps Q/K tensors where the attention kernel expects them.

from std.gpu import block_dim, block_idx, thread_idx
from std.math import cos, sin, pow


def rope_neox_f16(
    x_io: UnsafePointer[Float16, MutAnyOrigin],
    positions: UnsafePointer[Int32, MutAnyOrigin],
    n_heads: Int,
    head_dim: Int,
    theta_base: Float32,
):
    """Rotate one (token, head) per block; threads cover head_dim/2 pairs.

    Layout: x_io is [n_tokens, n_heads, head_dim] contiguous; grid.x = tokens,
    grid.y = heads. Frequencies follow the neox convention
    inv_freq_j = theta_base^(-2j/head_dim).
    """
    token = Int(block_idx.x)
    head = Int(block_idx.y)
    half = head_dim // 2
    base = (token * n_heads + head) * head_dim
    pos = Float32(positions[token])

    var j = Int(thread_idx.x)
    while j < half:
        freq = pow(theta_base, Float32(-2 * j) / Float32(head_dim))
        angle = pos * freq
        c = cos(angle)
        s = sin(angle)
        a = Float32(x_io[base + j])
        b = Float32(x_io[base + half + j])
        x_io[base + j] = Float16(a * c - b * s)
        x_io[base + half + j] = Float16(a * s + b * c)
        j += Int(block_dim.x)


def rope_neox_partial_f16(
    x_io: UnsafePointer[Float16, MutAnyOrigin],
    positions: UnsafePointer[Int32, MutAnyOrigin],
    n_heads: Int,
    head_dim: Int,
    n_rot: Int,
    theta_base: Float32,
):
    """Partial NEOX rotary: rotate only the first `n_rot` dims of each head,
    pass the rest through unchanged. qwen35moe attention applies M-RoPE with
    sections [11,11,10,0] (sum·2 = n_rot = 64) over head_dim 256; for text-only
    positions M-RoPE reduces to this NEOX partial rotary. Pair (j, j+n_rot/2)
    with inv_freq_j = theta_base^(-2j/n_rot). Layout matches rope_neox_f16
    ([n_tokens, n_heads, head_dim]); grid.x = tokens, grid.y = heads.
    """
    token = Int(block_idx.x)
    head = Int(block_idx.y)
    half = n_rot // 2
    base = (token * n_heads + head) * head_dim
    pos = Float32(positions[token])

    var j = Int(thread_idx.x)
    while j < half:
        freq = pow(theta_base, Float32(-2 * j) / Float32(n_rot))
        angle = pos * freq
        c = cos(angle)
        s = sin(angle)
        a = Float32(x_io[base + j])
        b = Float32(x_io[base + half + j])
        x_io[base + j] = Float16(a * c - b * s)
        x_io[base + half + j] = Float16(a * s + b * c)
        j += Int(block_dim.x)


def rope_neox_ff_f16(
    x_io: UnsafePointer[Float16, MutAnyOrigin],
    positions: UnsafePointer[Int32, MutAnyOrigin],
    freq_factors: UnsafePointer[Float32, MutAnyOrigin],
    n_heads: Int,
    head_dim: Int,
    theta_base: Float32,
):
    """rope_neox_f16 z dzielnikiem częstotliwości na wymiar.

    Warstwy globalne Gemmy 4 stosują rope proporcjonalne: częstotliwość każdej
    pary jest dzielona przez `freq_factors[j]` (tensor `rope_freqs` o długości
    head_dim/2). Bez tego pozycje w warstwach globalnych rozjeżdżają się z
    warstwami okiennymi, które ropują zwyczajnie.
    """
    token = Int(block_idx.x)
    head = Int(block_idx.y)
    half = head_dim // 2
    base = (token * n_heads + head) * head_dim
    pos = Float32(positions[token])

    var j = Int(thread_idx.x)
    while j < half:
        freq = pow(theta_base, Float32(-2 * j) / Float32(head_dim)) / freq_factors[j]
        angle = pos * freq
        c = cos(angle)
        s = sin(angle)
        a = Float32(x_io[base + j])
        b = Float32(x_io[base + half + j])
        x_io[base + j] = Float16(a * c - b * s)
        x_io[base + half + j] = Float16(a * s + b * c)
        j += Int(block_dim.x)
