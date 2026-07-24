# =============================================================================
# Plik: build_gemm_i8mma.mojo
# Opis: Kompiluje odizolowany kernel trzech projekcji Q8_0 z aktywacją Q8_1.
# Przykład: pixi run mojo build_gemm_i8mma.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.gemm import gemm_q8_0_i8mma_triplet_bm64


def main() raises:
    var ctx = DeviceContext()
    out_dir = Path("build") / ctx.arch_name()
    os.makedirs(String(out_dir), exist_ok=True)
    _ = ctx.compile_function[
        gemm_q8_0_i8mma_triplet_bm64,
        dump_asm=Path("gemm_q8_0_i8mma_triplet_bm64.ptx"),
    ]()
    temporary = Path("gemm_q8_0_i8mma_triplet_bm64.ptx")
    target = out_dir / "gemm_q8_0_i8mma_triplet_bm64.ptx"
    target.write_text(temporary.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(temporary))
    print("skompilowano gemm_q8_0_i8mma_triplet_bm64 ->", String(target))
