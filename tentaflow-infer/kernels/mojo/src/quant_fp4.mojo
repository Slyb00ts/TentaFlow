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
from std.memory import UnsafePointer, stack_allocation
from std.sys import _RegisterPackType
from std.sys._assembly import inlined_assembly

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
def _to_e2m1(value: Float32) -> UInt32:
    """Kod e2m1 najbliższy `value`; wartość spoza zakresu przycina się do 6."""
    a = abs(value)
    var code: UInt32 = 0
    if a >= 5.0:
        code = 7
    elif a >= 3.5:
        code = 6
    elif a >= 2.5:
        code = 5
    elif a >= 1.75:
        code = 4
    elif a >= 1.25:
        code = 3
    elif a >= 0.75:
        code = 2
    elif a >= 0.25:
        code = 1
    if value < 0.0:
        code |= 0x8
    return code


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
