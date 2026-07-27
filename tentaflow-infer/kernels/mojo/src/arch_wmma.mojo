# =============================================================================
# Plik: arch_wmma.mojo
# Opis: Jedno źródło prymitywu macierzowego 16x16x16 — WMMA na RDNA3+ i `mma`
#       na NVIDII. Kernele wołają wyłącznie te helpery, żeby kafle GEMM
#       przenosiły się między rodzinami bez przepisywania pętli wewnętrznej.
# Przykład: acc = wmma_i8_16x16x16(a_frag, b_frag, acc)
# =============================================================================

from std.sys.intrinsics import llvm_intrinsic


@always_inline
def wmma_i8_16x16x16(
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
    return llvm_intrinsic[
        "llvm.amdgcn.wmma.i32.16x16x16.iu8.v8i32.v4i32", SIMD[DType.int32, 8]
    ](True, a, True, b, c, False)


@always_inline
def wmma_f16_16x16x16(
    a: SIMD[DType.float16, 16],
    b: SIMD[DType.float16, 16],
    c: SIMD[DType.float32, 8],
) -> SIMD[DType.float32, 8]:
    """Kafel 16x16x16 f16 z akumulacją f32, jedna instrukcja."""
    return llvm_intrinsic[
        "llvm.amdgcn.wmma.f32.16x16x16.f16.v8f32.v16f16", SIMD[DType.float32, 8]
    ](a, b, c)
