# =============================================================================
# Plik: build_norm_residual.mojo
# Opis: Buduje rozwinięty rmsnorm z residuałem i rejestruje go w manifeście.
# Przykład: mojo build_norm_residual.mojo
# =============================================================================

from std.gpu.host import DeviceContext
from std.pathlib import Path
from std.python import Python
from src.norm import rmsnorm_residual_f16


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
        rmsnorm_residual_f16,
        dump_asm=Path("rmsnorm_residual_f16.ptx"),
    ]()
    _finish(out_dir, "rmsnorm_residual_f16")
