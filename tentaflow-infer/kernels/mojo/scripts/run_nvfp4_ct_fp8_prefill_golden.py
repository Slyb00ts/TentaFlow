#!/usr/bin/env python3
# =============================================================================
# Plik: run_nvfp4_ct_fp8_prefill_golden.py
# Opis: Buduje od zera i uruchamia golden FP8 z obowiązkowym shimem PTX 8.4.
# Przykład: python scripts/run_nvfp4_ct_fp8_prefill_golden.py
# =============================================================================

import os
import shutil
import subprocess
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


def main() -> None:
    mojo = shutil.which("mojo")
    if mojo is None:
        raise RuntimeError("brak kompilatora mojo w PATH")
    environment = os.environ.copy()
    environment["MODULAR_NVPTX_COMPILER_PATH"] = str(
        ROOT / "scripts" / "ptxas_fp8_shim.sh"
    )
    with tempfile.TemporaryDirectory(prefix="tentaflow-fp8-golden-") as temporary:
        root = Path(temporary)
        executable = root / "fp8_prefill_golden"
        audit = root / "ptxas-audit.log"
        environment["FORGE_PTXAS_AUDIT_LOG"] = str(audit)
        subprocess.run(
            [
                mojo,
                "build",
                str(ROOT / "test_nvfp4_ct_fp8_prefill_golden.mojo"),
                "-o",
                str(executable),
            ],
            cwd=ROOT,
            env=environment,
            check=True,
        )
        subprocess.run(
            [str(executable)],
            cwd=ROOT,
            env=environment,
            check=True,
        )
        if not audit.is_file() or not audit.read_text().strip():
            raise RuntimeError("golden nie uruchomił obowiązkowego shimu ptxas")
        print("fresh_build", executable.name, "ptxas_shim_invoked", True)


if __name__ == "__main__":
    main()

