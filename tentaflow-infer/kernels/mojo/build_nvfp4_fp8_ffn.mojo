# =============================================================================
# Plik: build_nvfp4_fp8_ffn.mojo
# Opis: Izolowany kompilator kerneli FP8 FFN dla modelu Bielik NVFP4.
# Przykład: pixi run mojo build_nvfp4_fp8_ffn.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.gemm_fp8_modular import (
    gemm_fp8_mod_11264_4096,
    gemm_fp8_mod_4096_11264,
)
from src.nvfp4 import pack_f16_fp8, pack_nvfp4_fp8


def _entry_from_ptx(ptx_path: Path) raises -> String:
    text = ptx_path.read_text()
    marker = ".visible .entry "
    i = text.find(marker)
    if i < 0:
        raise Error("brak wpisu PTX")
    j = text.find("(", i)
    return String(text[byte = i + marker.byte_length():j])


def _finalize(out_dir: Path, name: StringSlice) raises -> String:
    tmp = Path(String(name) + ".ptx")
    final = out_dir / (String(name) + ".ptx")
    text = tmp.read_text()
    text = text.replace(".version 8.1", ".version 8.4")
    final.write_text(text)
    os.remove(String(tmp))
    entry = _entry_from_ptx(final)
    print("  skompilowano", name, "->", entry)
    return (
        String('    "') + String(name) + String('": {"file": "')
        + String(name) + String('.ptx", "entry": "') + entry + String('"}')
    )


def main() raises:
    var ctx = DeviceContext()
    out_dir = Path("build") / ctx.arch_name()
    os.makedirs(String(out_dir), exist_ok=True)
    var entries = List[String]()
    _ = ctx.compile_function[
        gemm_fp8_mod_11264_4096,
        dump_asm=Path("gemm_fp8_mod_11264_4096.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_fp8_mod_11264_4096"))
    _ = ctx.compile_function[
        gemm_fp8_mod_4096_11264,
        dump_asm=Path("gemm_fp8_mod_4096_11264.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_fp8_mod_4096_11264"))
    _ = ctx.compile_function[
        pack_nvfp4_fp8,
        dump_asm=Path("pack_nvfp4_fp8.ptx"),
    ]()
    entries.append(_finalize(out_dir, "pack_nvfp4_fp8"))
    _ = ctx.compile_function[
        pack_f16_fp8,
        dump_asm=Path("pack_f16_fp8.ptx"),
    ]()
    entries.append(_finalize(out_dir, "pack_f16_fp8"))
    fragment = entries[0] + ",\n" + entries[1] + ",\n" + entries[2] + ",\n" + entries[3] + "\n"
    (out_dir / "manifest_nvfp4_fp8_ffn.json.fragment").write_text(fragment)
