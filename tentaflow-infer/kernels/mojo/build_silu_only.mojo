# ===== File: build_silu_only.mojo — isolated AOT build of the fused gate/up kernels =====
# Usage: cd kernels/mojo && pixi run mojo build_silu_only.mojo
import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.decode_dp4a import (
    gemv_silu_q4_k_dp4a_f16_gidx_batch,
    gemv_silu_q6_k_dp4a_f16_gidx_batch,
)


def _fin(out_dir: Path, name: StringSlice) raises:
    source = Path(String(name) + ".ptx")
    target = out_dir / (String(name) + ".ptx")
    var text = source.read_text()
    text = text.replace(".target sm_89", ".target sm_80")
    target.write_text(text)
    os.remove(String(source))
    print("ok", name)


def main() raises:
    var ctx = DeviceContext()
    out_dir = Path("build") / ctx.arch_name()
    _ = ctx.compile_function[
        gemv_silu_q4_k_dp4a_f16_gidx_batch,
        dump_asm=Path("gemv_silu_q4_k_dp4a_f16_gidx_batch.ptx"),
    ]()
    _fin(out_dir, "gemv_silu_q4_k_dp4a_f16_gidx_batch")
    _ = ctx.compile_function[
        gemv_silu_q6_k_dp4a_f16_gidx_batch,
        dump_asm=Path("gemv_silu_q6_k_dp4a_f16_gidx_batch.ptx"),
    ]()
    _fin(out_dir, "gemv_silu_q6_k_dp4a_f16_gidx_batch")
