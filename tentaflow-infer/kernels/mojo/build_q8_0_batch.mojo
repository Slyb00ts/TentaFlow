# =============================================================================
# Plik: build_q8_0_batch.mojo
# Opis: Izolowany kompilator AOT kerneli Q8_0 dla T=2/3/4.
# Przyklad: pixi run mojo build_q8_0_batch.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.q8_0_batch import gemm_q8_0_i8mma_b2, gemm_q8_0_i8mma_b3, gemm_q8_0_i8mma_b4
from src.q8_0_batch import gemm_q8_0_i8mma_out_f32_b3, gemm_q8_0_i8mma_out_f32_b4
from src.q8_0_batch import gemm_q8_0_dp4a_b3_nvidia, gemm_q8_0_dp4a_b4_nvidia
from src.q8_0_batch import gemm_q8_0_dp4a_out_f32_b3_nvidia, gemm_q8_0_dp4a_out_f32_b4_nvidia
from src.q8_0_batch import gemm_q8_0_f16_exact_out_f32_b2, gemm_q8_0_f16_exact_out_f32_b3, gemm_q8_0_f16_exact_out_f32_b4


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
        gemm_q8_0_i8mma_b2, dump_asm=Path("gemm_q8_0_i8mma_b2.ptx")
    ]()
    _finalize(out_dir, "gemm_q8_0_i8mma_b2")
    _ = ctx.compile_function[
        gemm_q8_0_i8mma_b3, dump_asm=Path("gemm_q8_0_i8mma_b3.ptx")
    ]()
    _finalize(out_dir, "gemm_q8_0_i8mma_b3")
    _ = ctx.compile_function[
        gemm_q8_0_i8mma_b4, dump_asm=Path("gemm_q8_0_i8mma_b4.ptx")
    ]()
    _finalize(out_dir, "gemm_q8_0_i8mma_b4")
    _ = ctx.compile_function[
        gemm_q8_0_i8mma_out_f32_b3,
        dump_asm=Path("gemm_q8_0_i8mma_out_f32_b3.ptx"),
    ]()
    _finalize(out_dir, "gemm_q8_0_i8mma_out_f32_b3")
    _ = ctx.compile_function[
        gemm_q8_0_i8mma_out_f32_b4,
        dump_asm=Path("gemm_q8_0_i8mma_out_f32_b4.ptx"),
    ]()
    _finalize(out_dir, "gemm_q8_0_i8mma_out_f32_b4")
    _ = ctx.compile_function[
        gemm_q8_0_dp4a_b3_nvidia, dump_asm=Path("gemm_q8_0_dp4a_b3_nvidia.ptx")
    ]()
    _finalize(out_dir, "gemm_q8_0_dp4a_b3_nvidia")
    _ = ctx.compile_function[
        gemm_q8_0_dp4a_b4_nvidia, dump_asm=Path("gemm_q8_0_dp4a_b4_nvidia.ptx")
    ]()
    _finalize(out_dir, "gemm_q8_0_dp4a_b4_nvidia")
    _ = ctx.compile_function[
        gemm_q8_0_dp4a_out_f32_b3_nvidia,
        dump_asm=Path("gemm_q8_0_dp4a_out_f32_b3_nvidia.ptx"),
    ]()
    _finalize(out_dir, "gemm_q8_0_dp4a_out_f32_b3_nvidia")
    _ = ctx.compile_function[
        gemm_q8_0_dp4a_out_f32_b4_nvidia,
        dump_asm=Path("gemm_q8_0_dp4a_out_f32_b4_nvidia.ptx"),
    ]()
    _finalize(out_dir, "gemm_q8_0_dp4a_out_f32_b4_nvidia")
    _ = ctx.compile_function[
        gemm_q8_0_f16_exact_out_f32_b2,
        dump_asm=Path("gemm_q8_0_f16_exact_out_f32_b2.ptx"),
    ]()
    _finalize(out_dir, "gemm_q8_0_f16_exact_out_f32_b2")
    _ = ctx.compile_function[
        gemm_q8_0_f16_exact_out_f32_b3,
        dump_asm=Path("gemm_q8_0_f16_exact_out_f32_b3.ptx"),
    ]()
    _finalize(out_dir, "gemm_q8_0_f16_exact_out_f32_b3")
    _ = ctx.compile_function[
        gemm_q8_0_f16_exact_out_f32_b4,
        dump_asm=Path("gemm_q8_0_f16_exact_out_f32_b4.ptx"),
    ]()
    _finalize(out_dir, "gemm_q8_0_f16_exact_out_f32_b4")
