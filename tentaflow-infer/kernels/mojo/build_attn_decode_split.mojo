# ===== File: build_attn_decode_split.mojo — isolated AOT build of the split flash-decode kernels =====
# Usage: cd kernels/mojo && pixi run mojo build_attn_decode_split.mojo

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.attention import (
    attn_decode_split_f16_hd64,
    attn_decode_split_f16_hd128,
    attn_decode_split_fp8_hd64,
    attn_decode_split_fp8_hd128,
)


def _finalize(out_dir: Path, name: StringSlice, keep_sm89: Bool) raises:
    source = Path(String(name) + ".ptx")
    target = out_dir / (String(name) + ".ptx")
    var text = source.read_text()
    if not keep_sm89:
        text = text.replace(".target sm_89", ".target sm_80")
    target.write_text(text)
    os.remove(String(source))
    print("skompilowano", name, "->", String(target))


def main() raises:
    var ctx = DeviceContext()
    out_dir = Path("build") / ctx.arch_name()
    os.makedirs(String(out_dir), exist_ok=True)

    _ = ctx.compile_function[
        attn_decode_split_f16_hd64,
        dump_asm=Path("attn_decode_split_f16_hd64.ptx"),
    ]()
    _finalize(out_dir, "attn_decode_split_f16_hd64", False)
    _ = ctx.compile_function[
        attn_decode_split_f16_hd128,
        dump_asm=Path("attn_decode_split_f16_hd128.ptx"),
    ]()
    _finalize(out_dir, "attn_decode_split_f16_hd128", False)
    _ = ctx.compile_function[
        attn_decode_split_fp8_hd64,
        dump_asm=Path("attn_decode_split_fp8_hd64.ptx"),
    ]()
    _finalize(out_dir, "attn_decode_split_fp8_hd64", True)
    _ = ctx.compile_function[
        attn_decode_split_fp8_hd128,
        dump_asm=Path("attn_decode_split_fp8_hd128.ptx"),
    ]()
    _finalize(out_dir, "attn_decode_split_fp8_hd128", True)
