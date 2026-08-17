# =============================================================================
# Plik: build_nvfp4_mma_bn128.mojo
# Opis: Kompiluje dokladny raw pipeline NVFP4 BM128 dla BN64 i BN128 do PTX.
# Przyklad: pixi run mojo build_nvfp4_mma_bn128.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.nvfp4_gguf_mma_bn128 import (
    gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1,
    gemm_nvfp4_gguf_mma_f16_bm128_bn128,
)


def main() raises:
    var ctx = DeviceContext()
    out_dir = Path("build") / ctx.arch_name()
    os.makedirs(String(out_dir), exist_ok=True)
    _ = ctx.compile_function[
        gemm_nvfp4_gguf_mma_f16_bm128_bn128,
        dump_asm=Path("gemm_nvfp4_gguf_mma_f16_bm128_bn128.ptx"),
    ]()
    temporary = Path("gemm_nvfp4_gguf_mma_f16_bm128_bn128.ptx")
    target = out_dir / "gemm_nvfp4_gguf_mma_f16_bm128_bn128.ptx"
    target.write_text(temporary.read_text().replace(".target sm_89", ".target sm_80"))
    os.remove(String(temporary))
    print("skompilowano raw NVFP4 BM128 BN128 ->", String(target))

    _ = ctx.compile_function[
        gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1,
        dump_asm=Path("gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1.ptx"),
    ]()
    temporary64 = Path("gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1.ptx")
    target64 = out_dir / "gemm_nvfp4_gguf_mma_f16_bm128_bn64_sync1.ptx"
    target64.write_text(
        temporary64.read_text().replace(".target sm_89", ".target sm_80")
    )
    os.remove(String(temporary64))
    print("skompilowano raw NVFP4 BM128 BN64 sync1 ->", String(target64))
