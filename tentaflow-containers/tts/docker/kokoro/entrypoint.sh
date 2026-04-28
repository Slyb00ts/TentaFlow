#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Sidecar QUIC (5000/UDP) + uvicorn server (127.0.0.1:8880) startuja
#       rownolegle. Logi obu na stdout. PID1 czeka na pierwszego upadku.
# =============================================================================

set -uo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

PORT="${KOKORO_PORT:-8880}"

echo "[entrypoint] sidecar config=$CONFIG_PATH"
NO_COLOR=1 /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH" 2>&1 \
  | sed -u 's/^/[sidecar] /' &
SIDECAR_PID=$!
echo "[entrypoint] sidecar PID=$SIDECAR_PID"

echo "[entrypoint] start kokoro server (uvicorn 127.0.0.1:$PORT)"
uvicorn server:app --host 127.0.0.1 --port "$PORT" 2>&1 \
  | sed -u 's/^/[kokoro] /' &
ENGINE_PID=$!
echo "[entrypoint] kokoro PID=$ENGINE_PID"

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
