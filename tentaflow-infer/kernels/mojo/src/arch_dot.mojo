# =============================================================================
# Plik: arch_dot.mojo
# Opis: Jedno źródło prymitywów zależnych od architektury GPU — iloczynów
#       skalarnych dot (int8, f16) i dekodowania float8. Kernele wołają wyłącznie
#       te helpery, więc rodziny przenoszą się na nowe karty bez dotykania ich
#       pętli wewnętrznych.
# Przykład: acc = dot4_i8(codes, activations, acc)
# =============================================================================

from std.sys.info import _accelerator_arch
from std.sys.intrinsics import llvm_intrinsic
from std.gpu.intrinsics import inlined_assembly
from std.memory import bitcast
from src.kv_fp8 import _e4m3x2_to_f16x2


@always_inline
def dot4_i8(a: Int32, b: Int32, c: Int32) -> Int32:
    """c + suma iloczynów czterech par bajtów ze znakiem z `a` i `b`.

    Jedna instrukcja sprzętowa na obu rodzinach: `dp4a.s32.s32` na sm_61+
    i `v_dot4_i32_i8` na gfx1030+. Kolejność akumulacji jest identyczna, więc
    wynik jest bitowo ten sam — instrukcje liczą int32 dokładnie.
    """
    # `_accelerator_arch()` opisuje akcelerator, dla którego kernel jest właśnie
    # kompilowany (np. "amdgpu:gfx1030", "nvidia:sm_89"). Nasz builder zawsze
    # kompiluje pod lokalne urządzenie, więc to jest właściwy dyskryminator;
    # przy kompilacji skrośnej trzeba go zastąpić parametrem celu.
    comptime if _accelerator_arch().startswith("amdgpu"):
        return inlined_assembly[
            "v_dot4_i32_i8 $0, $1, $2, $0",
            Int32,
            constraints="=v,v,v,0",
        ](a, b, c)
    else:
        return llvm_intrinsic["llvm.nvvm.idp4a.s.s", Int32](a, b, c)


@always_inline
def dot2_f16(a: SIMD[DType.float16, 2], b: SIMD[DType.float16, 2], c: Float32) -> Float32:
    """c + a[0]*b[0] + a[1]*b[1] z akumulacją w f32.

    Na RDNA2+ to jedna instrukcja `v_dot2_f32_f16` — dwa MAC-i f16 na takt na
    linię wektorową, czyli dwukrotność `v_fma_f32`. To odpowiednik `dot4_i8` dla
    f16 i jedyny sposób, żeby GEMM f16 bez jednostki macierzowej trafił w pułap
    karty zamiast w połowę. Na NVIDII (gdzie f16 idzie przez tensor core) helper
    degraduje do dwóch FMA i służy tylko do porównań referencyjnych.
    """
    comptime if _accelerator_arch().startswith("amdgpu"):
        return inlined_assembly[
            "v_dot2_f32_f16 $0, $1, $2, $0",
            Float32,
            constraints="=v,v,v,0",
        ](a, b, c)
    else:
        return c + Float32(a[0]) * Float32(b[0]) + Float32(a[1]) * Float32(b[1])


@always_inline
def f8e4m3_to_f32(b: UInt8) -> Float32:
    """Dekoduje `float8_e4m3fn` (skala grupy NVFP4) do f32.

    NVIDIA ma na to jedną instrukcję (`cvt.rn.f16x2.e4m3x2`), na pozostałych
    architekturach składamy wzorzec bitowy f32 wprost: wykładnik e4m3 ma bias 7,
    a f32 bias 127, więc normalne wartości to przesunięcie o 120 i mantysa
    przesunięta o 20 bitów. Wartości subnormalne (wykładnik zerowy) to
    `mant * 2^-9`. Kody NaN (0x7F/0xFF) nie występują w skalach NVFP4 i nie są
    tu odwzorowywane — dekodują się jako skończona liczba, tak samo jak w
    dotychczasowej ścieżce.
    """
    comptime if _accelerator_arch().startswith("amdgpu"):
        exponent = Int32((b >> 3) & 0x0F)
        mantissa = Int32(b & 0x07)
        bits = ((exponent + 120) << 23) | (mantissa << 20)
        normal = bitcast[DType.float32](bits.cast[DType.uint32]())
        subnormal = Float32(mantissa) * (1.0 / 512.0)
        magnitude = subnormal if exponent == 0 else normal
        return -magnitude if (b >> 7) != 0 else magnitude
    else:
        return Float32(_e4m3x2_to_f16x2(b, 0)[0])


@always_inline
def nvfp4_codes8(packed: SIMD[DType.uint8, 4]) -> SIMD[DType.int8, 8]:
    """Rozpakowuje osiem wartości e2m1 do PODWOJONYCH kodów całkowitych.

    Wartości e2m1 to 0, +-0.5, +-1, +-1.5, +-2, +-3, +-4, +-6 — same wielokrotności
    0,5, więc po przemnożeniu przez 2 są całkowite (0..12) i mieszczą się w int8.
    Dzięki temu NVFP4 wchodzi na `v_dot4_i32_i8` BEZ straty dokładności; korektę
    o czynnik 2 pochłania skala grupy. Bajt niesie kolumnę parzystą w młodszym
    półbajcie, a nieparzystą w starszym.
    """
    low = packed & 0x0F
    high = packed >> 4
    codes = low.interleave(high)
    magnitude = (codes & 0x07).cast[DType.int32]()
    is_small = ((magnitude - 2) >> 31) & 1
    # 4x magnituda tą samą arytmetyką bez gałęzi co `_e2m1x8`, potem /2.
    quad = is_small * (magnitude * 2) + (1 - is_small) * (
        (2 + (magnitude & 1)) << (magnitude >> 1)
    )
    doubled = quad >> 1
    negative = ((codes >> 3) & 1).cast[DType.int32]()
    return (doubled * (1 - 2 * negative)).cast[DType.int8]()
