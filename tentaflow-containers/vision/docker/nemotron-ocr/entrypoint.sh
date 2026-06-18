#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Uruchamia sidecar QUIC + serwer Nemotron-OCR (FastAPI) rownolegle.
#       Sidecar nasluchuje iroh natychmiast, server.py laduje model w tle.
#       PID 1 czeka na pierwszy proces ktory padnie, drugi konczy grzecznie.
# =============================================================================

set -uo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

export MODEL="${MODEL:-nvidia/nemotron-ocr-v1}"
export PORT="${OCR_PORT:-8093}"

echo "[entrypoint] sidecar config=$CONFIG_PATH"
NO_COLOR=1 /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH" 2>&1 \
  | sed -u 's/^/[sidecar] /' &
SIDECAR_PID=$!

echo "[entrypoint] nemotron-ocr server na 127.0.0.1:$PORT (model=$MODEL)"
uvicorn --app-dir /app server:app --host 127.0.0.1 --port "$PORT" 2>&1 \
  | sed -u 's/^/[ocr] /' &
OCR_PID=$!

cleanup() {
  echo "[entrypoint] shutdown sidecar=$SIDECAR_PID ocr=$OCR_PID"
  kill -TERM "$SIDECAR_PID" 2>/dev/null || true
  kill -TERM "$OCR_PID" 2>/dev/null || true
  wait "$SIDECAR_PID" 2>/dev/null || true
  wait "$OCR_PID" 2>/dev/null || true
}
trap cleanup SIGTERM SIGINT

wait -n "$SIDECAR_PID" "$OCR_PID"
EXIT_CODE=$?
echo "[entrypoint] proces ($EXIT_CODE) zakonczony - wychodze"
cleanup
exit $EXIT_CODE
