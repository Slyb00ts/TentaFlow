#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Uruchamia trtllm-serve (TensorRT-LLM OpenAI API na 127.0.0.1:8000)
#       w tle, a na froncie sidecar QUIC. Obsluguje SIGTERM dla graceful
#       shutdown obu procesow.
# =============================================================================

set -eo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

MODEL="${MODEL:?MODEL env required, np. 'Qwen/Qwen3.5-0.8B'}"
TRTLLM_PORT="${TRTLLM_PORT:-8000}"
TRTLLM_ARGS="${TRTLLM_ARGS:---max_batch_size 8 --max_num_tokens 8192}"

echo "[entrypoint] trtllm-serve $MODEL na 127.0.0.1:$TRTLLM_PORT"
# shellcheck disable=SC2086
trtllm-serve "$MODEL" \
  --host 127.0.0.1 \
  --port "$TRTLLM_PORT" \
  $TRTLLM_ARGS \
  >/tmp/trtllm.log 2>&1 &
TRTLLM_PID=$!
echo "[entrypoint] trtllm-serve PID=$TRTLLM_PID"

# Pierwsza inicjalizacja moze potrwac dluzej (build TensorRT engine z modelu HF),
# dlatego czekamy do 15 minut.
for i in $(seq 1 900); do
  if curl -fsS "http://127.0.0.1:$TRTLLM_PORT/v1/models" >/dev/null 2>&1; then
    echo "[entrypoint] trtllm-serve gotowy po ${i}s"
    break
  fi
  sleep 1
done

cleanup() {
  echo "[entrypoint] shutdown: zabijam trtllm-serve ($TRTLLM_PID)"
  kill -TERM "$TRTLLM_PID" 2>/dev/null || true
  wait "$TRTLLM_PID" 2>/dev/null || true
}
trap cleanup SIGTERM SIGINT

exec /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH"
