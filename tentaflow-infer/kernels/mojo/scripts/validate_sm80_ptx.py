#!/usr/bin/env python3
# =============================================================================
# Plik: validate_sm80_ptx.py
# Opis: Waliduje retargetowany PTX przez ptxas przed atomową publikacją.
# Przykład: python scripts/validate_sm80_ptx.py kernel.ptx build/sm_89/kernel.ptx
# =============================================================================

import os
import subprocess
import sys
import tempfile
from pathlib import Path
import json


def sm80_text(source: str) -> str:
    candidate = source.replace(".target sm_89", ".target sm_80")
    if ".target sm_80" not in candidate:
        raise RuntimeError("PTX nie zawiera targetu sm_80 ani sm_89")
    return candidate


def validate_sm80_text(text: str, ptxas: str | Path | None = None) -> None:
    executable = Path(ptxas or os.environ.get("FORGE_REAL_PTXAS", "/opt/cuda/bin/ptxas"))
    with tempfile.TemporaryDirectory(prefix="tentaflow-sm80-") as temporary:
        root = Path(temporary)
        candidate = root / "candidate.ptx"
        cubin = root / "candidate.cubin"
        candidate.write_text(text)
        subprocess.run(
            [str(executable), "-arch=sm_80", str(candidate), "-o", str(cubin)],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )


def publish_sm80(
    source: Path,
    target: Path,
    ptxas: str | Path | None = None,
) -> None:
    text = sm80_text(source.read_text())
    staged_path = None
    try:
        validate_sm80_text(text, ptxas)
        target.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(
            mode="w",
            prefix=f".{target.name}.",
            suffix=".tmp",
            dir=target.parent,
            delete=False,
        ) as staged:
            staged.write(text)
            staged_path = Path(staged.name)
        os.replace(staged_path, target)
        staged_path = None
    finally:
        source.unlink(missing_ok=True)
        if staged_path is not None:
            staged_path.unlink(missing_ok=True)


def manifest_text(manifest: Path, name: str, target: Path, text: str) -> str:
    marker = ".visible .entry "
    start = text.find(marker)
    end = text.find("(", start)
    if start < 0 or end < 0:
        raise RuntimeError(f"PTX {target} nie zawiera poprawnego symbolu entry")
    entry = text[start + len(marker) : end]
    document = json.loads(manifest.read_text())
    document["kernels"][name] = {"entry": entry, "file": target.name}
    return json.dumps(document, indent=2, sort_keys=True) + "\n"


def publish_sm80_with_manifest(
    source: Path,
    target: Path,
    name: str,
    manifest: Path,
    ptxas: str | Path | None = None,
) -> None:
    text = sm80_text(source.read_text())
    staged_target = None
    staged_manifest = None
    target_backup = target.with_name(f".{target.name}.previous")
    manifest_backup = manifest.with_name(f".{manifest.name}.previous")
    try:
        validate_sm80_text(text, ptxas)
        updated_manifest = manifest_text(manifest, name, target, text)
        target.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(
            mode="w",
            prefix=f".{target.name}.",
            suffix=".tmp",
            dir=target.parent,
            delete=False,
        ) as staged:
            staged.write(text)
            staged_target = Path(staged.name)
        with tempfile.NamedTemporaryFile(
            mode="w",
            prefix=f".{manifest.name}.",
            suffix=".tmp",
            dir=manifest.parent,
            delete=False,
        ) as staged:
            staged.write(updated_manifest)
            staged_manifest = Path(staged.name)

        target_backup.unlink(missing_ok=True)
        manifest_backup.unlink(missing_ok=True)
        target_backed_up = False
        manifest_backed_up = False
        target_installed = False
        manifest_installed = False
        try:
            if target.exists():
                os.replace(target, target_backup)
                target_backed_up = True
            if manifest.exists():
                os.replace(manifest, manifest_backup)
                manifest_backed_up = True
            os.replace(staged_target, target)
            staged_target = None
            target_installed = True
            os.replace(staged_manifest, manifest)
            staged_manifest = None
            manifest_installed = True
        except BaseException:
            if target_installed:
                target.unlink(missing_ok=True)
            if manifest_installed:
                manifest.unlink(missing_ok=True)
            if target_backed_up:
                os.replace(target_backup, target)
            if manifest_backed_up:
                os.replace(manifest_backup, manifest)
            raise
        target_backup.unlink(missing_ok=True)
        manifest_backup.unlink(missing_ok=True)
    finally:
        source.unlink(missing_ok=True)
        if staged_target is not None:
            staged_target.unlink(missing_ok=True)
        if staged_manifest is not None:
            staged_manifest.unlink(missing_ok=True)


def main() -> None:
    if len(sys.argv) not in (3, 5):
        raise SystemExit(
            "użycie: validate_sm80_ptx.py SOURCE TARGET [NAZWA MANIFEST]"
        )
    target = Path(sys.argv[2])
    if len(sys.argv) == 5:
        publish_sm80_with_manifest(
            Path(sys.argv[1]),
            target,
            sys.argv[3],
            Path(sys.argv[4]),
        )
    else:
        publish_sm80(Path(sys.argv[1]), target)


if __name__ == "__main__":
    main()
