# =============================================================================
# Plik: build_gemm_q8_triplet_variants.mojo
# Opis: Kompiluje izolowane warianty tripletu Q8 do osobnych plików PTX.
# Przykład: pixi run mojo build_gemm_q8_triplet_variants.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.gemm_q8_triplet_variants import (
    gemm_q8_0_i8mma_triplet_single_bm64,
    gemm_q8_0_i8mma_triplet_single_big,
)


def main() raises:
    var ctx = DeviceContext()
    out_dir = Path("build") / ctx.arch_name()
    os.makedirs(String(out_dir), exist_ok=True)
    name64 = "gemm_q8_0_i8mma_triplet_single_bm64"
    temporary64 = Path(name64 + ".ptx")
    final64 = out_dir / (name64 + ".ptx")
    _ = ctx.compile_function[
        gemm_q8_0_i8mma_triplet_single_bm64,
        dump_asm=Path("gemm_q8_0_i8mma_triplet_single_bm64.ptx"),
    ]()
    final64.write_text(temporary64.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(temporary64))
    print("skompilowano", name64)

    name_big = "gemm_q8_0_i8mma_triplet_single_big"
    temporary_big = Path(name_big + ".ptx")
    final_big = out_dir / (name_big + ".ptx")
    _ = ctx.compile_function[
        gemm_q8_0_i8mma_triplet_single_big,
        dump_asm=Path("gemm_q8_0_i8mma_triplet_single_big.ptx"),
    ]()
    final_big.write_text(temporary_big.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(temporary_big))
    print("skompilowano", name_big)
