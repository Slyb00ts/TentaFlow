#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Sidecar QUIC + serwer detekcji YOLOX startuja rownolegle. Sidecar
#       nasluchuje iroh natychmiast, serwer pobiera wagi .pth dla MODEL_REPO,
#       buduje siec YOLOX i laduje ja na GPU (inferencja w PyTorch, bez
#       onnxruntime). Logi obu na stdout. PID1 czeka na pierwszego upadku i
#       grzecznie konczy drugiego.
# =============================================================================

set -uo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

PORT="${NEMOTRON_YOLOX_PORT:-8086}"

# Core wstrzykuje repo modelu jako env MODEL; serwer yolox czyta MODEL_REPO
# (jeden obraz, trzy modele wybierane repo). Mapujemy MODEL -> MODEL_REPO.
export MODEL_REPO="${MODEL_REPO:-$MODEL}"

echo "[entrypoint] sidecar config=$CONFIG_PATH"
NO_COLOR=1 /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH" 2>&1 \
  | sed -u 's/^/[sidecar] /' &
SIDECAR_PID=$!
echo "[entrypoint] sidecar PID=$SIDECAR_PID"

echo "[entrypoint] start nemotron-yolox (model=${MODEL_REPO:-?})"
PORT="$PORT" python /app/server.py 2>&1 \
  | sed -u 's/^/[nemotron-yolox] /' &
ENGINE_PID=$!
echo "[entrypoint] nemotron-yolox PID=$ENGINE_PID"

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
