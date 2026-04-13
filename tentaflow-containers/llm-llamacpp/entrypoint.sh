#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Uruchamia llama-server w tle (localhost:8080) i sidecar QUIC na FG.
#       Oba procesy maja trap SIGTERM → graceful shutdown. Kill -15 sidecara
#       wylogowuje go i zatrzymuje llama-server.
# =============================================================================

set -eo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

MODEL_PATH="${MODEL_PATH:-/data/models/model.gguf}"
LLAMA_PORT="${LLAMA_PORT:-8080}"
LLAMA_ARGS="${LLAMA_ARGS:---n-gpu-layers 99 --ctx-size 8192}"

if [[ ! -f "$MODEL_PATH" ]]; then
  echo "[entrypoint] ERROR: MODEL_PATH=$MODEL_PATH nie istnieje — zamontuj /data/models z plikiem GGUF"
  exit 1
fi

echo "[entrypoint] start llama-server na 127.0.0.1:$LLAMA_PORT, model=$MODEL_PATH"
llama-server \
  --host 127.0.0.1 \
  --port "$LLAMA_PORT" \
  --model "$MODEL_PATH" \
  $LLAMA_ARGS \
  >/tmp/llama-server.log 2>&1 &

LLAMA_PID=$!
echo "[entrypoint] llama-server PID=$LLAMA_PID"

# Czekaj az llama odpowie 200 na /health (max 60s)
for i in $(seq 1 60); do
  if curl -fsS "http://127.0.0.1:$LLAMA_PORT/health" >/dev/null 2>&1; then
    echo "[entrypoint] llama-server gotowy po ${i}s"
    break
  fi
  sleep 1
done

# Trap SIGTERM — zabij llama-server gdy sidecar dostaje sygnal
cleanup() {
  echo "[entrypoint] shutdown: zabijam llama-server ($LLAMA_PID)"
  kill -TERM "$LLAMA_PID" 2>/dev/null || true
  wait "$LLAMA_PID" 2>/dev/null || true
}
trap cleanup SIGTERM SIGINT

echo "[entrypoint] start tentaflow-sidecar, config=$CONFIG_PATH"
exec /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH"
