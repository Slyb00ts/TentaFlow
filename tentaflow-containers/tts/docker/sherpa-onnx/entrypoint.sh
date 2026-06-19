#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: sherpa-onnx-offline-tts-server — direct-http (bez sidecara). Server
#       eksponuje /tts (custom JSON) i nasluchuje na 0.0.0.0:${PORT} (upstream
#       binduje wszystkie interfejsy domyslnie — brak flagi --host). Core gada
#       HTTP wprost do host-mapped portu.
# =============================================================================

set -uo pipefail

PORT="${PORT:-8084}"
TOKENS="${SHERPA_TOKENS:-/data/models/tokens.txt}"
ACOUSTIC="${SHERPA_ACOUSTIC:-/data/models/model.onnx}"
LEXICON="${SHERPA_LEXICON:-}"

ARGS=(--port "$PORT" --vits-model="$ACOUSTIC" --vits-tokens="$TOKENS")
[[ -n "$LEXICON" ]] && ARGS+=(--vits-lexicon="$LEXICON")

echo "[entrypoint] start sherpa na 0.0.0.0:$PORT"
exec sherpa-tts "${ARGS[@]}"
