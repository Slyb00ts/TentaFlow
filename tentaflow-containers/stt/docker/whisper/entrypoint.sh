#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Sidecar QUIC + whisper startuja rownolegle. Sidecar nasluchuje
#       iroh natychmiast (klient widzi peera od razu); engine laduje model w
#       tle. Logi obu na stdout kontenera. PID1 czeka na pierwszego upadku
#       i grzecznie konczy drugiego.
# =============================================================================

set -uo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

MODEL_PATH="${MODEL_PATH:-/data/models/ggml-large-v3-q5_0.bin}"
PORT="${WHISPER_PORT:-8081}"
WHISPER_ARGS="${WHISPER_ARGS:---threads 4 --processors 1}"

if [[ ! -f "$MODEL_PATH" ]]; then
  echo "[entrypoint] ERROR: MODEL_PATH=$MODEL_PATH nie istnieje - zamontuj /data/models"
  exit 1
fi

echo "[entrypoint] sidecar config=$CONFIG_PATH"
NO_COLOR=1 /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH" 2>&1 \
  | sed -u 's/^/[sidecar] /' &
SIDECAR_PID=$!
echo "[entrypoint] sidecar PID=$SIDECAR_PID"

echo "[entrypoint] start whisper"
# shellcheck disable=SC2086
whisper-server \
  --host 127.0.0.1 --port "$PORT" \
  --model "$MODEL_PATH" \
  $WHISPER_ARGS 2>&1 \
  | sed -u 's/^/[whisper] /' &
ENGINE_PID=$!
echo "[entrypoint] whisper PID=$ENGINE_PID"

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
