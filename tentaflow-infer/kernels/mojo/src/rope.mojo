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
