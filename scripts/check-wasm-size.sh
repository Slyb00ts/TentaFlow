#!/usr/bin/env bash
# ===== File: scripts/check-wasm-size.sh — size budget for the browser protocol codec =====
#
# The browser codec is generic (no typed protocol `Deserialize`); a single typed
# `from_value::<T>` on a protocol type links the visitors back and multiplies the size,
# so this budget is what catches it. Raise the numbers only with a measurement.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WASM_FILE="${1:-$ROOT/tentaflow-core/www/js/protocol/wasm_glue_bg.wasm}"
MAX_RAW_KB=1536
MAX_GZIPPED_KB=420

if [[ ! -f "$WASM_FILE" ]]; then
  echo "missing $WASM_FILE — build tentaflow-core first" >&2
  exit 1
fi

RAW_KB=$(( $(wc -c < "$WASM_FILE") / 1024 ))
GZIPPED_KB=$(( $(gzip -c "$WASM_FILE" | wc -c) / 1024 ))
echo "wasm_glue_bg.wasm: ${RAW_KB} KB raw (budget ${MAX_RAW_KB}), ${GZIPPED_KB} KB gzipped (budget ${MAX_GZIPPED_KB})"
if [[ $RAW_KB -gt $MAX_RAW_KB || $GZIPPED_KB -gt $MAX_GZIPPED_KB ]]; then
  echo "over budget" >&2
  exit 1
fi
