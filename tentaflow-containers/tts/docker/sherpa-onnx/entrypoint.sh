#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Sidecar QUIC + sherpa startuja rownolegle. Sidecar nasluchuje
#       iroh natychmiast (klient widzi peera od razu); engine laduje model w
#       tle. Logi obu na stdout kontenera. PID1 czeka na pierwszego upadku
#       i grzecznie konczy drugiego.
# =============================================================================

set -uo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

PORT="${SHERPA_PORT:-8084}"
TOKENS="${SHERPA_TOKENS:-/data/models/tokens.txt}"
ACOUSTIC="${SHERPA_ACOUSTIC:-/data/models/model.onnx}"
LEXICON="${SHERPA_LEXICON:-}"

ARGS=(--port "$PORT" --vits-model="$ACOUSTIC" --vits-tokens="$TOKENS")
[[ -n "$LEXICON" ]] && ARGS+=(--vits-lexicon="$LEXICON")

echo "[entrypoint] sidecar config=$CONFIG_PATH"
NO_COLOR=1 /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH" 2>&1 \
  | sed -u 's/^/[sidecar] /' &
SIDECAR_PID=$!
echo "[entrypoint] sidecar PID=$SIDECAR_PID"

echo "[entrypoint] start sherpa"
sherpa-tts "${ARGS[@]}" 2>&1 \
  | sed -u 's/^/[sherpa] /' &
ENGINE_PID=$!
echo "[entrypoint] sherpa PID=$ENGINE_PID"

cleanup() {
  echo "[entrypoint] shutdown sidecar=$SIDECAR_PID engine=$ENGINE_PID"
  kill -TERM "$SIDECAR_PID" 2>/dev/null || true
  kill -TERM "$ENGINE_PID" 2>/dev/null || true
  wait "$SIDECAR_PID" 2>/dev/null || true
  wait "$ENGINE_PID" 2>/dev/null || true
}
trap cleanup SIGTERM SIGINT

wait -n "$SIDECAR_PID" "$ENGINE_PID"
EXIT_CODE=$?
echo "[entrypoint] proces ($EXIT_CODE) zakonczony - wychodze"
cleanup
exit $EXIT_CODE
