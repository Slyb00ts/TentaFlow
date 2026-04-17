#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/ci-gates.sh
# Opis: CI gates dla refactoru WSS binary protocol (Task #35). Kazda bramka
#       zwraca non-zero gdy zlamana. Zlozone z:
#         1. Coverage (cargo-tarpaulin)
#         2. Fuzz smoke (cargo-fuzz, 5 min harness na Envelope/MessageBody)
#         3. WASM bundle size (codec.wasm <= 200 KB gzipped)
#         4. Observability check (kazdy #[handler] ma #[observed] — compile-gate)
#         5. Hardcoded-value gates (no emoji, no hex colors w CSS, no outline:none)
#       Uruchamiaj: ./scripts/ci-gates.sh
# =============================================================================

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FAIL=0

log()  { echo "[ci-gates] $*"; }
pass() { echo "  ✓ $*"; }
fail() { echo "  ✗ $*"; FAIL=1; }

# =============================================================================
# Gate 1: Coverage threshold (tarpaulin)
# =============================================================================
log "Gate 1: tarpaulin coverage"
if ! command -v cargo-tarpaulin >/dev/null 2>&1; then
  fail "cargo-tarpaulin nie zainstalowany (cargo install cargo-tarpaulin)"
else
  cd "$ROOT/tentaflow-protocol"
  if cargo tarpaulin --lib --line --out Json --output-dir "$ROOT/target/coverage" --timeout 120 >/dev/null 2>&1; then
    COV=$(python3 -c "import json; d=json.load(open('$ROOT/target/coverage/tarpaulin-report.json')); print(d.get('coverage', 0))" 2>/dev/null || echo 0)
    if awk "BEGIN { exit !($COV >= 80) }"; then
      pass "tentaflow-protocol coverage = ${COV}% (>= 80%)"
    else
      fail "tentaflow-protocol coverage = ${COV}% (<80%)"
    fi
  else
    fail "tarpaulin crashed — sprawdz logi"
  fi
  cd "$ROOT"
fi

# =============================================================================
# Gate 2: Fuzz smoke (5 min harness na Envelope + MessageBody decode)
# =============================================================================
log "Gate 2: cargo-fuzz smoke (5 min)"
if ! command -v cargo-fuzz >/dev/null 2>&1; then
  fail "cargo-fuzz nie zainstalowany (cargo install cargo-fuzz)"
else
  cd "$ROOT/tentaflow-protocol"
  if [[ ! -d fuzz ]]; then
    log "  brak katalogu fuzz/ — uruchom 'cargo fuzz init' raz"
    fail "fuzz harness nie zdefiniowany"
  else
    for target in envelope_decode message_body_decode; do
      if cargo fuzz run "$target" -- -max_total_time=300 >/dev/null 2>&1; then
        pass "fuzz $target: 5 min bez crasha"
      else
        fail "fuzz $target: crash lub timeout fail"
      fi
    done
  fi
  cd "$ROOT"
fi

# =============================================================================
# Gate 3: WASM bundle size (<= 200 KB gzipped)
# =============================================================================
log "Gate 3: WASM bundle size"
WASM_FILE="$ROOT/tentaflow-core/wwwroot/js/protocol/wasm_glue_bg.wasm"
MAX_GZIPPED_KB=200
if [[ ! -f "$WASM_FILE" ]]; then
  fail "brak $WASM_FILE — uruchom najpierw build.rs tentaflow-core"
else
  GZIPPED_BYTES=$(gzip -c "$WASM_FILE" | wc -c)
  GZIPPED_KB=$((GZIPPED_BYTES / 1024))
  if [[ $GZIPPED_KB -le $MAX_GZIPPED_KB ]]; then
    pass "wasm_glue_bg.wasm gzipped = ${GZIPPED_KB} KB (<= ${MAX_GZIPPED_KB} KB)"
  else
    fail "wasm_glue_bg.wasm gzipped = ${GZIPPED_KB} KB (> ${MAX_GZIPPED_KB} KB)"
  fi
fi

# =============================================================================
# Gate 4: Observability — KAZDY #[handler] ma #[observed]
# =============================================================================
# Compile-gate juz to enforce'uje (brak #[observed] = E0425). Tu tylko
# sanity check: grep wszystkich #[handler] musi miec #[observed] ABOVE lub BELOW.
log "Gate 4: observability coverage"
MISSING=$(grep -r -B1 -A1 "^#\[handler(" "$ROOT/tentaflow-core/src/dispatch/handlers.rs" 2>/dev/null \
  | grep -E "^#\[handler\(" \
  | awk '{print NR": "$0}' \
  | head -20)
HANDLER_COUNT=$(grep -c "^#\[handler(" "$ROOT/tentaflow-core/src/dispatch/handlers.rs" || echo 0)
OBSERVED_COUNT=$(grep -c "^#\[observed\]" "$ROOT/tentaflow-core/src/dispatch/handlers.rs" || echo 0)
if [[ $HANDLER_COUNT -eq $OBSERVED_COUNT ]]; then
  pass "handlers=${HANDLER_COUNT}, observed=${OBSERVED_COUNT} — equal"
else
  fail "handlers=${HANDLER_COUNT} != observed=${OBSERVED_COUNT} — brak #[observed]"
fi

# =============================================================================
# Gate 5: Hardcoded-value gates (DESIGN.md wymagania)
# =============================================================================
log "Gate 5: hardcoded-value checks"

# 5a: brak hex colors w CSS poza variables.css
HEX_VIOLATIONS=$(grep -rn '#[0-9a-fA-F]\{3,8\}' \
  "$ROOT/tentaflow-core/wwwroot/css/" 2>/dev/null \
  | grep -v 'variables.css' \
  | grep -v '\.min\.css' \
  | grep -iE '#(fff|000|[0-9a-f]{6})' \
  | head -5 || true)
if [[ -z "$HEX_VIOLATIONS" ]]; then
  pass "brak hardcoded hex colors poza variables.css"
else
  fail "hardcoded hex colors znalezione:"
  echo "$HEX_VIOLATIONS" | sed 's/^/    /'
fi

# 5b: brak emoji w kodzie (poza wwwroot/i18n jesli user confirms via DESIGN.md)
EMOJI=$(grep -rnP '[\x{1F300}-\x{1F9FF}\x{2600}-\x{26FF}]' \
  "$ROOT/tentaflow-core/wwwroot/js/" \
  "$ROOT/tentaflow-core/wwwroot/css/" \
  "$ROOT/tentaflow-core/wwwroot/index.html" 2>/dev/null \
  | head -5 || true)
if [[ -z "$EMOJI" ]]; then
  pass "brak emoji w JS/CSS/HTML"
else
  fail "emoji znalezione (uzyj SVG):"
  echo "$EMOJI" | sed 's/^/    /'
fi

# 5c: brak outline:none (accessibility)
OUTLINE_NONE=$(grep -rn 'outline:\s*none' "$ROOT/tentaflow-core/wwwroot/css/" 2>/dev/null \
  | head -5 || true)
if [[ -z "$OUTLINE_NONE" ]]; then
  pass "brak 'outline: none' (focus indicators preserved)"
else
  fail "'outline: none' znalezione — zlamanie a11y:"
  echo "$OUTLINE_NONE" | sed 's/^/    /'
fi

# =============================================================================
# Summary
# =============================================================================
echo ""
if [[ $FAIL -eq 0 ]]; then
  echo "[ci-gates] WSZYSTKIE BRAMKI PASSED ✓"
  exit 0
else
  echo "[ci-gates] BRAMKI FAILED — patrz wyzej"
  exit 1
fi
