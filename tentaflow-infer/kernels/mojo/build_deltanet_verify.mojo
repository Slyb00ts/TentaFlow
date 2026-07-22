# =============================================================================
# Plik: build_deltanet_verify.mojo
# Opis: Kompiluje odizolowane kernele krótkiego skanu i zatwierdzania DeltaNet.
# Przykład: pixi run mojo build_deltanet_verify.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.deltanet_verify import (
    deltanet_prepare_t2_f16,
    deltanet_prepare_t3_f16,
    deltanet_prepare_t4_f16,
    deltanet_prepare_dynamic_f16,
    deltanet_gated_scan_t2_f16,
    deltanet_gated_scan_t3_f16,
    deltanet_gated_scan_t4_f16,
    deltanet_gated_scan_t3_d128_f16,
    deltanet_gated_scan_t4_d128_f16,
    deltanet_gated_scan_dynamic_f16,
    deltanet_gated_scan_dynamic_d128_f16,
    deltanet_gated_scan_inplace_dynamic_d128_f16,
    deltanet_gated_scan_inplace_shared_d128_f16,
    deltanet_commit_checkpoint_f32,
)


def _entry_from_ptx(ptx_path: Path) raises -> String:
    text = ptx_path.read_text()
    marker = ".visible .entry "
    i = text.find(marker)
    if i < 0:
        raise Error("brak wpisu .visible .entry w " + String(ptx_path))
    j = text.find("(", i)
    if j < 0:
        raise Error("niepoprawny wpis kernela w " + String(ptx_path))
    return String(text[byte = i + marker.byte_length():j])


def _finalize(out_dir: Path, name: StringSlice) raises:
    tmp = Path(String(name) + ".ptx")
    final = out_dir / (String(name) + ".ptx")
    text = tmp.read_text().replace(".target sm_89", ".target sm_80")
    final.write_text(text)
    os.remove(String(tmp))
    print(name, _entry_from_ptx(final))


def main() raises:
    var ctx = DeviceContext()
    arch = ctx.arch_name()
    out_dir = Path("build") / arch
    os.makedirs(String(out_dir), exist_ok=True)

    _ = ctx.compile_function[deltanet_prepare_t2_f16, dump_asm=Path("deltanet_prepare_t2_f16.ptx")]()
    _finalize(out_dir, "deltanet_prepare_t2_f16")
    _ = ctx.compile_function[deltanet_prepare_t3_f16, dump_asm=Path("deltanet_prepare_t3_f16.ptx")]()
    _finalize(out_dir, "deltanet_prepare_t3_f16")
    _ = ctx.compile_function[deltanet_prepare_t4_f16, dump_asm=Path("deltanet_prepare_t4_f16.ptx")]()
    _finalize(out_dir, "deltanet_prepare_t4_f16")
    _ = ctx.compile_function[deltanet_prepare_dynamic_f16, dump_asm=Path("deltanet_prepare_dynamic_f16.ptx")]()
    _finalize(out_dir, "deltanet_prepare_dynamic_f16")

    _ = ctx.compile_function[deltanet_gated_scan_t2_f16, dump_asm=Path("deltanet_gated_scan_t2_f16.ptx")]()
    _finalize(out_dir, "deltanet_gated_scan_t2_f16")
    _ = ctx.compile_function[deltanet_gated_scan_t3_f16, dump_asm=Path("deltanet_gated_scan_t3_f16.ptx")]()
    _finalize(out_dir, "deltanet_gated_scan_t3_f16")
    _ = ctx.compile_function[deltanet_gated_scan_t4_f16, dump_asm=Path("deltanet_gated_scan_t4_f16.ptx")]()
    _finalize(out_dir, "deltanet_gated_scan_t4_f16")
    _ = ctx.compile_function[deltanet_gated_scan_t3_d128_f16, dump_asm=Path("deltanet_gated_scan_t3_d128_f16.ptx")]()
    _finalize(out_dir, "deltanet_gated_scan_t3_d128_f16")
    _ = ctx.compile_function[deltanet_gated_scan_t4_d128_f16, dump_asm=Path("deltanet_gated_scan_t4_d128_f16.ptx")]()
    _finalize(out_dir, "deltanet_gated_scan_t4_d128_f16")
    _ = ctx.compile_function[deltanet_gated_scan_dynamic_f16, dump_asm=Path("deltanet_gated_scan_dynamic_f16.ptx")]()
    _finalize(out_dir, "deltanet_gated_scan_dynamic_f16")
    _ = ctx.compile_function[deltanet_gated_scan_dynamic_d128_f16, dump_asm=Path("deltanet_gated_scan_dynamic_d128_f16.ptx")]()
    _finalize(out_dir, "deltanet_gated_scan_dynamic_d128_f16")
    _ = ctx.compile_function[deltanet_gated_scan_inplace_dynamic_d128_f16, dump_asm=Path("deltanet_gated_scan_inplace_dynamic_d128_f16.ptx")]()
    _finalize(out_dir, "deltanet_gated_scan_inplace_dynamic_d128_f16")
    _ = ctx.compile_function[deltanet_gated_scan_inplace_shared_d128_f16, dump_asm=Path("deltanet_gated_scan_inplace_shared_d128_f16.ptx")]()
    _finalize(out_dir, "deltanet_gated_scan_inplace_shared_d128_f16")
    _ = ctx.compile_function[deltanet_commit_checkpoint_f32, dump_asm=Path("deltanet_commit_checkpoint_f32.ptx")]()
    _finalize(out_dir, "deltanet_commit_checkpoint_f32")
