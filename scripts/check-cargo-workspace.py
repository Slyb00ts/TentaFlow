#!/usr/bin/env python3
# =============================================================================
# Plik: check-cargo-workspace.py
# Opis: Sprawdza centralizację manifestów, zależności i profili Cargo.
# Przykład: python3 scripts/check-cargo-workspace.py
# =============================================================================

import argparse
import glob
import json
from pathlib import Path
import subprocess
import sys

if sys.version_info < (3, 11):
    sys.exit("Wymagany Python 3.11 lub nowszy.")

import tomllib


def dependency_sections(document):
    for kind in ("dependencies", "build-dependencies", "dev-dependencies"):
        if kind in document:
            yield kind, document[kind]
    for target, table in document.get("target", {}).items():
        for kind in ("dependencies", "build-dependencies", "dev-dependencies"):
            if kind in table:
                yield f"target.{target}.{kind}", table[kind]


def validate(root):
    root = root.resolve()
    root_manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    workspace = root_manifest["workspace"]
    catalog = workspace["dependencies"]
    errors = []
    members = set()
    for pattern in workspace["members"]:
        matches = glob.glob(str(root / pattern))
        if not matches:
            errors.append(f"Wzorzec członków nie wskazuje żadnej ścieżki: {pattern}")
        for match in matches:
            path = Path(match)
            if not path.is_dir():
                errors.append(f"Wzorzec członków musi wskazywać katalog pakietu: {pattern}")
                continue
            manifest = path / "Cargo.toml"
            if not manifest.is_file():
                errors.append(f"Członek bez manifestu: {manifest.relative_to(root)}")
                continue
            members.add(manifest.resolve())

    tracked = subprocess.check_output(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z", "*Cargo.toml", "*.cargo/config.toml", ".cargo/config.toml"],
        cwd=root,
    ).decode().split("\0")
    candidates = {root / name for name in tracked if name and (root / name).is_file()}
    candidates.update(members)
    packages = 0
    used = set()
    forbidden = {"version", "path", "git", "rev", "branch", "tag", "registry", "package"}
    for path in sorted(candidates):
        relative = path.relative_to(root)
        if relative.parts[0] in ("vendor", "thirdparty"):
            if path.resolve() in members:
                errors.append(f"Zewnętrzny pakiet nie może być członkiem workspace: {relative}")
            continue
        if path == root / "Cargo.toml":
            continue
        document = tomllib.loads(path.read_text(encoding="utf-8"))
        for key in ("profile", "patch", "workspace"):
            if key in document:
                errors.append(f"{relative}: sekcja [{key}] musi znajdować się w root Cargo.toml")
        if path.name != "Cargo.toml" or "package" not in document:
            continue
        packages += 1
        if path.resolve() not in members:
            errors.append(f"Własny pakiet poza workspace: {relative}")
        for section, dependencies in dependency_sections(document):
            for name, specification in dependencies.items():
                used.add(name)
                if not isinstance(specification, dict) or specification.get("workspace") is not True:
                    errors.append(f"{relative}: {section}.{name} nie dziedziczy workspace")
                    continue
                duplicated = forbidden.intersection(specification)
                if duplicated:
                    errors.append(f"{relative}: {section}.{name} powiela pola {', '.join(sorted(duplicated))}")
                if name not in catalog:
                    errors.append(f"{relative}: {section}.{name} nie istnieje w katalogu workspace")

    for name in sorted(set(catalog) - used):
        errors.append(f"Nieużywana zależność w katalogu workspace: {name}")
    indexed_outputs = subprocess.check_output([
        "git", "ls-files", "-z", "*Cargo.lock",
        "tentaflow-core/www/js/protocol/wasm_glue*",
        "tentaflow-core/www/js/voxel/voxel_glue*",
        "tentaflow-core/www/js/quantum/quantum_glue*",
        "tentaflow-core/www/js/generated/*",
    ], cwd=root).decode().split("\0")
    for name in indexed_outputs:
        if not name:
            continue
        if name.startswith("tentaflow-core/www/js/"):
            errors.append(f"Generowany plik przeglądarki w indeksie Git: {name}")
        elif name != "Cargo.lock" and Path(name).parts[0] not in ("vendor", "thirdparty"):
            errors.append(f"Własny lockfile poza korzeniem: {name}")
    if not (root / "Cargo.lock").is_file():
        errors.append("Brak wspólnego Cargo.lock")
    elif "Cargo.lock" not in indexed_outputs:
        errors.append("Wspólny Cargo.lock musi być śledzony przez Git")
    if not errors:
        result = subprocess.run(
            ["cargo", "metadata", "--no-deps", "--offline", "--format-version", "1"],
            cwd=root,
            capture_output=True,
            text=True,
            encoding="utf-8",
        )
        if result.returncode:
            errors.append(f"Cargo odrzuca workspace: {result.stderr.strip()}")
        else:
            metadata = json.loads(result.stdout)
            member_ids = set(metadata["workspace_members"])
            actual = {Path(package["manifest_path"]).resolve() for package in metadata["packages"] if package["id"] in member_ids}
            if actual != members:
                errors.append("Lista członków Cargo różni się od zadeklarowanych własnych pakietów")
    return errors, packages


def main():
    parser = argparse.ArgumentParser(description="Sprawdzenie wspólnej konfiguracji Cargo.")
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    args = parser.parse_args()
    try:
        errors, packages = validate(args.root)
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"Nie można zweryfikować workspace: {error}", file=sys.stderr)
        return 1
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"Workspace poprawny: {packages} własnych pakietów; zależności i profile w korzeniu.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
