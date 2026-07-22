# =============================================================================
# Plik: build_nvfp4_batch.mojo
# Opis: Izolowany kompilator AOT kerneli NVFP4 dla małego batcha dekodu.
# Przykład: pixi run mojo build_nvfp4_batch.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path

from src.nvfp4_batch import (
    gemv_batch_nvfp4_f16_b4,
    gemv_batch_nvfp4_f16_b8,
    gemv_batch_nvfp4_f16_b16,
    gemv_batch_f16_out_f32_b4,
    gemv_batch_f16_out_f32_b8,
)
from src.gemm import gemm_nvfp4_f16_bm32, gemm_f16_out_f32_bm32


def _entry_from_ptx(ptx_path: Path) raises -> String:
    text = ptx_path.read_text()
    marker = ".visible .entry "
    i = text.find(marker)
    if i < 0:
        raise Error("brak .visible .entry w " + String(ptx_path))
    j = text.find("(", i)
    if j < 0:
        raise Error("nieprawidłowy wpis PTX w " + String(ptx_path))
    return String(text[byte = i + marker.byte_length():j])


def _finalize(out_dir: Path, name: StringSlice) raises -> String:
    tmp = Path(String(name) + ".ptx")
    final = out_dir / (String(name) + ".ptx")
    text = tmp.read_text()
    final.write_text(text)
    os.remove(String(tmp))
    entry = _entry_from_ptx(final)
    print("  skompilowano", name, "->", entry)
    return (
        String('    "')
        + String(name)
        + String('": {"file": "')
        + String(name)
        + String('.ptx", "entry": "')
        + entry
        + String('"}')
    )


def main() raises:
    var ctx = DeviceContext()
    arch = ctx.arch_name()
    out_dir = Path("build") / arch
    os.makedirs(String(out_dir), exist_ok=True)
    print("architektura docelowa:", arch)

    var entries = List[String]()
    _ = ctx.compile_function[
        gemv_batch_nvfp4_f16_b4,
        dump_asm=Path("gemv_batch_nvfp4_f16_b4.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_batch_nvfp4_f16_b4"))
    _ = ctx.compile_function[
        gemv_batch_nvfp4_f16_b8,
        dump_asm=Path("gemv_batch_nvfp4_f16_b8.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_batch_nvfp4_f16_b8"))
    _ = ctx.compile_function[
        gemv_batch_nvfp4_f16_b16,
        dump_asm=Path("gemv_batch_nvfp4_f16_b16.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_batch_nvfp4_f16_b16"))
    _ = ctx.compile_function[
        gemv_batch_f16_out_f32_b4,
        dump_asm=Path("gemv_batch_f16_out_f32_b4.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_batch_f16_out_f32_b4"))
    _ = ctx.compile_function[
        gemv_batch_f16_out_f32_b8,
        dump_asm=Path("gemv_batch_f16_out_f32_b8.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemv_batch_f16_out_f32_b8"))
    _ = ctx.compile_function[
        gemm_nvfp4_f16_bm32,
        dump_asm=Path("gemm_nvfp4_f16_bm32.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_nvfp4_f16_bm32"))
    _ = ctx.compile_function[
        gemm_f16_out_f32_bm32,
        dump_asm=Path("gemm_f16_out_f32_bm32.ptx"),
    ]()
    entries.append(_finalize(out_dir, "gemm_f16_out_f32_bm32"))

    var fragment = String("")
    for i in range(len(entries)):
        fragment += entries[i]
        if i + 1 < len(entries):
            fragment += ","
        fragment += "\n"
    (out_dir / "manifest_nvfp4_batch.json.fragment").write_text(fragment)
    print(
        "zapisano fragment manifestu:",
        String(out_dir / "manifest_nvfp4_batch.json.fragment"),
    )
