# =============================================================================
# Plik: arch_wmma.mojo
# Opis: Jedno źródło prymitywu macierzowego 16x16x16 — WMMA na RDNA3+ i `mma`
#       na NVIDII. Kernele wołają wyłącznie te helpery, żeby kafle GEMM
#       przenosiły się między rodzinami bez przepisywania pętli wewnętrznej.
# Przykład: acc = wmma_i8_16x16x16(a_frag, b_frag, acc)
# =============================================================================

from std.sys.info import _accelerator_arch
from std.sys.intrinsics import llvm_intrinsic
from std.gpu import thread_idx


# RDNA3 a RDNA4: ta sama operacja 16x16x16, INNY rozmiar fragmentu na linię.
#
# RDNA3 wymaga, żeby każda linia niosła CAŁY wiersz 16 wartości, a obie połowy
# fali trzymały to samo — 32 linie x 16 = 512 wartości na macierz 256-elementową,
# czyli dwukrotne zdublowanie. RDNA4 dublowanie usunęło: linia niesie 8 wartości,
# 32 x 8 = 256 dokładnie. Fragment RDNA4 jest więc POŁÓWKĄ fragmentu RDNA3 —
# tą, którą wskazuje numer połowy fali. Dzięki temu kernele mogą ładować dane
# raz, po staremu, a wybór połowy siedzi w tym jednym miejscu.
@always_inline
def _wave_half() -> Int:
    return Int(thread_idx.x) % 32 // 16


@always_inline
def wmma_acc_row(lane: Int, i: Int) -> Int:
    """Wiersz kafla 16x16, ktory niesie `i`-ty akumulator danej linii.

    UKLAD AKUMULATORA ROZNI SIE MIEDZY RDNA3 A RDNA4 i to jest jedyna roznica,
    ktorej nie da sie schowac w samym prymitywie. RDNA3 przeplata wiersze co
    drugi (`i*2 + polowa fali`), RDNA4 daje kazdej polowie fali osiem KOLEJNYCH
    wierszy (`8*polowa + i`). Kolumna jest w obu ta sama: `lane % 16`.

    Zmierzone na karcie sonda `probe_wmma_layout.mojo`, a nie przyjete z
    dokumentacji — pierwsza wersja zakladala uklad RDNA3 i test zloty pokazal
    blad wzgledny 42.
    """
    comptime if _accelerator_arch().startswith("amdgpu:gfx12"):
        return 8 * (lane // 16) + i
    else:
        return wmma_acc_row(lane, i)


@always_inline
def wmma_i8_16x16x16[preselected: Bool = False](
    a: SIMD[DType.int32, 4],
    b: SIMD[DType.int32, 4],
    c: SIMD[DType.int32, 8],
) -> SIMD[DType.int32, 8]:
    """Kafel 16x16x16 int8 ze znakiem, akumulacja int32, jedna instrukcja.

    Fragment `a` i `b` to po 16 bajtów na linię wektorową (wave32 powtarza
    wiersz na dwóch połówkach fali), wynik to 8 akumulatorów int32 na linię.

    Dwa pierwsze argumenty `i1` instrukcji wybierają interpretację ZE ZNAKIEM —
    ta sama pułapka co w `dot4_i8`: bez nich RDNA3 policzyłaby bez znaku i
    zwróciła śmieci na ujemnych bajtach, nie zgłaszając niczego.
    """
    comptime if _accelerator_arch().startswith("amdgpu:gfx12"):
        # `preselected` mowi, ze wolajacy ZALADOWAL juz tylko swoja polowke (tak
        # robi `_row_frag_i8` w gemm_wmma, zeby nie czytac dwa razy wiecej niz
        # RDNA4 potrzebuje). Pozostali podaja pelny fragment RDNA3 i polowke
        # wybieramy tutaj.
        var half = 0 if preselected else _wave_half()
        var a8 = a.slice[2, offset=2]() if half == 1 else a.slice[2, offset=0]()
        var b8 = b.slice[2, offset=2]() if half == 1 else b.slice[2, offset=0]()
        return llvm_intrinsic[
            "llvm.amdgcn.wmma.i32.16x16x16.iu8.v8i32.v2i32", SIMD[DType.int32, 8]
        ](True, a8, True, b8, c, False)
    else:
        return llvm_intrinsic[
            "llvm.amdgcn.wmma.i32.16x16x16.iu8.v8i32.v4i32", SIMD[DType.int32, 8]
        ](True, a, True, b, c, False)


@always_inline
def wmma_f16_16x16x16(
    a: SIMD[DType.float16, 16],
    b: SIMD[DType.float16, 16],
    c: SIMD[DType.float32, 8],
) -> SIMD[DType.float32, 8]:
    """Kafel 16x16x16 f16 z akumulacją f32, jedna instrukcja.

    Przyjmuje fragment W UKŁADZIE RDNA3 (cały wiersz 16 wartości). Na RDNA4
    wybiera z niego połowę należącą do tej linii i woła wariant natywny —
    kernele pisane pod RDNA4 podają swoją połowę wprost przez
    `wmma_f16_16x16x16_native` i nie czytają dwa razy więcej, niż potrzebują.
    """
    comptime if _accelerator_arch().startswith("amdgpu:gfx12"):
        var half = _wave_half()
        var a8 = a.slice[8, offset=8]() if half == 1 else a.slice[8, offset=0]()
        var b8 = b.slice[8, offset=8]() if half == 1 else b.slice[8, offset=0]()
        return wmma_f16_16x16x16_native(a8, b8, c)
    else:
        return llvm_intrinsic[
            "llvm.amdgcn.wmma.f32.16x16x16.f16.v8f32.v16f16", SIMD[DType.float32, 8]
        ](a, b, c)


@always_inline
def wmma_fp8_16x16x16(
    a: SIMD[DType.int32, 2],
    b: SIMD[DType.int32, 2],
    c: SIMD[DType.float32, 8],
) -> SIMD[DType.float32, 8]:
    """Kafel 16x16x16 FP8 (E4M3) z akumulacja f32, jedna instrukcja RDNA4.

    Fragment to osiem bajtow FP8 na linie, spakowanych w dwa int32 — ten sam
    rozmiar co `iu8` na RDNA4, wiec kafel `gemm_wmma.mojo` przenosi sie tu bez
    zmiany geometrii; rozni sie wylacznie prymityw i skalowanie.

    ISTNIEJE TYLKO NA RDNA4. NVIDIA liczy FP8 kaflem `m16n8k32` z `ld_matrix`,
    czyli INNA geometria i inny sposob ladowania fragmentow — tego nie da sie
    schowac za `comptime if` w jednym kernelu i dlatego `gemm_fp8.mojo` zostaje
    kernelem NVIDII, a RDNA4 potrzebuje wlasnego, zbudowanego na tym helperze.
    """
    comptime if _accelerator_arch().startswith("amdgpu:gfx12"):
        return llvm_intrinsic[
            "llvm.amdgcn.wmma.f32.16x16x16.fp8.fp8.v8f32.v2i32",
            SIMD[DType.float32, 8],
        ](a, b, c)
    else:
        # Poza RDNA4 tej instrukcji NIE MA. Kernel, ktory tu trafil, jest zle
        # zakresowany w katalogu — zwracamy akumulator bez zmian, zeby bledny
        # zakres wyszedl jako rozjazd z referencja w tescie zlotym, a nie jako
        # cichy wynik z niewiadomej instrukcji.
        return c


@always_inline
def wmma_f16_16x16x16_native(
    a: SIMD[DType.float16, 8],
    b: SIMD[DType.float16, 8],
    c: SIMD[DType.float32, 8],
) -> SIMD[DType.float32, 8]:
    """Kafel 16x16x16 f16 we WŁASNYM rozmiarze fragmentu RDNA4 (osiem wartości).

    ISTNIEJE TYLKO NA RDNA4 — RDNA3 wymaga szesnastu i ma inny wariant
    intrinsika, wiec kernel wolajacy to wejscie musi byc zakresowany na gfx12.
    """
    comptime if _accelerator_arch().startswith("amdgpu:gfx12"):
        return llvm_intrinsic[
            "llvm.amdgcn.wmma.f32.16x16x16.f16.v8f32.v8f16",
            SIMD[DType.float32, 8],
        ](a, b, c)
    else:
        return c

@always_inline
def wmma_iu4_16x16x32(
    a: SIMD[DType.int32, 2],
    b: SIMD[DType.int32, 2],
    c: SIMD[DType.int32, 8],
) -> SIMD[DType.int32, 8]:
    """Kafel 16x16x32 int4 ze znakiem, akumulacja int32, jedna instrukcja RDNA4.

    DWA RAZY WIĘCEJ K NA INSTRUKCJĘ niż `iu8`, a fragment ma ten sam rozmiar
    (osiem bajtów na linię, czyli 32 czterobitowe kody). Dla wag czterobitowych
    (Q4_K, Q4_0) znika też rozpakowanie do int8.

    K=32 to DOKŁADNIE jeden podblok skali Q4_K — dlatego akumulator int32 zrzuca
    się do f32 raz na instrukcję, a nie raz na dwie jak przy `iu8`.

    Dwa argumenty `i1` wybierają interpretację ZE ZNAKIEM — ta sama pułapka co w
    `dot4_i8` i `wmma_i8`: bez nich instrukcja liczy bez znaku i cicho zwraca
    śmieci na ujemnych kodach.

    ISTNIEJE TYLKO NA RDNA4.
    """
    comptime if _accelerator_arch().startswith("amdgpu:gfx12"):
        return llvm_intrinsic[
            "llvm.amdgcn.wmma.i32.16x16x32.iu4.v8i32.v2i32", SIMD[DType.int32, 8]
        ](True, a, True, b, c, False)
    else:
        # Poza RDNA4 tej instrukcji NIE MA. Kernel, ktory tu trafil, jest zle
        # zakresowany w katalogu — zwracamy akumulator bez zmian, zeby bledny
        # zakres wyszedl jako rozjazd z referencja w tescie zlotym.
        return c
