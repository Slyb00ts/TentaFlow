#!/usr/bin/env python3
# =============================================================================
# Plik: sync_embedded_gfx1030.py
# Opis: Przepisuje liste wkompilowanych artefaktow gfx1030 w registry.rs wprost
#       z manifestu katalogu, zeby zestaw nie rozjechal sie z buildem kerneli.
# Przyklad: python scripts/sync_embedded_gfx1030.py
# =============================================================================
"""Wpisuje liste wkompilowanych artefaktow gfx1030 do registry.rs z manifestu."""
import json, pathlib, re, sys

root = pathlib.Path(__file__).resolve().parents[3]
manifest = json.loads((root / "kernels/mojo/build/gfx1030/manifest.json").read_text())
names = sorted(manifest["kernels"])
body = "\n".join(f'    "{n}",' for n in names)
block = f"const EMBEDDED_GFX1030: &[EmbeddedArtifact] = embedded_gfx1030![\n{body}\n];\n"

p = root / "crates/forge-kernels/src/registry.rs"
s = p.read_text()
if "const EMBEDDED_GFX1030" in s:
    s = re.sub(r"const EMBEDDED_GFX1030: &\[EmbeddedArtifact\] = embedded_gfx1030!\[.*?\n\];\n",
               block, s, flags=re.S)
else:
    anchor = "const EMBEDDED_SM89: &[EmbeddedArtifact] = embedded!["
    s = s.replace(anchor, block + "\n" + anchor, 1)
p.write_text(s)
print(f"wpisano {len(names)} artefaktow gfx1030")
