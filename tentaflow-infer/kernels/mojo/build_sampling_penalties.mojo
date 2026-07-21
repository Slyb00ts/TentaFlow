# =============================================================================
# Plik: build_sampling_penalties.mojo
# Opis: Izolowany kompilator fused kerneli kar i samplingu.
# Przykład: pixi run mojo build_sampling_penalties.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.sampling import (
    argmax_batched_f32,
    argmax_final_f32,
    argmax_partial_f32,
    penalize_batched_f32,
    penalize_histogram_f32,
    penalized_argmax_f32,
    topk_batched_f32,
    topk_final_f32,
    topk_partial_f32,
)


def main() raises:
    var ctx = DeviceContext()
    out_dir = Path("build") / ctx.arch_name()
    os.makedirs(String(out_dir), exist_ok=True)
    _ = ctx.compile_function[penalized_argmax_f32, dump_asm=Path("penalized_argmax_f32.ptx")]()
    source = Path("penalized_argmax_f32.ptx")
    target = out_dir / source
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[penalize_batched_f32, dump_asm=Path("penalize_batched_f32.ptx")]()
    source = Path("penalize_batched_f32.ptx")
    target = out_dir / source
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[argmax_partial_f32, dump_asm=Path("argmax_partial_f32.ptx")]()
    source = Path("argmax_partial_f32.ptx")
    target = out_dir / source
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[argmax_final_f32, dump_asm=Path("argmax_final_f32.ptx")]()
    source = Path("argmax_final_f32.ptx")
    target = out_dir / source
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[topk_partial_f32, dump_asm=Path("topk_partial_f32.ptx")]()
    source = Path("topk_partial_f32.ptx")
    target = out_dir / source
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[topk_final_f32, dump_asm=Path("topk_final_f32.ptx")]()
    source = Path("topk_final_f32.ptx")
    target = out_dir / source
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[argmax_batched_f32, dump_asm=Path("argmax_batched_f32.ptx")]()
    source = Path("argmax_batched_f32.ptx")
    target = out_dir / source
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[topk_batched_f32, dump_asm=Path("topk_batched_f32.ptx")]()
    source = Path("topk_batched_f32.ptx")
    target = out_dir / source
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[penalize_histogram_f32, dump_asm=Path("penalize_histogram_f32.ptx")]()
    source = Path("penalize_histogram_f32.ptx")
    target = out_dir / source
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
