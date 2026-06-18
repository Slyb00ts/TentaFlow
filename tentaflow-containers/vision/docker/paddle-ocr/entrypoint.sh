#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Uruchamia sidecar QUIC + serwer PaddleOCR (FastAPI) rownolegle.
#       Sidecar nasluchuje iroh natychmiast, server.py inicjalizuje silnik OCR
#       przy pierwszym zadaniu. PID 1 czeka na pierwszy padly proces.
# =============================================================================

set -uo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

export PORT="${OCR_PORT:-8095}"
export OCR_LANG="${OCR_LANG:-en}"

echo "[entrypoint] sidecar config=$CONFIG_PATH"
NO_COLOR=1 /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH" 2>&1 \
  | sed -u 's/^/[sidecar] /' &
SIDECAR_PID=$!

echo "[entrypoint] paddle-ocr server na 127.0.0.1:$PORT (lang=$OCR_LANG)"
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
