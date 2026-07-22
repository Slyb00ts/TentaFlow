# =============================================================================
# Plik: build_nvfp4_gguf.mojo
# Opis: Kompiluje odizolowany kernel GEMV dla surowego formatu GGUF NVFP4.
# Przykład: pixi run mojo build_nvfp4_gguf.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.nvfp4 import gemv_nvfp4_gguf_f16


def _entry_from_ptx(ptx_path: Path) raises -> String:
    text = ptx_path.read_text()
    marker = ".visible .entry "
    i = text.find(marker)
    if i < 0:
        raise Error("brak wpisu .visible .entry w " + String(ptx_path))
    j = text.find("(", i)
    if j < 0:
        raise Error("niepoprawny wpis kernela w " + String(ptx_path))
    return String(text[byte = i + marker.byte_length():j])


def main() raises:
    var ctx = DeviceContext()
    arch = ctx.arch_name()
    out_dir = Path("build") / arch
    os.makedirs(String(out_dir), exist_ok=True)
    tmp = Path("gemv_nvfp4_gguf_f16.ptx")
    final = out_dir / "gemv_nvfp4_gguf_f16.ptx"
    _ = ctx.compile_function[gemv_nvfp4_gguf_f16, dump_asm=Path("gemv_nvfp4_gguf_f16.ptx")]()
    final.write_text(tmp.read_text())
    os.remove(String(tmp))
    print(_entry_from_ptx(final))
