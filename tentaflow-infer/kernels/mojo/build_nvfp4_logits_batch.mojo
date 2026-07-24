# =============================================================================
# Plik: build_nvfp4_logits_batch.mojo
# Opis: Buduje głowy logitów GGUF NVFP4 dla batchy B4, B8 i B16.
# Przykład: pixi run mojo build_nvfp4_logits_batch.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.nvfp4_gguf_batch import gemm_nvfp4_gguf_out_f32_b4, gemm_nvfp4_gguf_out_f32_b8, gemm_nvfp4_gguf_out_f32_b16


def _store(out_dir: Path, name: String) raises:
    source = Path(name + ".ptx")
    target = out_dir / (name + ".ptx")
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))


def main() raises:
    var ctx = DeviceContext()
    out_dir = Path("build") / ctx.arch_name()
    os.makedirs(String(out_dir), exist_ok=True)
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_out_f32_b4,
        dump_asm=Path("gemm_nvfp4_gguf_out_f32_b4.ptx"),
    ]()
    _store(out_dir, "gemm_nvfp4_gguf_out_f32_b4")
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_out_f32_b8,
        dump_asm=Path("gemm_nvfp4_gguf_out_f32_b8.ptx"),
    ]()
    _store(out_dir, "gemm_nvfp4_gguf_out_f32_b8")
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_out_f32_b16,
        dump_asm=Path("gemm_nvfp4_gguf_out_f32_b16.ptx"),
    ]()
    _store(out_dir, "gemm_nvfp4_gguf_out_f32_b16")
