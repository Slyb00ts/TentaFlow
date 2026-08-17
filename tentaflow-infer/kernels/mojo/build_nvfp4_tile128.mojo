# =============================================================================
# Plik: build_nvfp4_tile128.mojo
# Opis: Kompiluje produkcyjne kernele układu NVFP4 TileN128K64 do PTX.
# Przykład: pixi run mojo build_nvfp4_tile128.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.nvfp4_tile128_repack import nvfp4_repack_tile128
from src.nvfp4_tile128_decode import gemv_nvfp4_tile128_coop_q8_1_f16
from src.nvfp4_tile128_mma import (
    gemm_nvfp4_tile128_mma_f16_bm128_bn64,
    gemm_nvfp4_tile128_mma_f16_bm128_bn128,
)


def _finish(out_dir: Path, name: StringSlice) raises:
    temporary = Path(String(name) + ".ptx")
    target = out_dir / (String(name) + ".ptx")
    target.write_text(temporary.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(temporary))
    print("skompilowano", name, "->", String(target))


def main() raises:
    var ctx = DeviceContext()
    out_dir = Path("build") / ctx.arch_name()
    os.makedirs(String(out_dir), exist_ok=True)

    _ = ctx.compile_function[
        nvfp4_repack_tile128,
        dump_asm=Path("nvfp4_repack_tile128.ptx"),
    ]()
    _finish(out_dir, "nvfp4_repack_tile128")
    _ = ctx.compile_function[
        gemv_nvfp4_tile128_coop_q8_1_f16,
        dump_asm=Path("gemv_nvfp4_tile128_coop_q8_1_f16.ptx"),
    ]()
    _finish(out_dir, "gemv_nvfp4_tile128_coop_q8_1_f16")
    _ = ctx.compile_function[
        gemm_nvfp4_tile128_mma_f16_bm128_bn64,
        dump_asm=Path("gemm_nvfp4_tile128_mma_f16_bm128_bn64.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_tile128_mma_f16_bm128_bn64")
    _ = ctx.compile_function[
        gemm_nvfp4_tile128_mma_f16_bm128_bn128,
        dump_asm=Path("gemm_nvfp4_tile128_mma_f16_bm128_bn128.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_tile128_mma_f16_bm128_bn128")
