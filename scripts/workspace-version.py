#!/usr/bin/env python3
# =============================================================================
# Plik: workspace-version.py
# Opis: Odczytuje wersję narzędzia powiązanego z zależnością workspace.
# Przykład: python3 scripts/workspace-version.py wasm-bindgen
# =============================================================================

from pathlib import Path
import sys

if sys.version_info < (3, 11):
    sys.exit("Wymagany Python 3.11 lub nowszy.")

import tomllib

if len(sys.argv) != 2:
    sys.exit("Użycie: workspace-version.py <nazwa zależności>")
with (Path(__file__).resolve().parent.parent / "Cargo.toml").open("rb") as manifest:
    dependency = tomllib.load(manifest)["workspace"]["dependencies"][sys.argv[1]]
version = dependency if isinstance(dependency, str) else dependency["version"]
print(version.lstrip("="))
