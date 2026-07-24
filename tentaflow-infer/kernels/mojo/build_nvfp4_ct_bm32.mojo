# =============================================================================
# Plik: build_nvfp4_ct_bm32.mojo
# Opis: Buduje specjalizacje BM32 dla układu S0 N64/K128 (batch decode 17..32).
# Przykład: mojo build_nvfp4_ct_bm32.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.pathlib import Path
from std.python import Python
from src.nvfp4_ct_direct import (
    gemm_nvfp4_ct_bm32_down_m24,
    gemm_nvfp4_ct_bm32_down_m32,
    gemm_nvfp4_ct_bm32_gateup_m24,
    gemm_nvfp4_ct_bm32_gateup_m32,
    gemm_nvfp4_ct_bm32_o_m24,
    gemm_nvfp4_ct_bm32_o_m32,
    gemm_nvfp4_ct_bm32_qkv_m24,
    gemm_nvfp4_ct_bm32_qkv_m32,
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
        gemm_nvfp4_ct_bm32_qkv_m32,
        dump_asm=Path("gemm_nvfp4_ct_bm32_qkv_m32.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm32_qkv_m32")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm32_o_m32,
        dump_asm=Path("gemm_nvfp4_ct_bm32_o_m32.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm32_o_m32")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm32_gateup_m32,
        dump_asm=Path("gemm_nvfp4_ct_bm32_gateup_m32.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm32_gateup_m32")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm32_down_m32,
        dump_asm=Path("gemm_nvfp4_ct_bm32_down_m32.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm32_down_m32")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm32_qkv_m24,
        dump_asm=Path("gemm_nvfp4_ct_bm32_qkv_m24.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm32_qkv_m24")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm32_o_m24,
        dump_asm=Path("gemm_nvfp4_ct_bm32_o_m24.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm32_o_m24")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm32_gateup_m24,
        dump_asm=Path("gemm_nvfp4_ct_bm32_gateup_m24.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm32_gateup_m24")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_bm32_down_m24,
        dump_asm=Path("gemm_nvfp4_ct_bm32_down_m24.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_bm32_down_m24")
