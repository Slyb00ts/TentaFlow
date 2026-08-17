# =============================================================================
# Plik: build_nvfp4_ct_stage1.mojo
# Opis: Kompiluje nieaktywne artefakty fazy 1 NVFP4 CT S0 N64/K128.
# Przykład: pixi run mojo build_nvfp4_ct_stage1.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.pathlib import Path
from std.python import Python
from src.nvfp4_ct_layout import repack_nvfp4_ct_s0_n64k128_into
from src.nvfp4_ct_decode import (
    gemv_nvfp4_ct_s0_n64k128_f16,
    gemv_batch_nvfp4_ct_s0_n64k128_f16_b4,
    gemv_batch_nvfp4_ct_s0_n64k128_f16_b8,
    gemv_batch_nvfp4_ct_s0_n64k128_f16_b16,
)
from src.nvfp4_ct_fp8 import pack_nvfp4_ct_s0_fp8
from src.nvfp4_ct_prefill import (
    gemm_nvfp4_ct_s0_f16_bm64,
    gemm_nvfp4_ct_s0_f16_bm128,
)
from src.decode_fused import (
    gemv_norm_nvfp4_ct_s0_f16,
    gemv_norm_silu_nvfp4_ct_s0_f16,
    gemv_residual_nvfp4_ct_s0_f16,
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
        repack_nvfp4_ct_s0_n64k128_into,
        dump_asm=Path("repack_nvfp4_ct_s0_n64k128_into.ptx"),
    ]()
    _finish(out_dir, "repack_nvfp4_ct_s0_n64k128_into")
    _ = ctx.compile_function[
        gemv_nvfp4_ct_s0_n64k128_f16,
        dump_asm=Path("gemv_nvfp4_ct_s0_n64k128_f16.ptx"),
    ]()
    _finish(out_dir, "gemv_nvfp4_ct_s0_n64k128_f16")
    _ = ctx.compile_function[
        gemv_batch_nvfp4_ct_s0_n64k128_f16_b4,
        dump_asm=Path("gemv_batch_nvfp4_ct_s0_n64k128_f16_b4.ptx"),
    ]()
    _finish(out_dir, "gemv_batch_nvfp4_ct_s0_n64k128_f16_b4")
    _ = ctx.compile_function[
        gemv_batch_nvfp4_ct_s0_n64k128_f16_b8,
        dump_asm=Path("gemv_batch_nvfp4_ct_s0_n64k128_f16_b8.ptx"),
    ]()
    _finish(out_dir, "gemv_batch_nvfp4_ct_s0_n64k128_f16_b8")
    _ = ctx.compile_function[
        gemv_batch_nvfp4_ct_s0_n64k128_f16_b16,
        dump_asm=Path("gemv_batch_nvfp4_ct_s0_n64k128_f16_b16.ptx"),
    ]()
    _finish(out_dir, "gemv_batch_nvfp4_ct_s0_n64k128_f16_b16")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_s0_f16_bm64,
        dump_asm=Path("gemm_nvfp4_ct_s0_f16_bm64.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_s0_f16_bm64")
    _ = ctx.compile_function[
        gemm_nvfp4_ct_s0_f16_bm128,
        dump_asm=Path("gemm_nvfp4_ct_s0_f16_bm128.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_ct_s0_f16_bm128")
    _ = ctx.compile_function[
        gemv_norm_nvfp4_ct_s0_f16,
        dump_asm=Path("gemv_norm_nvfp4_ct_s0_f16.ptx"),
    ]()
    _finish(out_dir, "gemv_norm_nvfp4_ct_s0_f16")
    _ = ctx.compile_function[
        gemv_norm_silu_nvfp4_ct_s0_f16,
        dump_asm=Path("gemv_norm_silu_nvfp4_ct_s0_f16.ptx"),
    ]()
    _finish(out_dir, "gemv_norm_silu_nvfp4_ct_s0_f16")
    _ = ctx.compile_function[
        gemv_residual_nvfp4_ct_s0_f16,
        dump_asm=Path("gemv_residual_nvfp4_ct_s0_f16.ptx"),
    ]()
    _finish(out_dir, "gemv_residual_nvfp4_ct_s0_f16")
    _ = ctx.compile_function[
        pack_nvfp4_ct_s0_fp8,
        dump_asm=Path("pack_nvfp4_ct_s0_fp8.ptx"),
    ]()
    _finish(out_dir, "pack_nvfp4_ct_s0_fp8")
