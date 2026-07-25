# =============================================================================
# Plik: arch_dot.mojo
# Opis: Jedno źródło iloczynu skalarnego int8 dla wszystkich architektur GPU.
#       Na NVIDII to `llvm.nvvm.idp4a.s.s`, na AMD instrukcja `v_dot4_i32_i8`
#       (RDNA2+ / CDNA). Kernele dp4a wołają wyłącznie ten helper, więc rodzina
#       przenosi się na nowe karty bez dotykania ich pętli wewnętrznych.
# Przykład: acc = dot4_i8(codes, activations, acc)
# =============================================================================

from std.sys.info import _accelerator_arch
from std.sys.intrinsics import llvm_intrinsic
from std.gpu.intrinsics import inlined_assembly


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
