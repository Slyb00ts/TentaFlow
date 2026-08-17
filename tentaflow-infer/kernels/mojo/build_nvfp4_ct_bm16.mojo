# =============================================================================
# Plik: build_nvfp4_ct_bm16.mojo
# Opis: Buduje zmierzone specjalizacje BM16 dla układu S0 N64/K128.
# Przykład: mojo build_nvfp4_ct_bm16.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.pathlib import Path
from std.python import Python
from src.nvfp4_ct_direct import (
    gemm_nvfp4_ct_bm16_down_m16,
    gemm_nvfp4_ct_bm16_down_m4,
    gemm_nvfp4_ct_bm16_down_m8,
    gemm_nvfp4_ct_bm16_gateup_m16,
    gemm_nvfp4_ct_bm16_gateup_m4,
    gemm_nvfp4_ct_bm16_gateup_m8,
    gemm_nvfp4_ct_bm16_o_m16,
    gemm_nvfp4_ct_bm16_o_m4,
    gemm_nvfp4_ct_bm16_o_m8,
    gemm_nvfp4_ct_bm16_qkv_m16,
    gemm_nvfp4_ct_bm16_qkv_m4,
    gemm_nvfp4_ct_bm16_qkv_m8,
    reduce_nvfp4_direct_down,
)


def _finish(out_dir: Path, name: StringSlice) raises:
    temporary = Path(String(name) + ".ptx")
    target = out_dir / (String(name) + ".ptx")
    subprocess = Python.import_module("subprocess")
    sys = Python.import_module("sys")
    _ = subprocess.run(
        Python.list(
            sys.executable,
            "scripts/validate_sm80_ptx.py",
            String(temporary),
            String(target),
            String(name),
            String(out_dir / "manifest.json"),
        ),
        check=True,
    )
    print("skompilowano", name, "->", String(target))


def main() raises:
    var ctx = DeviceContext()
    out_dir = Path("build") / ctx.arch_name()
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_qkv_m16,
        dump_asm=Path("gemm_nvfp4_ct_bm16_qkv_m16.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm16_qkv_m16")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_o_m16,
        dump_asm=Path("gemm_nvfp4_ct_bm16_o_m16.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm16_o_m16")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_gateup_m16,
        dump_asm=Path("gemm_nvfp4_ct_bm16_gateup_m16.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm16_gateup_m16")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_down_m16,
        dump_asm=Path("gemm_nvfp4_ct_bm16_down_m16.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm16_down_m16")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_qkv_m4,
        dump_asm=Path("gemm_nvfp4_ct_bm16_qkv_m4.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm16_qkv_m4")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_o_m4,
        dump_asm=Path("gemm_nvfp4_ct_bm16_o_m4.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm16_o_m4")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_gateup_m4,
        dump_asm=Path("gemm_nvfp4_ct_bm16_gateup_m4.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm16_gateup_m4")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_down_m4,
        dump_asm=Path("gemm_nvfp4_ct_bm16_down_m4.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm16_down_m4")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_qkv_m8,
        dump_asm=Path("gemm_nvfp4_ct_bm16_qkv_m8.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm16_qkv_m8")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_o_m8,
        dump_asm=Path("gemm_nvfp4_ct_bm16_o_m8.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm16_o_m8")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_gateup_m8,
        dump_asm=Path("gemm_nvfp4_ct_bm16_gateup_m8.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm16_gateup_m8")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm16_down_m8,
        dump_asm=Path("gemm_nvfp4_ct_bm16_down_m8.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm16_down_m8")
    _ = ctx.compile_function[
        reduce_nvfp4_direct_down,
        dump_asm=Path("reduce_nvfp4_ct_bm16.ptx"),
    ]()
    _finish(out_dir, "reduce_nvfp4_ct_bm16")
