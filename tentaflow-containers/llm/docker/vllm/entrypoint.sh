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

# Auto-detect liczby GPU widzialnych dla kontenera (CUDA_VISIBLE_DEVICES albo
# wszystkie z --gpus all). Ustawia TP automatycznie gdy user nie wymusil.
GPU_COUNT=$(nvidia-smi -L 2>/dev/null | wc -l || echo 1)
[[ "$GPU_COUNT" -lt 1 ]] && GPU_COUNT=1
echo "[entrypoint] wykryto $GPU_COUNT GPU widocznych dla kontenera"

# Default TP/PP wedlug GPU count. User moze nadpisac w VLLM_ARGS.
# Uwaga: TP musi dzielic num_attention_heads modelu, PP musi dzielic
# num_hidden_layers. Dla 3/6/12 GPU lepiej uzyc TP=2 x PP=3 niz TP=3.
case "$GPU_COUNT" in
  1) AUTO_PARALLEL="--tensor-parallel-size 1" ;;
  2) AUTO_PARALLEL="--tensor-parallel-size 2" ;;
  3) AUTO_PARALLEL="--tensor-parallel-size 1 --pipeline-parallel-size 3" ;;
  4) AUTO_PARALLEL="--tensor-parallel-size 4" ;;
  6) AUTO_PARALLEL="--tensor-parallel-size 2 --pipeline-parallel-size 3" ;;
  8) AUTO_PARALLEL="--tensor-parallel-size 8" ;;
  *) AUTO_PARALLEL="--tensor-parallel-size $GPU_COUNT" ;;
esac

VLLM_ARGS="${VLLM_ARGS:---dtype auto --gpu-memory-utilization 0.9 --max-model-len 8192 --max-num-batched-tokens 8192 --enable-chunked-prefill $AUTO_PARALLEL}"

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
