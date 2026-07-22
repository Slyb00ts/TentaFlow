# =============================================================================
# Plik: build_nvfp4_gguf_batch.mojo
# Opis: Izolowany kompilator AOT natywnych kerneli GEMM GGUF NVFP4.
# Przyklad: pixi run mojo build_nvfp4_gguf_batch.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.nvfp4_gguf_batch import (
    gemm_nvfp4_gguf_f16_b2,
    gemm_nvfp4_gguf_out_f32_b2,
    gemm_nvfp4_gguf_f16_b3,
    gemm_nvfp4_gguf_f16_b4,
    gemm_nvfp4_gguf_f16_b1_nvidia,
    gemm_nvfp4_gguf_out_f32_b1_nvidia,
    gemm_nvfp4_gguf_f16_b3_nvidia,
    gemm_nvfp4_gguf_f16_b4_nvidia,
    gemm_nvfp4_gguf_f16_b8,
    gemm_nvfp4_gguf_f16_b16,
)
from src.nvfp4 import gemv_nvfp4_gguf_out_f32, pack_q8_0_nvfp4_gguf
from src.nvfp4_gguf_mma import (
    gemm_nvfp4_gguf_mma_f16_bm32,
    gemm_nvfp4_gguf_mma_f16_bm128,
    gemm_nvfp4_gguf_mma_f16_bm128_bn32,
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
        gemm_nvfp4_gguf_f16_b2,
        dump_asm=Path("gemm_nvfp4_gguf_f16_b2.ptx"),
    ]()
    _finalize(out_dir, "gemm_nvfp4_gguf_f16_b2")
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_out_f32_b2,
        dump_asm=Path("gemm_nvfp4_gguf_out_f32_b2.ptx"),
    ]()
    _finalize(out_dir, "gemm_nvfp4_gguf_out_f32_b2")
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b3,
        dump_asm=Path("gemm_nvfp4_gguf_f16_b3.ptx"),
    ]()
    _finalize(out_dir, "gemm_nvfp4_gguf_f16_b3")
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b4,
        dump_asm=Path("gemm_nvfp4_gguf_f16_b4.ptx"),
    ]()
    _finalize(out_dir, "gemm_nvfp4_gguf_f16_b4")
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b1_nvidia,
        dump_asm=Path("gemm_nvfp4_gguf_f16_b1_nvidia.ptx"),
    ]()
    _finalize(out_dir, "gemm_nvfp4_gguf_f16_b1_nvidia")
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_out_f32_b1_nvidia,
        dump_asm=Path("gemm_nvfp4_gguf_out_f32_b1_nvidia.ptx"),
    ]()
    _finalize(out_dir, "gemm_nvfp4_gguf_out_f32_b1_nvidia")
    _ = ctx.compile_function[
        gemv_nvfp4_gguf_out_f32,
        dump_asm=Path("gemv_nvfp4_gguf_out_f32.ptx"),
    ]()
    _finalize(out_dir, "gemv_nvfp4_gguf_out_f32")
    _ = ctx.compile_function[
        pack_q8_0_nvfp4_gguf,
        dump_asm=Path("pack_q8_0_nvfp4_gguf.ptx"),
    ]()
    _finalize(out_dir, "pack_q8_0_nvfp4_gguf")
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b3_nvidia,
        dump_asm=Path("gemm_nvfp4_gguf_f16_b3_nvidia.ptx"),
    ]()
    _finalize(out_dir, "gemm_nvfp4_gguf_f16_b3_nvidia")
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b4_nvidia,
        dump_asm=Path("gemm_nvfp4_gguf_f16_b4_nvidia.ptx"),
    ]()
    _finalize(out_dir, "gemm_nvfp4_gguf_f16_b4_nvidia")
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b8,
        dump_asm=Path("gemm_nvfp4_gguf_f16_b8.ptx"),
    ]()
    _finalize(out_dir, "gemm_nvfp4_gguf_f16_b8")
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_f16_b16,
        dump_asm=Path("gemm_nvfp4_gguf_f16_b16.ptx"),
    ]()
    _finalize(out_dir, "gemm_nvfp4_gguf_f16_b16")
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_mma_f16_bm32,
        dump_asm=Path("gemm_nvfp4_gguf_mma_f16_bm32.ptx"),
    ]()
    _finalize(out_dir, "gemm_nvfp4_gguf_mma_f16_bm32")
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_mma_f16_bm128,
        dump_asm=Path("gemm_nvfp4_gguf_mma_f16_bm128.ptx"),
    ]()
    _finalize(out_dir, "gemm_nvfp4_gguf_mma_f16_bm128")
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_mma_f16_bm128_bn32,
        dump_asm=Path("gemm_nvfp4_gguf_mma_f16_bm128_bn32.ptx"),
    ]()
    _finalize(out_dir, "gemm_nvfp4_gguf_mma_f16_bm128_bn32")
