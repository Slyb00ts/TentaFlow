# ===== File: deepseek.mojo — przekształcenia aktywacji DeepSeeka V4 =====
#
# Pięć operacji, których nie ma w pozostałych architekturach, a bez których
# ścieżka tego modelu nie policzy się poprawnie. Każda ma referencję CPU
# przypiętą do implementacji autorów modelu w testach
# `forge-formats/tests/deepseek_v4_attention.rs`.
#
# Wspólna cecha: wszystkie działają W MIEJSCU na buforze aktywacji, bo w tym
# modelu są nakładane na wycinki (ostatnie 64 wymiary głowicy, część bez rope),
# a nie na cały tensor.

from std.gpu import block_dim, block_idx, thread_idx, global_idx
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import stack_allocation
from std.math import exp, cos, sin, rsqrt
from std.memory import bitcast

comptime DS_MAX_HEAD = 512
comptime FP8_MAX: Float32 = 448.0
comptime FP4_MAX: Float32 = 6.0


def rmsnorm_head_f16(
    buf: UnsafePointer[Float16, MutAnyOrigin],
    head_dim: Int,
    n_heads: Int,
    eps: Float32,
):
    """Normalizacja RMS osobno dla każdej głowicy, BEZ wagi.

    Q dostaje ją już po projekcji `wq_b`, obok zwykłej normy na wejściu — to
    druga, oddzielna normalizacja, a nie ta sama zastosowana inaczej.
    Jeden blok na głowicę; redukcja w f32 niezależnie od formatu zapisu.
    """
    head = Int(block_idx.x)
    if head >= n_heads:
        return
    tid = Int(thread_idx.x)
    nthreads = Int(block_dim.x)
    base = head * head_dim

    partial = stack_allocation[
        1024, Float32, address_space = AddressSpace.SHARED
    ]()
    var acc: Float32 = 0.0
    var i = tid
    while i < head_dim:
        v = Float32(buf[base + i])
        acc += v * v
        i += nthreads
    partial[tid] = acc
    barrier()
    var stride = nthreads // 2
    while stride > 0:
        if tid < stride:
            partial[tid] += partial[tid + stride]
        barrier()
        stride //= 2
    inv = rsqrt(partial[0] / Float32(head_dim) + eps)
    i = tid
    while i < head_dim:
        buf[base + i] = Float16(Float32(buf[base + i]) * inv)
        i += nthreads


def rope_interleaved_f16(
    buf: UnsafePointer[Float16, MutAnyOrigin],
    freqs: UnsafePointer[Float32, MutAnyOrigin],
    row_stride: Int,
    offset: Int,
    rope_dim: Int,
    n_rows: Int,
    pos_base: Int,
    pos_stride: Int,
    inverse: Int,
):
    """Rope obracające pary SĄSIADUJĄCE `(2i, 2i+1)` na wycinku wiersza.

    Reszta FORGE używa układu NeoX (pary `(i, i + dim/2)`), który dla tych wag
    daje liczby tego samego rzędu i inny wynik. `offset` wskazuje początek
    wycinka obejmowanego przez rope, bo tutaj obraca się tylko ogon głowicy.
    `inverse` sprzęga obrót — tak wygląda rope nakładane na WYJŚCIE uwagi.
    """
    idx = Int(global_idx.x)
    pairs = rope_dim // 2
    if idx >= n_rows * pairs:
        return
    row = idx // pairs
    pair = idx % pairs
    pos = pos_base + row * pos_stride
    angle = Float32(pos) * freqs[pair]
    c = cos(angle)
    s = sin(angle)
    if inverse != 0:
        s = -s
    at = row * row_stride + offset + 2 * pair
    a = Float32(buf[at])
    b = Float32(buf[at + 1])
    buf[at] = Float16(a * c - b * s)
    buf[at + 1] = Float16(a * s + b * c)


def hadamard_bf16_f16(
    buf: UnsafePointer[Float16, MutAnyOrigin],
    width: Int,
    n_rows: Int,
):
    """Transformata Walsha-Hadamarda po wierszu, znormalizowana `1/sqrt(width)`,
    z wynikiem zaokrąglonym do bf16.

    Zaokrąglenie NIE jest kosmetyczne: referencja trzyma ten tensor w bf16, a
    przesunięcie maksimum grupy przez granicę potęgi dwójki zmienia skalę całej
    grupy przy następującej po tym kwantyzacji FP4.
    """
    row = Int(block_idx.x)
    if row >= n_rows:
        return
    tid = Int(thread_idx.x)
    nthreads = Int(block_dim.x)
    shared = stack_allocation[
        DS_MAX_HEAD, Float32, address_space = AddressSpace.SHARED
    ]()
    var i = tid
    while i < width:
        shared[i] = Float32(buf[row * width + i])
        i += nthreads
    barrier()

    var step = 1
    while step < width:
        var j = tid
        while j < width // 2:
            block = j // step
            inner = j % step
            lo = block * 2 * step + inner
            hi = lo + step
            a = shared[lo]
            b = shared[hi]
            shared[lo] = a + b
            shared[hi] = a - b
            j += nthreads
        barrier()
        step *= 2

    scale = rsqrt(Float32(width))
    i = tid
    while i < width:
        v = shared[i] * scale
        # Zaokrąglenie do bf16 przez obcięcie mantysy z zaokrągleniem do
        # najbliższej parzystej.
        bits = bitcast[DType.uint32, 1](v)[0]
        rounded = bits + ((bits >> 16) & 1) + 0x7FFF
        buf[row * width + i] = Float16(
            bitcast[DType.float32, 1](rounded & 0xFFFF0000)[0]
        )
        i += nthreads


def act_quant_fp8_f16(
    buf: UnsafePointer[Float16, MutAnyOrigin],
    row_stride: Int,
    offset: Int,
    span: Int,
    block: Int,
    n_rows: Int,
):
    """Symulacja kwantyzacji aktywacji do FP8 E4M3 ze skalą zaokrągloną do
    potęgi dwójki (`ue8m0`), w miejscu.

    Model przeszedł z tym trening (QAT), więc pominięcie kroku zmienia wartości
    trafiające do cache'u KV — to nie jest opcjonalna optymalizacja.
    """
    idx = Int(global_idx.x)
    groups = span // block
    if idx >= n_rows * groups:
        return
    row = idx // groups
    group = idx % groups
    base = row * row_stride + offset + group * block

    var amax: Float32 = 1e-4
    var i = 0
    while i < block:
        v = abs(Float32(buf[base + i]))
        if v > amax:
            amax = v
        i += 1
    scale = _pow2i(_log2_ceil(amax / FP8_MAX))
    inv = 1.0 / scale
    i = 0
    while i < block:
        buf[base + i] = Float16(
            _round_e4m3(Float32(buf[base + i]) * inv) * scale
        )
        i += 1


def act_quant_fp4_f16(
    buf: UnsafePointer[Float16, MutAnyOrigin],
    row_stride: Int,
    span: Int,
    block: Int,
    n_rows: Int,
):
    """Ta sama symulacja, ale do FP4 E2M1 — używana przez indekser."""
    idx = Int(global_idx.x)
    groups = span // block
    if idx >= n_rows * groups:
        return
    row = idx // groups
    group = idx % groups
    base = row * row_stride + group * block

    var amax: Float32 = 6.0 * 5.877472e-39
    var i = 0
    while i < block:
        v = abs(Float32(buf[base + i]))
        if v > amax:
            amax = v
        i += 1
    scale = _pow2i(_log2_ceil(amax / FP4_MAX))
    inv = 1.0 / scale
    i = 0
    while i < block:
        x = Float32(buf[base + i]) * inv
        if x > FP4_MAX:
            x = FP4_MAX
        if x < -FP4_MAX:
            x = -FP4_MAX
        buf[base + i] = Float16(_round_e2m1(x) * scale)
        i += 1


def _pow2i(exponent: Int32) -> Float32:
    """`2^exponent` przez złożenie wykładnika IEEE 754 — dokładne i bez log/exp,
    tak jak w kernelach autorów modelu."""
    return bitcast[DType.float32, 1]((exponent + 127).cast[DType.uint32]() << 23)[0]


def _log2_ceil(x: Float32) -> Int32:
    """`ceil(log2(x))` z pól liczby zmiennoprzecinkowej."""
    bits = bitcast[DType.uint32, 1](x)[0]
    exponent = ((bits >> 23) & 0xFF).cast[DType.int32]() - 127
    mantissa = bits & 0x7FFFFF
    if mantissa != 0:
        exponent += 1
    return exponent


def _round_e4m3(x: Float32) -> Float32:
    """Najbliższa wartość reprezentowalna w E4M3 (bez NaN, z nasyceniem)."""
    if x != x:
        return 0.0
    var v = x
    if v > FP8_MAX:
        v = FP8_MAX
    if v < -FP8_MAX:
        v = -FP8_MAX
    sign: Float32 = 1.0
    if v < 0.0:
        sign = -1.0
        v = -v
    if v < 0.0009765625:  # połowa najmniejszej subnormalnej E4M3
        return 0.0
    # Wykładnik binade, przycięty do zakresu normalnego E4M3.
    var e = _log2_ceil(v)
    bits = bitcast[DType.uint32, 1](v)[0]
    if (bits & 0x7FFFFF) == 0:
        e = ((bits >> 23) & 0xFF).cast[DType.int32]() - 127
    else:
        e -= 1
    if e < -6:
        e = -6
    step = _pow2i(e - 3)
    q = _round_half_even(v / step) * step
    if q > FP8_MAX:
        q = FP8_MAX
    return sign * q


def _round_half_even(x: Float32) -> Float32:
    down = Float32(Int(x))
    frac = x - down
    if frac > 0.5:
        return down + 1.0
    if frac < 0.5:
        return down
    if (Int(down) % 2) == 0:
        return down
    return down + 1.0


def _round_e2m1(x: Float32) -> Float32:
    """Najbliższa wartość z ośmioelementowego kodu E2M1."""
    sign: Float32 = 1.0
    var v = x
    if v < 0.0:
        sign = -1.0
        v = -v
    # Ośmioelementowy kod E2M1; progi to środki między sąsiednimi wartościami.
    var q: Float32 = 6.0
    if v < 0.25:
        q = 0.0
    elif v < 0.75:
        q = 0.5
    elif v < 1.25:
        q = 1.0
    elif v < 1.75:
        q = 1.5
    elif v < 2.5:
        q = 2.0
    elif v < 3.5:
        q = 3.0
    elif v < 5.0:
        q = 4.0
    return sign * q
