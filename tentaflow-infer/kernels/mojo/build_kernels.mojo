# ===== File: build_kernels.mojo — AOT kernel compiler: Mojo → PTX + manifest =====
# Compiles every registered kernel for the local GPU arch and dumps PTX into
# kernels/build/<arch>/<name>.ptx plus manifest.json describing each artifact.
# Rust (forge-kernels) loads these artifacts; no Mojo runtime ships in the
# server binary (ADR-0001).
#
# Registration is intentionally explicit: `dump_asm` is a compile-time
# parameter, so each kernel gets a literal dump path here and the file is
# relocated into the per-arch directory at runtime.

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.norm import rmsnorm_f16, rmsnorm_residual_f16
from src.activation import silu_mul_f16
from src.rope import rope_neox_f16
from src.gemv import gemv_q8_0_f16, gemv_f16


def _entry_from_ptx(ptx_path: Path) raises -> String:
    # The mangled kernel symbol is only known post-compilation, so recover it
    # from the emitted `.visible .entry <name>(` line.
    text = ptx_path.read_text()
    marker = ".visible .entry "
    i = text.find(marker)
    if i < 0:
        raise Error("no .visible .entry in " + String(ptx_path))
    j = text.find("(", i)
    if j < 0:
        raise Error("malformed entry line in " + String(ptx_path))
    return String(text[byte = i + marker.byte_length():j])


def _finalize(out_dir: Path, name: StringSlice) raises -> String:
    # Relocate the statically-named dump into the per-arch directory and
    # return its manifest fragment.
    tmp = Path(String(name) + ".ptx")
    final = out_dir / (String(name) + ".ptx")
    final.write_text(tmp.read_text())
    os.remove(String(tmp))
    entry = _entry_from_ptx(final)
    print("  compiled", name, "->", entry)
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
    print("target arch:", arch)

    var entries = List[String]()

    _ = ctx.compile_function[rmsnorm_f16, dump_asm=Path("rmsnorm_f16.ptx")]()
    entries.append(_finalize(out_dir, "rmsnorm_f16"))

    _ = ctx.compile_function[rmsnorm_residual_f16, dump_asm=Path("rmsnorm_residual_f16.ptx")]()
    entries.append(_finalize(out_dir, "rmsnorm_residual_f16"))

    _ = ctx.compile_function[silu_mul_f16, dump_asm=Path("silu_mul_f16.ptx")]()
    entries.append(_finalize(out_dir, "silu_mul_f16"))

    _ = ctx.compile_function[rope_neox_f16, dump_asm=Path("rope_neox_f16.ptx")]()
    entries.append(_finalize(out_dir, "rope_neox_f16"))

    _ = ctx.compile_function[gemv_q8_0_f16, dump_asm=Path("gemv_q8_0_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_q8_0_f16"))

    _ = ctx.compile_function[gemv_f16, dump_asm=Path("gemv_f16.ptx")]()
    entries.append(_finalize(out_dir, "gemv_f16"))

    var manifest = String('{\n  "arch": "') + arch + String('",\n  "kernels": {\n')
    for i in range(len(entries)):
        manifest += entries[i]
        if i + 1 < len(entries):
            manifest += ","
        manifest += "\n"
    manifest += String("  }\n}\n")
    (out_dir / "manifest.json").write_text(manifest)
    print("manifest written:", String(out_dir / "manifest.json"))
