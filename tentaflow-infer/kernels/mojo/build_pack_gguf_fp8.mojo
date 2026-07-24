# ===== File: build_pack_gguf_fp8.mojo — isolated AOT build of the GGUF→e4m3 pack kernels =====
# Usage: cd kernels/mojo && pixi run mojo build_pack_gguf_fp8.mojo

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.pack_gguf_fp8 import pack_q4_k_fp8, pack_q6_k_fp8, pack_q8_0_fp8


def _finalize(out_dir: Path, name: StringSlice) raises:
    source = Path(String(name) + ".ptx")
    target = out_dir / (String(name) + ".ptx")
    text = source.read_text().replace(".target sm_89", ".target sm_80")
    target.write_text(text)
    os.remove(String(source))
    print("skompilowano", name, "->", String(target))


def main() raises:
    var ctx = DeviceContext()
    out_dir = Path("build") / ctx.arch_name()
    os.makedirs(String(out_dir), exist_ok=True)

    _ = ctx.compile_function[
        pack_q4_k_fp8,
        dump_asm=Path("pack_q4_k_fp8.ptx"),
    ]()
    _finalize(out_dir, "pack_q4_k_fp8")
    _ = ctx.compile_function[
        pack_q6_k_fp8,
        dump_asm=Path("pack_q6_k_fp8.ptx"),
    ]()
    _finalize(out_dir, "pack_q6_k_fp8")
    _ = ctx.compile_function[
        pack_q8_0_fp8,
        dump_asm=Path("pack_q8_0_fp8.ptx"),
    ]()
    _finalize(out_dir, "pack_q8_0_fp8")
