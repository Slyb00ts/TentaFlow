#!/usr/bin/env python3
# =============================================================================
# Plik: sync_embedded_arch.py
# Opis: Przepisuje liste wkompilowanych artefaktow danej architektury w
#       registry.rs wprost z manifestu katalogu, zeby zestaw nie rozjechal sie
#       z buildem kerneli.
# Przyklad: python scripts/sync_embedded_arch.py gfx1100
# =============================================================================
"""Wpisuje liste wkompilowanych artefaktow architektury do registry.rs."""
import json, pathlib, re, sys

if len(sys.argv) != 2:
    raise SystemExit("uzycie: sync_embedded_arch.py <arch>")
arch = sys.argv[1]
root = pathlib.Path(__file__).resolve().parents[3]
manifest = json.loads((root / f"kernels/mojo/build/{arch}/manifest.json").read_text())
if manifest["arch"] != arch:
    raise SystemExit(f"manifest opisuje {manifest['arch']}, a proszono o {arch}")
extension = ".ptx" if arch.startswith("sm_") else ".hsaco"
constant = "EMBEDDED_" + arch.upper().replace("SM_", "SM")
names = sorted(manifest["kernels"])
body = "\n".join(f'    "{n}",' for n in names)
block = (
    f"const {constant}: &[EmbeddedArtifact] = "
    f'embedded_arch!["{arch}", "{extension}",\n{body}\n];\n'
)

p = root / "crates/forge-kernels/src/registry.rs"
s = p.read_text()
pattern = rf"const {constant}: &\[EmbeddedArtifact\] = embedded_arch!\[.*?\n\];\n"
if re.search(pattern, s, flags=re.S):
    s = re.sub(pattern, block, s, flags=re.S)
else:
    # Nowa architektura wchodzi przed zestawem NVIDII, bo ten jest najdluzszy i
    # zawsze obecny — kolejnosc stalych zostaje wtedy przewidywalna.
    anchor = "const EMBEDDED_SM89: &[EmbeddedArtifact] = embedded_arch!["
    s = s.replace(anchor, block + "\n" + anchor, 1)
p.write_text(s)
print(f"wpisano {len(names)} artefaktow {arch}")
