# =============================================================================
# Plik: build_mtp_stage.mojo
# Opis: Izolowany kompilator kernela metadanych kroku MTP.
# Przykład: pixi run mojo build_mtp_stage.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.mtp import mtp_stage_step, mtp_norm_join_shifted_f16, mtp_project_joined_q8_f16


def main() raises:
    var ctx = DeviceContext()
    out_dir = Path("build") / ctx.arch_name()
    os.makedirs(String(out_dir), exist_ok=True)
    _ = ctx.compile_function[mtp_stage_step, dump_asm=Path("mtp_stage_step.ptx")]()
    source = Path("mtp_stage_step.ptx")
    target = out_dir / "mtp_stage_step.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[
        mtp_project_joined_q8_f16,
        dump_asm=Path("mtp_project_joined_q8_f16.ptx"),
    ]()
    source = Path("mtp_project_joined_q8_f16.ptx")
    target = out_dir / "mtp_project_joined_q8_f16.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
    _ = ctx.compile_function[
        mtp_norm_join_shifted_f16,
        dump_asm=Path("mtp_norm_join_shifted_f16.ptx"),
    ]()
    source = Path("mtp_norm_join_shifted_f16.ptx")
    target = out_dir / "mtp_norm_join_shifted_f16.ptx"
    target.write_text(source.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(source))
