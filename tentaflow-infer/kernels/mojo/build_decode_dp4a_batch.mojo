# ===== File: build_decode_dp4a_batch.mojo — isolated AOT build of the small-batch dp4a GEMV kernels =====
# Usage: cd kernels/mojo && pixi run mojo build_decode_dp4a_batch.mojo
# Emits build/<arch>/gemv_q{4,6}_k_dp4a_batch_b{2,4,8,16}.ptx retargeted to
# sm_80 (dp4a needs sm_61+, no fp8/nvfp4 instructions involved).

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.decode_dp4a_batch import (
    gemv_q4_k_dp4a_batch_b2,
    gemv_q4_k_dp4a_batch_b4,
    gemv_q4_k_dp4a_batch_b8,
    gemv_q4_k_dp4a_batch_b16,
    gemv_q6_k_dp4a_batch_b2,
    gemv_q6_k_dp4a_batch_b4,
    gemv_q6_k_dp4a_batch_b8,
    gemv_q6_k_dp4a_batch_b16,
)


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
        gemv_q4_k_dp4a_batch_b2,
        dump_asm=Path("gemv_q4_k_dp4a_batch_b2.ptx"),
    ]()
    _finalize(out_dir, "gemv_q4_k_dp4a_batch_b2")
    _ = ctx.compile_function[
        gemv_q4_k_dp4a_batch_b4,
        dump_asm=Path("gemv_q4_k_dp4a_batch_b4.ptx"),
    ]()
    _finalize(out_dir, "gemv_q4_k_dp4a_batch_b4")
    _ = ctx.compile_function[
        gemv_q4_k_dp4a_batch_b8,
        dump_asm=Path("gemv_q4_k_dp4a_batch_b8.ptx"),
    ]()
    _finalize(out_dir, "gemv_q4_k_dp4a_batch_b8")
    _ = ctx.compile_function[
        gemv_q4_k_dp4a_batch_b16,
        dump_asm=Path("gemv_q4_k_dp4a_batch_b16.ptx"),
    ]()
    _finalize(out_dir, "gemv_q4_k_dp4a_batch_b16")
    _ = ctx.compile_function[
        gemv_q6_k_dp4a_batch_b2,
        dump_asm=Path("gemv_q6_k_dp4a_batch_b2.ptx"),
    ]()
    _finalize(out_dir, "gemv_q6_k_dp4a_batch_b2")
    _ = ctx.compile_function[
        gemv_q6_k_dp4a_batch_b4,
        dump_asm=Path("gemv_q6_k_dp4a_batch_b4.ptx"),
    ]()
    _finalize(out_dir, "gemv_q6_k_dp4a_batch_b4")
    _ = ctx.compile_function[
        gemv_q6_k_dp4a_batch_b8,
        dump_asm=Path("gemv_q6_k_dp4a_batch_b8.ptx"),
    ]()
    _finalize(out_dir, "gemv_q6_k_dp4a_batch_b8")
    _ = ctx.compile_function[
        gemv_q6_k_dp4a_batch_b16,
        dump_asm=Path("gemv_q6_k_dp4a_batch_b16.ptx"),
    ]()
    _finalize(out_dir, "gemv_q6_k_dp4a_batch_b16")
