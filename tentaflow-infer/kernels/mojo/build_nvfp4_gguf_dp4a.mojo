# =============================================================================
# Plik: build_nvfp4_gguf_dp4a.mojo
# Opis: Buduje przenośny artefakt PTX decode GEMV GGUF NVFP4 Q8_1/dp4a.
# Przykład: mojo build_nvfp4_gguf_dp4a.mojo
# =============================================================================

import std.os as os
from std.gpu.host import DeviceContext
from std.pathlib import Path
from src.nvfp4_gguf_dp4a import gemv_nvfp4_gguf_q8_1_f16


def main() raises:
    var ctx = DeviceContext()
    out_dir = Path("build") / ctx.arch_name()
    os.makedirs(String(out_dir), exist_ok=True)
    _ = ctx.compile_function[
        gemv_nvfp4_gguf_q8_1_f16,
        dump_asm=Path("gemv_nvfp4_gguf_q8_1_f16.ptx"),
    ]()
    source = Path("gemv_nvfp4_gguf_q8_1_f16.ptx")
    target = out_dir / source
    text = source.read_text().replace(".target sm_89", ".target sm_80")
    target.write_text(text)
    os.remove(String(source))
    print("zapisano", String(target))
