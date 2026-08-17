# =============================================================================
# Plik: build_attention_decode_hd256.mojo
# Opis: Buduje produkcyjne kernele dzielonego flash-decode HD256.
# Przykład: pixi run mojo build_attention_decode_hd256.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.attention import (
    attn_decode_f16_hd256,
    attn_decode_split8_combine_f16_hd256,
    attn_decode_split8_f16_hd256,
)


def _finalize(out_dir: Path, name: String) raises:
    source = Path(name + ".ptx")
    target = out_dir / (name + ".ptx")
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))


def main() raises:
    var ctx = DeviceContext()
    out_dir = Path("build") / ctx.arch_name()
    os.makedirs(String(out_dir), exist_ok=True)
    _ = ctx.compile_function[
        attn_decode_f16_hd256,
        dump_asm=Path("attn_decode_f16_hd256.ptx"),
    ]()
    _finalize(out_dir, "attn_decode_f16_hd256")
    _ = ctx.compile_function[
        attn_decode_split8_f16_hd256,
        dump_asm=Path("attn_decode_split8_f16_hd256.ptx"),
    ]()
    _finalize(out_dir, "attn_decode_split8_f16_hd256")
    _ = ctx.compile_function[
        attn_decode_split8_combine_f16_hd256,
        dump_asm=Path("attn_decode_split8_combine_f16_hd256.ptx"),
    ]()
    _finalize(out_dir, "attn_decode_split8_combine_f16_hd256")
