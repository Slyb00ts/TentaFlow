# =============================================================================
# Plik: build_attention_segmented_hd128.mojo
# Opis: Buduje przenośny i warp32 kernel segmentowanej atencji dla HD128.
# Przykład: pixi run mojo build_attention_segmented_hd128.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.attention import attn_verify_segmented_f16_hd128, attn_verify_segmented_f16_hd128_warp32
from src.prefill import attn_prefill_segmented_f16_hd128, attn_prefill_segmented_f16_hd256, attn_prefill_fa_segmented_f16_hd128


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
        attn_verify_segmented_f16_hd128,
        dump_asm=Path("attn_verify_segmented_f16_hd128.ptx"),
    ]()
    _store(out_dir, "attn_verify_segmented_f16_hd128")
    _ = ctx.compile_function[
        attn_verify_segmented_f16_hd128_warp32,
        dump_asm=Path("attn_verify_segmented_f16_hd128_warp32.ptx"),
    ]()
    _store(out_dir, "attn_verify_segmented_f16_hd128_warp32")
    _ = ctx.compile_function[
        attn_prefill_segmented_f16_hd128,
        dump_asm=Path("attn_prefill_segmented_f16_hd128.ptx"),
    ]()
    _store(out_dir, "attn_prefill_segmented_f16_hd128")
    _ = ctx.compile_function[
        attn_prefill_segmented_f16_hd256,
        dump_asm=Path("attn_prefill_segmented_f16_hd256.ptx"),
    ]()
    _store(out_dir, "attn_prefill_segmented_f16_hd256")
    _ = ctx.compile_function[
        attn_prefill_fa_segmented_f16_hd128,
        dump_asm=Path("attn_prefill_fa_segmented_f16_hd128.ptx"),
    ]()
    _store(out_dir, "attn_prefill_fa_segmented_f16_hd128")
