#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Uruchamia sidecar QUIC + serwer Nemotron-Parse (FastAPI) rownolegle.
#       Sidecar nasluchuje iroh natychmiast, server.py laduje model w tle na
#       GPU (CUDA). PID 1 czeka na pierwszy padly proces.
# =============================================================================

set -uo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

export MODEL="${MODEL:-nvidia/NVIDIA-Nemotron-Parse-v1.2}"
export PORT="${PARSE_PORT:-8094}"

echo "[entrypoint] sidecar config=$CONFIG_PATH"
NO_COLOR=1 /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH" 2>&1 \
  | sed -u 's/^/[sidecar] /' &
SIDECAR_PID=$!

echo "[entrypoint] nemotron-parse server na 127.0.0.1:$PORT (model=$MODEL)"
uvicorn --app-dir /app server:app --host 127.0.0.1 --port "$PORT" 2>&1 \
  | sed -u 's/^/[parse] /' &
PARSE_PID=$!

cleanup() {
  echo "[entrypoint] shutdown sidecar=$SIDECAR_PID parse=$PARSE_PID"
  kill -TERM "$SIDECAR_PID" 2>/dev/null || true
  kill -TERM "$PARSE_PID" 2>/dev/null || true
  wait "$SIDECAR_PID" 2>/dev/null || true
  wait "$PARSE_PID" 2>/dev/null || true
}
trap cleanup SIGTERM SIGINT

wait -n "$SIDECAR_PID" "$PARSE_PID"
EXIT_CODE=$?
echo "[entrypoint] proces ($EXIT_CODE) zakonczony - wychodze"
cleanup
exit $EXIT_CODE
