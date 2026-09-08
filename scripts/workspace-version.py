#!/usr/bin/env python3
# =============================================================================
# Plik: workspace-version.py
# Opis: Odczytuje wersję aplikacji lub zależności z głównego manifestu workspace.
# Przykład: python3 scripts/workspace-version.py --package
# =============================================================================

from pathlib import Path
import sys

if sys.version_info < (3, 11):
    sys.exit("Wymagany Python 3.11 lub nowszy.")

import tomllib

if len(sys.argv) != 2:
    sys.exit("Użycie: workspace-version.py --package | <nazwa zależności>")
with (Path(__file__).resolve().parent.parent / "Cargo.toml").open("rb") as manifest:
    workspace = tomllib.load(manifest)["workspace"]
if sys.argv[1] == "--package":
    version = workspace["package"]["version"]
else:
    dependency = workspace["dependencies"][sys.argv[1]]
    version = dependency if isinstance(dependency, str) else dependency["version"]
print(version.lstrip("="))
