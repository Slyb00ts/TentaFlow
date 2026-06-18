#!/usr/bin/env bash
# =============================================================================
# File: update-vllm-recipes.sh
# Purpose: Vendor a snapshot of the vLLM deployment recipes (https://recipes.vllm.ai)
#          into tentaflow-core/vllm-recipes/recipes.json.gz, which build.rs embeds
#          into the binary. This gives offline / HF-only deploys the same expert
#          launch flags + env vars that the online recipe API serves.
#
#          The rendered JSON is NOT committed upstream (only YAML sources, which
#          require their Node renderer). So we fetch the ALREADY-rendered public
#          JSON API and merge the parts we need: per model the base argv/env plus
#          the inline per-hardware-family overrides (hopper/blackwell/amd) and
#          model variants.
#
# Usage:   ./scripts/update-vllm-recipes.sh
# Refresh: re-run any time to pull fresh recipes (like build-all.sh --update).
# =============================================================================
set -euo pipefail

BASE_URL="${VLLM_RECIPES_BASE_URL:-https://recipes.vllm.ai}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="$SCRIPT_DIR/../tentaflow-core/vllm-recipes"
OUT_FILE="$OUT_DIR/recipes.json.gz"

mkdir -p "$OUT_DIR"

command -v python3 >/dev/null || { echo "python3 required" >&2; exit 1; }

echo "[recipes] fetching index + per-model recipes from $BASE_URL ..."

BASE_URL="$BASE_URL" OUT_FILE="$OUT_FILE" python3 <<'PY'
import gzip, json, os, sys, urllib.request
from concurrent.futures import ThreadPoolExecutor

base = os.environ["BASE_URL"].rstrip("/")
out_file = os.environ["OUT_FILE"]

def get(path):
    url = path if path.startswith("http") else base + path
    req = urllib.request.Request(url, headers={"User-Agent": "tentaflow-recipes-vendor"})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.loads(r.read().decode("utf-8"))

index = get("/models.json")
print(f"[recipes] index: {len(index)} models")

def strip_serve_prefix(argv):
    # Recipes prefix argv with ["vllm","serve","<model>"] — Core supplies those
    # itself (it owns --model/--served-model-name/--host/--port), so drop them.
    out = list(argv or [])
    if out[:2] == ["vllm", "serve"]:
        out = out[3:] if len(out) >= 3 else out[2:]
    return out

def fetch_one(entry):
    hf_id = entry.get("hf_id")
    jpath = entry.get("json")
    if not hf_id or not jpath:
        return None
    try:
        d = get(jpath)
    except Exception as e:  # noqa: BLE001 — best-effort vendor, skip failures
        print(f"[recipes] skip {hf_id}: {e}", file=sys.stderr)
        return None
    rc = d.get("recommended_command") or {}
    rec = {
        "hf_id": hf_id,
        "base_argv": strip_serve_prefix(rc.get("argv")),
        "base_env": rc.get("env") or {},
        "hardware_overrides": d.get("hardware_overrides") or {},
        "variants": {},
    }
    # Variants carry an alternative model_id (different weights/precision) plus
    # their own extra_env; argv is inherited from the base recommended_command.
    for vname, v in (d.get("variants") or {}).items():
        if not isinstance(v, dict):
            continue
        rec["variants"][vname] = {
            "model_id": v.get("model_id"),
            "extra_env": v.get("extra_env") or {},
            "precision": v.get("precision"),
        }
    return hf_id, rec

merged = {}
with ThreadPoolExecutor(max_workers=12) as ex:
    for res in ex.map(fetch_one, index):
        if not res:
            continue
        hf_id, rec = res
        merged[hf_id.lower()] = rec
        # Index variant model_ids as aliases pointing at the parent recipe so a
        # user deploying e.g. nvidia/DeepSeek-V3-FP4 resolves to its base flags.
        for v in rec["variants"].values():
            mid = v.get("model_id")
            if mid and mid.lower() not in merged:
                merged[mid.lower()] = {**rec, "_variant_of": hf_id, "_variant_env": v["extra_env"]}

payload = {"source": base, "count": len(merged), "recipes": merged}
raw = json.dumps(payload, separators=(",", ":"), sort_keys=True).encode("utf-8")
# Deterministic gzip: zero mtime + no stored filename, so identical recipe
# content always produces byte-identical output. Without this, every refresh
# (e.g. setup.sh) rewrites the gzip header timestamp and dirties the git tree.
with open(out_file, "wb") as fh, gzip.GzipFile(
    fileobj=fh, mode="wb", compresslevel=9, mtime=0
) as gz:
    gz.write(raw)
print(f"[recipes] wrote {out_file}: {len(merged)} entries, {len(raw)} bytes raw")
PY

echo "[recipes] done -> $OUT_FILE ($(du -h "$OUT_FILE" | cut -f1))"
