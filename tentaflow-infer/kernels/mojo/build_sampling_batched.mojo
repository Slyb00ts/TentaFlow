# ===== File: build_sampling_batched.mojo — isolated AOT build of the batched sampling kernels =====
# Usage: cd kernels/mojo && pixi run mojo build_sampling_batched.mojo

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.sampling import topk_batched_partial_f32, topk_batched_final_f32


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
        topk_batched_partial_f32,
        dump_asm=Path("topk_batched_partial_f32.ptx"),
    ]()
    _finalize(out_dir, "topk_batched_partial_f32")
    _ = ctx.compile_function[
        topk_batched_final_f32,
        dump_asm=Path("topk_batched_final_f32.ptx"),
    ]()
    _finalize(out_dir, "topk_batched_final_f32")
