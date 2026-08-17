# =============================================================================
# Plik: build_nvfp4_mma_prefetch.mojo
# Opis: Kompiluje odizolowany kernel GEMM NVFP4 z wyprzedzajacym pobieraniem wag.
# Przykład: pixi run mojo build_nvfp4_mma_prefetch.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.nvfp4_gguf_mma import gemm_nvfp4_gguf_mma_f16_bm128_prefetch


def main() raises:
    var ctx = DeviceContext()
    out_dir = Path("build") / ctx.arch_name()
    os.makedirs(String(out_dir), exist_ok=True)
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_mma_f16_bm128_prefetch,
        dump_asm=Path("gemm_nvfp4_gguf_mma_f16_bm128_prefetch.ptx"),
    ]()
    temporary = Path("gemm_nvfp4_gguf_mma_f16_bm128_prefetch.ptx")
    target = out_dir / "gemm_nvfp4_gguf_mma_f16_bm128_prefetch.ptx"
    target.write_text(temporary.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(temporary))
    print("skompilowano gemm_nvfp4_gguf_mma_f16_bm128_prefetch ->", String(target))
