# =============================================================================
# Plik: build_nvfp4_gguf_b16_nvidia.mojo
# Opis: Buduje brakujący wariant NVIDIA batchowego GEMM NVFP4 GGUF dla 16 tokenów.
# Przykład: mojo build_nvfp4_gguf_b16_nvidia.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.pathlib import Path
from std.python import Python
from src.nvfp4_gguf_batch import gemm_nvfp4_gguf_f16_b16_nvidia


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
        gemm_nvfp4_gguf_f16_b16_nvidia,
        dump_asm=Path("gemm_nvfp4_gguf_f16_b16_nvidia.ptx"),
    ]()
    _finish(out_dir, "gemm_nvfp4_gguf_f16_b16_nvidia")
