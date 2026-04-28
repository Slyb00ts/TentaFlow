#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Sidecar QUIC + comfy startuja rownolegle. Sidecar nasluchuje
#       iroh natychmiast (klient widzi peera od razu); engine laduje model w
#       tle. Logi obu na stdout kontenera. PID1 czeka na pierwszego upadku
#       i grzecznie konczy drugiego.
# =============================================================================

set -uo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

PORT="${COMFY_PORT:-8188}"
cd /opt/ComfyUI

echo "[entrypoint] sidecar config=$CONFIG_PATH"
NO_COLOR=1 /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH" 2>&1 \
  | sed -u 's/^/[sidecar] /' &
SIDECAR_PID=$!
echo "[entrypoint] sidecar PID=$SIDECAR_PID"

echo "[entrypoint] start comfy"
python3 main.py --listen 127.0.0.1 --port "$PORT" 2>&1 \
  | sed -u 's/^/[comfy] /' &
ENGINE_PID=$!
echo "[entrypoint] comfy PID=$ENGINE_PID"

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
