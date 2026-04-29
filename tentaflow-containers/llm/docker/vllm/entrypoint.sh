#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Uruchamia sidecar QUIC + vLLM OpenAI API rownolegle. Sidecar nasluchuje
#       iroh natychmiast (klient tentaflow widzi peera od razu); vLLM laduje
#       model w tle. Logi obu procesow trafiaja na stdout kontenera z prefixem.
#       PID 1 czeka na pierwszego z procesow ktory padnie - druga konczymy
#       grzecznie i exit z jego kodem (docker restart policy decyduje co dalej).
# =============================================================================

set -uo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

MODEL="${MODEL:?MODEL env required, np. 'Qwen/Qwen2.5-0.5B-Instruct'}"
VLLM_PORT="${VLLM_PORT:-8000}"
VLLM_ARGS="${VLLM_ARGS:---dtype auto --gpu-memory-utilization 0.9 --max-model-len 8192 --max-num-batched-tokens 8192 --enable-chunked-prefill}"

echo "[entrypoint] sidecar config=$CONFIG_PATH"
NO_COLOR=1 /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH" 2>&1 \
  | sed -u 's/^/[sidecar] /' &
SIDECAR_PID=$!
echo "[entrypoint] sidecar PID=$SIDECAR_PID"

echo "[entrypoint] vllm serve $MODEL na 127.0.0.1:$VLLM_PORT"
# shellcheck disable=SC2086
vllm serve "$MODEL" \
  --host 127.0.0.1 \
  --port "$VLLM_PORT" \
  $VLLM_ARGS 2>&1 \
  | sed -u 's/^/[vllm] /' &
VLLM_PID=$!
echo "[entrypoint] vllm PID=$VLLM_PID"

cleanup() {
  echo "[entrypoint] shutdown sidecar=$SIDECAR_PID vllm=$VLLM_PID"
  kill -TERM "$SIDECAR_PID" 2>/dev/null || true
  kill -TERM "$VLLM_PID" 2>/dev/null || true
  wait "$SIDECAR_PID" 2>/dev/null || true
  wait "$VLLM_PID" 2>/dev/null || true
}
trap cleanup SIGTERM SIGINT

wait -n "$SIDECAR_PID" "$VLLM_PID"
EXIT_CODE=$?
echo "[entrypoint] proces ($EXIT_CODE) zakonczony - wychodze"
cleanup
exit $EXIT_CODE
