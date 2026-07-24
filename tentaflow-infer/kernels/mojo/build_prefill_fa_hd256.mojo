# =============================================================================
# Plik: build_prefill_fa_hd256.mojo
# Opis: Kompiluje izolowane kerneli Flash Attention F16 HD256 do artefaktow PTX.
# Przyklad: pixi run mojo build_prefill_fa_hd256.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.prefill_fa_hd256 import (
    attn_prefill_fa_mojo_f16_hd256,
    attn_prefill_fa_mojo_device_pos_f16_hd256,
    attn_prefill_fa_mojo_f16_hd256_vtrans,
    attn_prefill_fa_mojo_device_pos_f16_hd256_vtrans,
    attn_prefill_fa_mojo_device_pos_f16_hd256_bk32,
)


def _finalize(out_dir: Path, name: String) raises:
    source = Path(name + ".ptx")
    target = out_dir / (name + ".ptx")
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    print("skompilowano", name, "->", String(target))


def main() raises:
    var ctx = DeviceContext()
    out_dir = Path("build") / ctx.arch_name()
    os.makedirs(String(out_dir), exist_ok=True)
    _ = ctx.compile_function[
        attn_prefill_fa_mojo_f16_hd256,
        dump_asm=Path("attn_prefill_fa_mojo_f16_hd256.ptx"),
    ]()
    _finalize(out_dir, "attn_prefill_fa_mojo_f16_hd256")
    _ = ctx.compile_function[
        attn_prefill_fa_mojo_device_pos_f16_hd256,
        dump_asm=Path("attn_prefill_fa_mojo_device_pos_f16_hd256.ptx"),
    ]()
    _finalize(out_dir, "attn_prefill_fa_mojo_device_pos_f16_hd256")
    _ = ctx.compile_function[
        attn_prefill_fa_mojo_f16_hd256_vtrans,
        dump_asm=Path("attn_prefill_fa_mojo_f16_hd256_vtrans.ptx"),
    ]()
    _finalize(out_dir, "attn_prefill_fa_mojo_f16_hd256_vtrans")
    _ = ctx.compile_function[
        attn_prefill_fa_mojo_device_pos_f16_hd256_vtrans,
        dump_asm=Path("attn_prefill_fa_mojo_device_pos_f16_hd256_vtrans.ptx"),
    ]()
    _finalize(out_dir, "attn_prefill_fa_mojo_device_pos_f16_hd256_vtrans")
    _ = ctx.compile_function[
        attn_prefill_fa_mojo_device_pos_f16_hd256_bk32,
        dump_asm=Path("attn_prefill_fa_mojo_device_pos_f16_hd256_bk32.ptx"),
    ]()
    _finalize(out_dir, "attn_prefill_fa_mojo_device_pos_f16_hd256_bk32")
