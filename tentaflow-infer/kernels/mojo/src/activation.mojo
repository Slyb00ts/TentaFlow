# ===== File: activation.mojo — elementwise activation kernels (SwiGLU) =====

from std.gpu import block_dim, block_idx, thread_idx, global_idx
from std.math import exp


def silu_mul_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    gate: UnsafePointer[Float16, MutAnyOrigin],
    up: UnsafePointer[Float16, MutAnyOrigin],
    n: Int,
):
    """out = silu(gate) * up over n contiguous elements (SwiGLU FFN)."""
    i = Int(global_idx.x)
    if i < n:
        g = Float32(gate[i])
        s = g / (1.0 + exp(-g))
        out_ptr[i] = Float16(s * Float32(up[i]))


def gelu_mul_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    gate: UnsafePointer[Float16, MutAnyOrigin],
    up: UnsafePointer[Float16, MutAnyOrigin],
    n: Int,
):
    """out = gelu(gate) * up nad n kolejnymi elementami (GeGLU, rodzina Gemma).

    Wariant tanh (`gelu_pytorch_tanh`), bo to on jest w referencji Gemmy — nie
    dokladny erf. Rozjazd miedzy nimi jest rzedu 1e-3 i widoczny w logitach.
    """
    i = Int(global_idx.x)
    if i < n:
        g = Float32(gate[i])
        inner = 0.7978845608028654 * (g + 0.044715 * g * g * g)
        # tanh liczone jako 1 - 2/(exp(2x)+1): przy dużych bramkach (|g| ~ 30,
        # realne w FFN) exp(2x) przelewa się do inf, a ta postać nasyca się wtedy
        # do +-1. Wariant (e-1)/(e+1) dawał inf/inf = NaN, co ścieżka int8
        # kwantyzacji aktywacji zamieniała w ciche śmieci.
        tanh_inner = 1.0 - 2.0 / (exp(2.0 * inner) + 1.0)
        out_ptr[i] = Float16(0.5 * g * (1.0 + tanh_inner) * Float32(up[i]))


def sigmoid_mul_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Float16, MutAnyOrigin],
    gate: UnsafePointer[Float16, MutAnyOrigin],
    n: Int,
):
    """out = a * sigmoid(gate) over n contiguous elements (attention output gate)."""
    i = Int(global_idx.x)
    if i < n:
        g = Float32(gate[i])
        s = 1.0 / (1.0 + exp(-g))
        out_ptr[i] = Float16(Float32(a[i]) * s)


def deinterleave_gate_f16(
    qc: UnsafePointer[Float16, MutAnyOrigin],
    gatec: UnsafePointer[Float16, MutAnyOrigin],
    q_full: UnsafePointer[Float16, MutAnyOrigin],
    head_dim: Int,
    n: Int,
):
    """De-interleave the gated Q projection [n_heads, 2*head_dim] into query and
    gate halves: for element i (head h = i // head_dim, lane d = i % head_dim),
    qc[i] = q_full[h*2*head_dim + d], gatec[i] = q_full[h*2*head_dim + head_dim + d].
    Pure data move (no math) — bit-identical to the per-head copy loop."""
    i = Int(global_idx.x)
    if i < n:
        h = i // head_dim
        d = i % head_dim
        base = h * 2 * head_dim + d
        qc[i] = q_full[base]
        gatec[i] = q_full[base + head_dim]


def scale_f16(
    buf: UnsafePointer[Float16, MutAnyOrigin],
    n: Int,
    factor: Float32,
):
    """buf *= factor w miejscu, nad n elementami f16.

    Potrzebne rodzinie Gemma, która mnoży embedding wejściowy przez
    `sqrt(hidden)`. Sama norma RMS tego nie widzi (jest niezmiennicza na skalę),
    ale strumień rezydualny już tak — dlatego to nie jest operacja pusta.
    """
    i = Int(global_idx.x)
    if i < n:
        buf[i] = Float16(Float32(buf[i]) * factor)


def softcap_f32(
    logits: UnsafePointer[Float32, MutAnyOrigin],
    n: Int,
    cap: Float32,
):
    """logits = cap * tanh(logits / cap) w miejscu.

    Ograniczenie logitów rodziny Gemma, stosowane po `output_norm`, a przed
    samplingiem.
    """
    i = Int(global_idx.x)
    if i < n:
        x = Float32(logits[i]) / cap
        e = exp(2.0 * x)
        logits[i] = cap * ((e - 1.0) / (e + 1.0))


def cast_f32_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    src: UnsafePointer[Float32, MutAnyOrigin],
    n: Int,
):
    """out = f16(src) nad n elementami.

    Tensor parallel sumuje wyniki cząstkowe projekcji `down` w f32, bo dodawanie
    w f16 gubiłoby bity przy każdej karcie. Strumień rezydualny silnika jest
    natomiast f16 — ten kernel jest jedynym miejscem, gdzie te dwie
    reprezentacje się spotykają.
    """
    i = Int(global_idx.x)
    if i < n:
        out_ptr[i] = Float16(src[i])


def add_f32_out_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    a: UnsafePointer[Float32, MutAnyOrigin],
    b: UnsafePointer[Float32, MutAnyOrigin],
    n: Int,
):
    """out = f16(a + b) nad n elementami.

    Ogon podziału kolumnowego na dwie karty: suma cząstkowa karty zbierającej
    plus przysłana suma drugiej karty, od razu zawężona do f16 strumienia
    rezydualnego. Osobne dodawanie i osobne zawężenie to były dwa uruchomienia na
    warstwę, czyli 130 na token — przy zmierzonym narzucie rzędu 4,5 us na
    uruchomienie to więcej niż cała wymiana aktywacji między kartami.
    """
    i = Int(global_idx.x)
    if i < n:
        out_ptr[i] = Float16(a[i] + b[i])
