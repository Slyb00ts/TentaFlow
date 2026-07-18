#!/usr/bin/env python3
# ===== File: merge_onnx_manifest.py — merge ONNX f32 kernels into manifest.json =====
# Reads the PTX files emitted by build_onnx_kernels.mojo, extracts each kernel's
# mangled `.visible .entry` symbol, and merges the entries into the committed
# build/<arch>/manifest.json without disturbing the main kernel set.
import json
import re
import sys
from pathlib import Path

ARCH = sys.argv[1] if len(sys.argv) > 1 else "sm_89"
ONNX_KERNELS = [
    "conv1d_f32", "relu_f32", "sigmoid_f32", "add_f32",
    "pow_f32", "sqrt_f32", "reduce_mean_f32", "lstm_f32",
]

build_dir = Path(__file__).resolve().parent.parent / "build" / ARCH
manifest_path = build_dir / "manifest.json"
manifest = json.loads(manifest_path.read_text())

entry_re = re.compile(r"\.visible \.entry ([A-Za-z0-9_$]+)\(")
for name in ONNX_KERNELS:
    ptx = build_dir / f"{name}.ptx"
    m = entry_re.search(ptx.read_text())
    if not m:
        raise SystemExit(f"no .visible .entry in {ptx}")
    manifest["kernels"][name] = {"file": f"{name}.ptx", "entry": m.group(1)}

# Re-emit in build_kernels.mojo's flat one-entry-per-line style (stable diff).
lines = [f'{{\n  "arch": "{manifest["arch"]}",\n  "kernels": {{']
items = list(manifest["kernels"].items())
for idx, (name, ent) in enumerate(items):
    comma = "," if idx + 1 < len(items) else ""
    lines.append(
        f'    "{name}": {{"file": "{ent["file"]}", "entry": "{ent["entry"]}"}}{comma}'
    )
lines.append("  }\n}\n")
manifest_path.write_text("\n".join(lines))
print(f"merged {len(ONNX_KERNELS)} ONNX kernels into {manifest_path}")
print(f"total kernels: {len(manifest['kernels'])}")
