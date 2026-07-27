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


def compressor_pool_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    kv: UnsafePointer[Float32, MutAnyOrigin],
    score: UnsafePointer[Float32, MutAnyOrigin],
    slots: UnsafePointer[Int32, MutAnyOrigin],
    head_dim: Int,
    window: Int,
    n_blocks: Int,
):
    """Bramkowany pooling kompresora KV: softmax po oknie, osobno dla każdego
    wymiaru, i ważona suma wartości.

    Wejścia są w f32, bo referencja liczy kompresję w tej precyzji jawnie:
    wyniki bramki potrafią wyjść poza zakres f16, a wtedy softmax daje NaN
    zamiast rozkładu.

    `slots[block * window + w]` wskazuje wiersz źródłowy w `kv`/`score` dla
    pozycji `w` okna bloku `block`; wartość ujemna oznacza pozycję pustą, która
    ma dostać wagę zero. To przez tę tablicę przechodzi cała logika okien Z
    ZAKŁADKĄ (stopień 4) — kernel nie musi o niej nic wiedzieć, a wariant bez
    zakładki jest tym samym kernelem z inną tablicą.

    Jeden blok na wymiar-blok; redukcja po oknie jest szeregowa, bo okno ma
    najwyżej `2 * ratio` pozycji.
    """
    idx = Int(global_idx.x)
    if idx >= n_blocks * head_dim:
        return
    block = idx // head_dim
    dim = idx % head_dim

    var mx: Float32 = -3.0e38
    var w = 0
    while w < window:
        row = Int(slots[block * window + w])
        if row >= 0:
            v = score[row * head_dim + dim]
            if v > mx:
                mx = v
        w += 1

    var denom: Float32 = 0.0
    var acc: Float32 = 0.0
    w = 0
    while w < window:
        row = Int(slots[block * window + w])
        if row >= 0:
            e = exp(score[row * head_dim + dim] - mx)
            denom += e
            acc += e * kv[row * head_dim + dim]
        w += 1
    out_ptr[block * head_dim + dim] = Float16(acc / denom)


def sparse_attn_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    q: UnsafePointer[Float16, MutAnyOrigin],
    kv: UnsafePointer[Float16, MutAnyOrigin],
    sink: UnsafePointer[Float32, MutAnyOrigin],
    idxs: UnsafePointer[Int32, MutAnyOrigin],
    head_dim: Int,
    n_heads: Int,
    n_idx: Int,
    scale: Float32,
):
    """Uwaga liczona WYŁĄCZNIE po zebranych indeksach, z kotwicą.

    Dwa szczegóły przesądzają o poprawności. Indeks `-1` oznacza pozycję
    zamaskowaną: jej wynik to minus nieskończoność, a wektor wartości zero —
    potraktowanie go jak zwykłego indeksu czyta cudzy wiersz. Kotwica
    (`sink[head]`) wchodzi WYŁĄCZNIE do mianownika softmaxu, jako dodatkowy
    logit o zerowym wektorze wartości.

    Jeden blok na głowicę jednego tokenu; `idxs` jest wspólne dla wszystkich
    głowic tego tokenu.
    """
    head = Int(block_idx.x)
    if head >= n_heads:
        return
    tid = Int(thread_idx.x)
    nthreads = Int(block_dim.x)
    qbase = head * head_dim

    shared = stack_allocation[
        1024, Float32, address_space = AddressSpace.SHARED
    ]()

    # Maksimum wyników — potrzebne przed wykładnikami, żeby softmax był stabilny.
    var local_max: Float32 = -3.0e38
    var k = tid
    while k < n_idx:
        row = Int(idxs[k])
        if row >= 0:
            var dot: Float32 = 0.0
            var d = 0
            while d < head_dim:
                dot += Float32(q[qbase + d]) * Float32(kv[row * head_dim + d])
                d += 1
            s = dot * scale
            if s > local_max:
                local_max = s
        k += nthreads
    shared[tid] = local_max
    barrier()
    var stride = nthreads // 2
    while stride > 0:
        if tid < stride:
            if shared[tid + stride] > shared[tid]:
                shared[tid] = shared[tid + stride]
        barrier()
        stride //= 2
    mx = shared[0]
    barrier()

    # Mianownik: suma wykładników plus kotwica.
    var local_sum: Float32 = 0.0
    k = tid
    while k < n_idx:
        row = Int(idxs[k])
        if row >= 0:
            var dot: Float32 = 0.0
            var d = 0
            while d < head_dim:
                dot += Float32(q[qbase + d]) * Float32(kv[row * head_dim + d])
                d += 1
            local_sum += exp(dot * scale - mx)
        k += nthreads
    shared[tid] = local_sum
    barrier()
    stride = nthreads // 2
    while stride > 0:
        if tid < stride:
            shared[tid] += shared[tid + stride]
        barrier()
        stride //= 2
    denom = shared[0] + exp(sink[head] - mx)
    barrier()

    # Licznik: kotwica NIE wnosi tu nic.
    var d = tid
    while d < head_dim:
        var acc: Float32 = 0.0
        var kk = 0
        while kk < n_idx:
            row = Int(idxs[kk])
            if row >= 0:
                var dot: Float32 = 0.0
                var dd = 0
                while dd < head_dim:
                    dot += Float32(q[qbase + dd]) * Float32(kv[row * head_dim + dd])
                    dd += 1
                acc += exp(dot * scale - mx) * Float32(kv[row * head_dim + d])
            kk += 1
        out_ptr[qbase + d] = Float16(acc / denom)
        d += nthreads


def hc_sinkhorn_f32(
    pre: UnsafePointer[Float32, MutAnyOrigin],
    post: UnsafePointer[Float32, MutAnyOrigin],
    comb: UnsafePointer[Float32, MutAnyOrigin],
    mixes: UnsafePointer[Float32, MutAnyOrigin],
    scale: UnsafePointer[Float32, MutAnyOrigin],
    base: UnsafePointer[Float32, MutAnyOrigin],
    hc: Int,
    iters: Int,
    eps: Float32,
    n_tokens: Int,
):
    """Wagi hyper-connections: rozdziela `mixes` na wagi redukcji, rozprowadzenia
    i macierz mieszającą, tę ostatnią doprowadzając Sinkhornem do postaci
    podwójnie stochastycznej.

    Kolejność normalizacji jest nieoczywista i przesądza o wyniku: po softmaksie
    po wierszach idzie NAJPIERW normalizacja po kolumnach, a dopiero potem
    `iters - 1` pełnych par wiersz+kolumna. Rozpoczęcie od wierszy daje inną
    macierz.

    Jeden wątek na token — `hc` wynosi 4, więc macierz ma 16 elementów i
    zrównoleglanie wewnątrz tokenu nic by nie dało.
    """
    token = Int(global_idx.x)
    if token >= n_tokens:
        return
    mix_hc = (2 + hc) * hc
    mbase = token * mix_hc

    var m = 0
    while m < hc:
        v = mixes[mbase + m] * scale[0] + base[m]
        pre[token * hc + m] = 1.0 / (1.0 + exp(-v)) + eps
        w = mixes[mbase + m + hc] * scale[1] + base[m + hc]
        post[token * hc + m] = 2.0 / (1.0 + exp(-w))
        m += 1

    cbase = token * hc * hc
    var j = 0
    while j < hc:
        var k = 0
        while k < hc:
            at = j * hc + k + 2 * hc
            comb[cbase + j * hc + k] = mixes[mbase + at] * scale[2] + base[at]
            k += 1
        j += 1

    # Softmax po wierszach, z przesunięciem o maksimum wiersza.
    j = 0
    while j < hc:
        var mx: Float32 = -3.0e38
        var k = 0
        while k < hc:
            v = comb[cbase + j * hc + k]
            if v > mx:
                mx = v
            k += 1
        var total: Float32 = 0.0
        k = 0
        while k < hc:
            e = exp(comb[cbase + j * hc + k] - mx)
            comb[cbase + j * hc + k] = e
            total += e
            k += 1
        k = 0
        while k < hc:
            comb[cbase + j * hc + k] = comb[cbase + j * hc + k] / total + eps
            k += 1
        j += 1

    # Najpierw kolumny — dopiero potem pary wiersz+kolumna.
    var k = 0
    while k < hc:
        var total: Float32 = 0.0
        j = 0
        while j < hc:
            total += comb[cbase + j * hc + k]
            j += 1
        j = 0
        while j < hc:
            comb[cbase + j * hc + k] = comb[cbase + j * hc + k] / (total + eps)
            j += 1
        k += 1

    var it = 1
    while it < iters:
        j = 0
        while j < hc:
            var total: Float32 = 0.0
            k = 0
            while k < hc:
                total += comb[cbase + j * hc + k]
                k += 1
            k = 0
            while k < hc:
                comb[cbase + j * hc + k] = comb[cbase + j * hc + k] / (total + eps)
                k += 1
            j += 1
        k = 0
        while k < hc:
            var total: Float32 = 0.0
            j = 0
            while j < hc:
                total += comb[cbase + j * hc + k]
                j += 1
            j = 0
            while j < hc:
                comb[cbase + j * hc + k] = comb[cbase + j * hc + k] / (total + eps)
                j += 1
            k += 1
        it += 1


def hc_reduce_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    pre: UnsafePointer[Float32, MutAnyOrigin],
    dim: Int,
    hc: Int,
    n_tokens: Int,
):
    """Redukcja `hc` kopii strumienia rezydualnego do jednej, ważona `pre`."""
    idx = Int(global_idx.x)
    if idx >= n_tokens * dim:
        return
    token = idx // dim
    d = idx % dim
    var acc: Float32 = 0.0
    var copy = 0
    while copy < hc:
        acc += pre[token * hc + copy] * Float32(x[(token * hc + copy) * dim + d])
        copy += 1
    out_ptr[idx] = Float16(acc)


def hc_expand_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    block_out: UnsafePointer[Float16, MutAnyOrigin],
    residual: UnsafePointer[Float16, MutAnyOrigin],
    post: UnsafePointer[Float32, MutAnyOrigin],
    comb: UnsafePointer[Float32, MutAnyOrigin],
    dim: Int,
    hc: Int,
    n_tokens: Int,
):
    """Rozprowadzenie wyjścia bloku z powrotem na `hc` kopii, z domieszką
    poprzedniego strumienia przez macierz `comb`.

    Uwaga na kierunek indeksowania `comb`: mnożnik kopii wejściowej `i` przy
    kopii wyjściowej `o` to `comb[i * hc + o]`, a nie odwrotnie — transpozycja
    daje poprawny kształt i inny model.
    """
    idx = Int(global_idx.x)
    if idx >= n_tokens * hc * dim:
        return
    d = idx % dim
    out_copy = (idx // dim) % hc
    token = idx // (dim * hc)

    var acc = post[token * hc + out_copy] * Float32(block_out[token * dim + d])
    var in_copy = 0
    while in_copy < hc:
        w = comb[token * hc * hc + in_copy * hc + out_copy]
        acc += w * Float32(residual[(token * hc + in_copy) * dim + d])
        in_copy += 1
    out_ptr[idx] = Float16(acc)


def index_score_f16(
    out_ptr: UnsafePointer[Float16, MutAnyOrigin],
    q: UnsafePointer[Float16, MutAnyOrigin],
    kv: UnsafePointer[Float16, MutAnyOrigin],
    head_w: UnsafePointer[Float16, MutAnyOrigin],
    head_dim: Int,
    n_heads: Int,
    n_blocks: Int,
    n_tokens: Int,
    scale: Float32,
):
    """Punktowanie pozycji przez indekser: `relu(q·k)` ważone per głowica i
    zsumowane po głowicach.

    Prostownik przed ważeniem jest istotny — bez niego ujemne dopasowania
    odejmowałyby się od wyniku i zmieniały ranking pozycji.

    Jeden wątek na parę (token, blok); pętla po głowicach jest szeregowa, bo
    wynik i tak jest sumą po nich.
    """
    idx = Int(global_idx.x)
    if idx >= n_tokens * n_blocks:
        return
    token = idx // n_blocks
    block = idx % n_blocks

    var acc: Float32 = 0.0
    var head = 0
    while head < n_heads:
        qbase = (token * n_heads + head) * head_dim
        kbase = block * head_dim
        var dot: Float32 = 0.0
        var d = 0
        while d < head_dim:
            dot += Float32(q[qbase + d]) * Float32(kv[kbase + d])
            d += 1
        if dot < 0.0:
            dot = 0.0
        acc += dot * Float32(head_w[token * n_heads + head]) * scale
        head += 1
    out_ptr[idx] = Float16(acc)


def compressor_add_ape_f32(
    acc: UnsafePointer[Float32, MutAnyOrigin],
    src: UnsafePointer[Float32, MutAnyOrigin],
    n: Int,
):
    """`acc += src` nad `n` elementami w f32 — kodowanie pozycji kompresora
    (zapisane w checkpoincie jako f32) wchodzi do wyników bramki liczonych w tej
    samej precyzji."""
    i = Int(global_idx.x)
    if i < n:
        acc[i] = acc[i] + src[i]
