# ===== File: build_q4k_native.mojo — isolated AOT build of the 24 native Q4_K int8 GEMM PTX =====
# The full build_kernels.mojo cannot run in this toolchain because the fp8 mma
# kernels emit PTX ISA 8.1 which the local ptxas rejects (needs 8.4); those PTX
# are already committed. This isolated driver compiles ONLY the portable int8
# Q4_K native GEMM instances (no fp8 in the module) so they can be regenerated
# without the fp8 dependency. It reuses build_kernels' _finalize retarget +
# manifest-fragment convention (sm_89 → sm_80 for these portable int8 tiles).

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path

from src.gemm_q4k_i8_multistage import (
    gemm_q4k_i8_native_4096_4096_m128,
    gemm_q4k_i8_native_4096_4096_m256,
    gemm_q4k_i8_native_4096_4096_m512,
    gemm_q4k_i8_native_4096_4096_m1024,
    gemm_q4k_i8_native_4096_4096_m2048,
    gemm_q4k_i8_native_4096_4096_m4096,
    gemm_q4k_i8_native_1024_4096_m128,
    gemm_q4k_i8_native_1024_4096_m256,
    gemm_q4k_i8_native_1024_4096_m512,
    gemm_q4k_i8_native_1024_4096_m1024,
    gemm_q4k_i8_native_1024_4096_m2048,
    gemm_q4k_i8_native_1024_4096_m4096,
    gemm_q4k_i8_native_14336_4096_m128,
    gemm_q4k_i8_native_14336_4096_m256,
    gemm_q4k_i8_native_14336_4096_m512,
    gemm_q4k_i8_native_14336_4096_m1024,
    gemm_q4k_i8_native_14336_4096_m2048,
    gemm_q4k_i8_native_14336_4096_m4096,
    gemm_q4k_i8_native_4096_14336_m128,
    gemm_q4k_i8_native_4096_14336_m256,
    gemm_q4k_i8_native_4096_14336_m512,
    gemm_q4k_i8_native_4096_14336_m1024,
    gemm_q4k_i8_native_4096_14336_m2048,
    gemm_q4k_i8_native_4096_14336_m4096,
)


def _entry_from_ptx(ptx_path: Path) raises -> String:
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
    tmp = Path(String(name) + ".ptx")
    final = out_dir / (String(name) + ".ptx")
    text = tmp.read_text()
    text = text.replace(".target sm_89", ".target sm_80")
    final.write_text(text)
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

    _ = ctx.compile_function[gemm_q4k_i8_native_4096_4096_m128, dump_asm=Path("gemm_q4k_i8_native_4096_4096_m128.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_4096_m128"))
    _ = ctx.compile_function[gemm_q4k_i8_native_4096_4096_m256, dump_asm=Path("gemm_q4k_i8_native_4096_4096_m256.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_4096_m256"))
    _ = ctx.compile_function[gemm_q4k_i8_native_4096_4096_m512, dump_asm=Path("gemm_q4k_i8_native_4096_4096_m512.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_4096_m512"))
    _ = ctx.compile_function[gemm_q4k_i8_native_4096_4096_m1024, dump_asm=Path("gemm_q4k_i8_native_4096_4096_m1024.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_4096_m1024"))
    _ = ctx.compile_function[gemm_q4k_i8_native_4096_4096_m2048, dump_asm=Path("gemm_q4k_i8_native_4096_4096_m2048.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_4096_m2048"))
    _ = ctx.compile_function[gemm_q4k_i8_native_4096_4096_m4096, dump_asm=Path("gemm_q4k_i8_native_4096_4096_m4096.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_4096_m4096"))
    _ = ctx.compile_function[gemm_q4k_i8_native_1024_4096_m128, dump_asm=Path("gemm_q4k_i8_native_1024_4096_m128.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_1024_4096_m128"))
    _ = ctx.compile_function[gemm_q4k_i8_native_1024_4096_m256, dump_asm=Path("gemm_q4k_i8_native_1024_4096_m256.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_1024_4096_m256"))
    _ = ctx.compile_function[gemm_q4k_i8_native_1024_4096_m512, dump_asm=Path("gemm_q4k_i8_native_1024_4096_m512.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_1024_4096_m512"))
    _ = ctx.compile_function[gemm_q4k_i8_native_1024_4096_m1024, dump_asm=Path("gemm_q4k_i8_native_1024_4096_m1024.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_1024_4096_m1024"))
    _ = ctx.compile_function[gemm_q4k_i8_native_1024_4096_m2048, dump_asm=Path("gemm_q4k_i8_native_1024_4096_m2048.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_1024_4096_m2048"))
    _ = ctx.compile_function[gemm_q4k_i8_native_1024_4096_m4096, dump_asm=Path("gemm_q4k_i8_native_1024_4096_m4096.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_1024_4096_m4096"))
    _ = ctx.compile_function[gemm_q4k_i8_native_14336_4096_m128, dump_asm=Path("gemm_q4k_i8_native_14336_4096_m128.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_14336_4096_m128"))
    _ = ctx.compile_function[gemm_q4k_i8_native_14336_4096_m256, dump_asm=Path("gemm_q4k_i8_native_14336_4096_m256.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_14336_4096_m256"))
    _ = ctx.compile_function[gemm_q4k_i8_native_14336_4096_m512, dump_asm=Path("gemm_q4k_i8_native_14336_4096_m512.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_14336_4096_m512"))
    _ = ctx.compile_function[gemm_q4k_i8_native_14336_4096_m1024, dump_asm=Path("gemm_q4k_i8_native_14336_4096_m1024.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_14336_4096_m1024"))
    _ = ctx.compile_function[gemm_q4k_i8_native_14336_4096_m2048, dump_asm=Path("gemm_q4k_i8_native_14336_4096_m2048.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_14336_4096_m2048"))
    _ = ctx.compile_function[gemm_q4k_i8_native_14336_4096_m4096, dump_asm=Path("gemm_q4k_i8_native_14336_4096_m4096.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_14336_4096_m4096"))
    _ = ctx.compile_function[gemm_q4k_i8_native_4096_14336_m128, dump_asm=Path("gemm_q4k_i8_native_4096_14336_m128.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_14336_m128"))
    _ = ctx.compile_function[gemm_q4k_i8_native_4096_14336_m256, dump_asm=Path("gemm_q4k_i8_native_4096_14336_m256.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_14336_m256"))
    _ = ctx.compile_function[gemm_q4k_i8_native_4096_14336_m512, dump_asm=Path("gemm_q4k_i8_native_4096_14336_m512.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_14336_m512"))
    _ = ctx.compile_function[gemm_q4k_i8_native_4096_14336_m1024, dump_asm=Path("gemm_q4k_i8_native_4096_14336_m1024.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_14336_m1024"))
    _ = ctx.compile_function[gemm_q4k_i8_native_4096_14336_m2048, dump_asm=Path("gemm_q4k_i8_native_4096_14336_m2048.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_14336_m2048"))
    _ = ctx.compile_function[gemm_q4k_i8_native_4096_14336_m4096, dump_asm=Path("gemm_q4k_i8_native_4096_14336_m4096.ptx")]()
    entries.append(_finalize(out_dir, "gemm_q4k_i8_native_4096_14336_m4096"))

    var frag = String("")
    for i in range(len(entries)):
        frag += entries[i]
        if i + 1 < len(entries):
            frag += ","
        frag += "\n"
    (out_dir / "manifest_q4k_native.json").write_text(frag)
    print("fragment written:", String(out_dir / "manifest_q4k_native.json"))
