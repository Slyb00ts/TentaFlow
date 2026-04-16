#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Uruchamia vLLM OpenAI API server w tle (127.0.0.1:8000) + sidecar QUIC.
# =============================================================================

set -eo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

MODEL="${MODEL:?MODEL env required, np. 'speakleash/Bielik-11B-v2.6-Instruct-AWQ'}"
VLLM_PORT="${VLLM_PORT:-8000}"
VLLM_ARGS="${VLLM_ARGS:---dtype auto --gpu-memory-utilization 0.9 --max-model-len 8192}"

echo "[entrypoint] vllm serve $MODEL na 127.0.0.1:$VLLM_PORT"
# shellcheck disable=SC2086
vllm serve "$MODEL" \
  --host 127.0.0.1 \
  --port "$VLLM_PORT" \
  $VLLM_ARGS \
  >/tmp/vllm.log 2>&1 &
VLLM_PID=$!
echo "[entrypoint] vllm PID=$VLLM_PID"

# vLLM startuje dlugo — czekamy do 10 min
for i in $(seq 1 600); do
  if curl -fsS "http://127.0.0.1:$VLLM_PORT/v1/models" >/dev/null 2>&1; then
    echo "[entrypoint] vllm gotowy po ${i}s"
    break
  fi
  sleep 1
done

cleanup() {
  echo "[entrypoint] shutdown: zabijam vllm ($VLLM_PID)"
  kill -TERM "$VLLM_PID" 2>/dev/null || true
  wait "$VLLM_PID" 2>/dev/null || true
}
trap cleanup SIGTERM SIGINT

exec /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH"
