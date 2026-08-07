# ===== File: quant_fp4.mojo — aktywacje f16 do bloków NVFP4 =====
# Blokowo-skalowane MMA żąda CZTERECH BITÓW PO OBU STRONACH, więc ścieżka FP4
# stoi na kwantyzacji aktywacji — inaczej niż ścieżka FP8, gdzie przepakowaniu
# podlegała tylko waga. To jest ta cena i widać ją w bramce dokładności.
#
# Skala jest dwupoziomowa, jak w każdej implementacji NVFP4: jeden mnożnik f32
# na TOKEN plus skala UE4M3 na każdą szesnastkę. Bez tego pierwszego skale
# bloków wypadałyby poza zakres E4M3 dla wszystkiego poniżej 2^-9, a warstwy
# wejściowe transformera potrafią mieć aktywacje o kilka rzędów mniejsze niż
# maksimum tokena.
#
# Układ bajtów jest DOKŁADNIE ten sam co w bloku GGUF NVFP4 (bajt `j` niesie
# element `j` w młodszym półbajcie i `j + 8` w starszym), żeby `gemm_fp4.mojo`
# czytało oba operandy tym samym adresowaniem. Przestawienie `k` wewnątrz
# szesnastki nie ma znaczenia, dopóki jest to samo po obu stronach.

from std.gpu import block_dim, block_idx, thread_idx
from std.gpu.sync import barrier
from std.gpu.memory import AddressSpace
from std.memory import UnsafePointer, bitcast, stack_allocation
from std.sys import _RegisterPackType
from std.sys._assembly import inlined_assembly

from std.gpu import global_idx
from src.reduce import block_reduce_max
from src.nvfp4_gguf_batch import _ue4m3_branchless

comptime GROUP = 16
"""Elementów na jedną skalę UE4M3."""

comptime BLOCK_VALUES = 64
comptime BLOCK_BYTES = 36

comptime E2M1_MAX = 6.0
comptime E4M3_MAX = 448.0


@always_inline
def _to_e4m3(value: Float32) -> UInt32:
    """Jeden bajt E4M3 z zaokrągleniem do najbliższego, przez jednostkę konwersji.

    Ręczne składanie bitów dawałoby inne zaokrąglenie niż to, którym karta potem
    ODCZYTUJE skalę w instrukcji — a rozjazd o jeden ULP skali to rozjazd o jeden
    ULP na wszystkich szesnastu wartościach naraz.
    """
    var r = inlined_assembly[
        "cvt.rn.satfinite.e4m3x2.f32 $0, $1, $2;",
        _RegisterPackType[UInt16],
        constraints="=h,f,f",
        has_side_effect=False,
    ](Float32(0.0), value)
    return UInt32(r[0]) & 0xFF


@always_inline
def _to_e2m1_mag(a: Float32) -> UInt32:
    """Kod e2m1 najbliższy `a >= 0`, bez bitu znaku.

    ILE PROGÓW przekroczyła wartość, a nie łańcuch siedmiu gałęzi: kwantyzacja
    aktywacji woła to trzy razy na wartość, więc rozgałęzienie liczyło się tu
    bardziej niż porównanie. Nieskończoność przechodzi wszystkie siedem, czyli
    przycina się do szóstki tak samo jak wcześniej, a NaN nie przechodzi
    żadnego.
    """
    return (
        UInt32(Int(a >= 0.25))
        + UInt32(Int(a >= 0.75))
        + UInt32(Int(a >= 1.25))
        + UInt32(Int(a >= 1.75))
        + UInt32(Int(a >= 2.5))
        + UInt32(Int(a >= 3.5))
        + UInt32(Int(a >= 5.0))
    )


@always_inline
def _to_e2m1(value: Float32) -> UInt32:
    """Kod e2m1 najbliższy `value`; wartość spoza zakresu przycina się do 6."""
    code = _to_e2m1_mag(abs(value))
    return code | 0x8 if value < 0.0 else code


@always_inline
def _e2m1_mag_value(code: UInt32) -> Float32:
    """Liczba, którą instrukcja odczyta z tego półbajtu bez znaku.

    Składana z bitów, a nie czytana z tablicy: indeks tablicy jest tu wartością
    z rejestru, więc kompilator odkłada ją do pamięci lokalnej i każdy odczyt
    schodzi poza multiprocesor.

    Wykładnik e2m1 to `code >> 1` przy odchyleniu jeden, czyli `+126` w f32;
    mantysa istnieje dopiero od `code >= 2`, bo `0,5` jest tu postacią
    zdenormalizowaną.
    """
    e = code >> 1
    mant = (code & 1) & ((e + 3) >> 2)
    bits = ((e + 126) << 23) | (mant << 22)
    nonzero = UInt32(0) - ((code + 7) >> 3)
    return bitcast[DType.float32, 1](bits & nonzero)


def quantize_act_nvfp4(
    xq: UnsafePointer[UInt8, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
):
    """Jeden blok na token: `x[t, :]` do bloków NVFP4 plus mnożnik `xs[t]`.

    `xs[t]` jest tak dobrane, żeby największa szesnastka tokena dostała skalę
    równą maksimum E4M3 — czyli żeby cały zakres E4M3 pracował, zamiast siedzieć
    przy górnej granicy dla jednego bloku i wpadać w denormale dla reszty.
    """
    token = Int(block_idx.x)
    tid = Int(thread_idx.x)
    threads = Int(block_dim.x)
    row = x + token * n_cols

    var local: Float32 = 0.0
    var i = tid
    while i < n_cols:
        local = max(local, abs(Float32(row[i])))
        i += threads
    amax = block_reduce_max(local)

    gs = stack_allocation[1, Float32, address_space = AddressSpace.SHARED]()
    if tid == 0:
        gs[0] = (
            amax / (E2M1_MAX * E4M3_MAX) if amax > 0.0 else Float32(1.0)
        )
        xs[token] = gs[0]
    barrier()
    scale = gs[0]

    groups = n_cols // GROUP
    out = xq + token * (n_cols // BLOCK_VALUES) * BLOCK_BYTES
    var group = tid
    while group < groups:
        base = group * GROUP
        var gmax: Float32 = 0.0
        for j in range(GROUP):
            gmax = max(gmax, abs(Float32(row[base + j])))
        code = _to_e4m3(gmax / (E2M1_MAX * scale))
        # Ta sama liczba, którą instrukcja zobaczy — nie ta, którą chcieliśmy.
        step = _ue4m3_branchless(UInt8(code)) * scale
        inv = 1.0 / step if step > 0.0 else Float32(0.0)

        block = group // 4
        sub = group % 4
        out[block * BLOCK_BYTES + sub] = UInt8(code)
        packed = out + block * BLOCK_BYTES + 4 + sub * 8
        for j in range(8):
            lo = _to_e2m1(Float32(row[base + j]) * inv)
            hi = _to_e2m1(Float32(row[base + j + 8]) * inv)
            packed[j] = UInt8(lo | (hi << 4))
        group += threads


comptime MXFP4_BLOCK = 32


@always_inline
def _to_e8m0_ceil(value: Float32) -> UInt32:
    """Najmniejsza potęga dwójki nie mniejsza niż `value`, jako bajt UE8M0."""
    if not (value > 0.0):
        return 127
    bits = UnsafePointer(to=value).bitcast[UInt32]()[0]
    var e = Int((bits >> 23) & 0xFF)
    if (bits & 0x007FFFFF) != 0:
        e += 1
    if e < 1:
        e = 1
    if e > 254:
        e = 254
    return UInt32(e)


def quantize_act_mxf4(
    xq: UnsafePointer[UInt8, MutAnyOrigin],
    xs: UnsafePointer[Float32, MutAnyOrigin],
    x: UnsafePointer[Float16, MutAnyOrigin],
    n_cols: Int,
):
    """Aktywacje f16 do postaci MXFP4, w układzie bajtów `pack_mxfp4_mma`.

    Bajt `j` szesnastobajtowego ładunku niesie element `j` w młodszym półbajcie
    i `j + 16` w starszym — konwencja GGML-owego MXFP4, nie ta z NVFP4. Liczy
    się tylko to, żeby OBA operandy przestawiały `k` jednakowo, a waga tu jest
    przepakowanym MXFP4.

    `xs[t]` jest jednością i nie ma tu drugiego poziomu skali, bo UE8M0 pokrywa
    `2^-127..2^127`: nie ma czego ratować mnożnikiem na token. Bufor zostaje,
    bo kafel jest wspólny z postacią NVFP4, w której E4M3 sięga tylko `2^8`.
    """
    token = Int(block_idx.x)
    tid = Int(thread_idx.x)
    threads = Int(block_dim.x)
    row = x + token * n_cols
    if tid == 0:
        xs[token] = 1.0

    blocks = n_cols // MXFP4_BLOCK
    dst = xq + token * (n_cols // BLOCK_VALUES) * BLOCK_BYTES
    var b = tid
    while b < blocks:
        base = b * MXFP4_BLOCK
        # Blok wchodzi JEDNYM ładunkiem wektorowym. Trzy przebiegi po trzydzieści
        # dwa ładunki skalarne kosztowały tu więcej niż cała reszta kernela:
        # sąsiednie linie fali dzieli trzydzieści dwie wartości, więc każdy z
        # nich ciągnął własny sektor i przejście szło 30 GB/s.
        vv = (row + base).load[width=MXFP4_BLOCK, alignment=64]().cast[
            DType.float32
        ]()
        av = abs(vv)
        amax = av.reduce_max()
        # Skala jest POTĘGĄ DWÓJKI, bo UE8M0 nie ma mantysy, więc zaokrąglenie
        # w górę marnuje do jednego bitu z dwóch, które ma e2m1 — a zaokrąglenie
        # w dół przycina szczyt bloku do 6. Który z dwóch kandydatów jest lepszy,
        # zależy od ROZKŁADU w bloku, nie od jego maksimum, więc wybiera go błąd
        # kwadratowy policzony dla obu. Trzydzieści dwie wartości dwa razy to
        # cena, której w profilu nie widać: to przejście czyta aktywację raz, a
        # mnożenie czyta wagę eksperta.
        var code = _to_e8m0_ceil(amax / E2M1_MAX)
        if code > 1:
            var err_hi: Float32 = 0.0
            var err_lo: Float32 = 0.0
            hi_step = _e8m0(UInt8(code))
            lo_step = _e8m0(UInt8(code - 1))
            hi_inv = 1.0 / hi_step
            lo_inv = 1.0 / lo_step
            # Na module, nie na wartości ze znakiem: rekonstrukcja niesie znak
            # wejścia, więc różnica jest co do bitu przeciwna, a jej kwadrat ten
            # sam — a moduł oszczędza bit znaku w obu wywołaniach.
            comptime for j in range(MXFP4_BLOCK):
                a = av[j]
                d_hi = a - _e2m1_mag_value(_to_e2m1_mag(a * hi_inv)) * hi_step
                d_lo = a - _e2m1_mag_value(_to_e2m1_mag(a * lo_inv)) * lo_step
                err_hi += d_hi * d_hi
                err_lo += d_lo * d_lo
            if err_lo < err_hi:
                code -= 1
        step = _e8m0(UInt8(code))
        inv = 1.0 / step if step > 0.0 else Float32(0.0)

        pair = b // 2
        half = b % 2
        dst[pair * BLOCK_BYTES + half] = UInt8(code)
        bytes_out = dst + pair * BLOCK_BYTES + 4 + half * 16
        comptime for j in range(16):
            lo = _to_e2m1(vv[j] * inv)
            hi = _to_e2m1(vv[j + 16] * inv)
            bytes_out[j] = UInt8(lo | (hi << 4))
        b += threads


@always_inline
def _e8m0(code: UInt8) -> Float32:
    """`2^(code - 127)`, czyli to, co instrukcja czyta z bajtu skali."""
    var bits: UInt32
    if code == 0:
        bits = 0
    else:
        bits = UInt32(code) << 23
    return UnsafePointer(to=bits).bitcast[Float32]()[0]
