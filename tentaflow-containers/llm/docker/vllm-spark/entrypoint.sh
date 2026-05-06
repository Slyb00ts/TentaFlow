#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh (vllm-spark)
# Opis: Identyczny lifecycle co `llm/docker/vllm/entrypoint.sh` (sidecar QUIC
#       + vllm OpenAI API rownolegle), ale z DGX Spark env baseline:
#       TORCH_CUDA_ARCH_LIST=12.1a + VLLM_USE_FLASHINFER_MXFP4_MOE=1 zeby
#       runtime nie cofal sie do sm_120 forward-compat na FP8/Mamba kernelach.
# =============================================================================

set -uo pipefail

# Spark-specific runtime env. Te same wartosci sa w bundle.toml [launch.env]
# dla deploy.native — duplikujemy tu zeby docker dzialal niezaleznie od bundla.
export TORCH_CUDA_ARCH_LIST="${TORCH_CUDA_ARCH_LIST:-12.1a}"
export VLLM_USE_FLASHINFER_MXFP4_MOE="${VLLM_USE_FLASHINFER_MXFP4_MOE:-1}"
# FlashInfer attention backend — natywne sm_121 kernele.
export VLLM_ATTENTION_BACKEND="${VLLM_ATTENTION_BACKEND:-FLASHINFER}"

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

MODEL="${MODEL:?MODEL env required, np. 'Qwen/Qwen3.5-0.8B'}"
VLLM_PORT="${VLLM_PORT:-8000}"

GPU_COUNT=$(nvidia-smi -L 2>/dev/null | wc -l || echo 1)
[[ "$GPU_COUNT" -lt 1 ]] && GPU_COUNT=1
echo "[entrypoint] DGX Spark vllm — GPU widocznych: $GPU_COUNT"

# DGX Spark to single-GPU SoC (jeden GB10) — TP=1 to default. Multi-Spark
# mesh nie idzie przez jeden kontener, wiec nie kombinujemy z PP.
case "$GPU_COUNT" in
  1) AUTO_PARALLEL="--tensor-parallel-size 1" ;;
  *) AUTO_PARALLEL="--tensor-parallel-size $GPU_COUNT" ;;
esac

VLLM_ARGS="${VLLM_ARGS:---dtype auto --gpu-memory-utilization 0.9 --max-model-len 8192 --max-num-batched-tokens 8192 --enable-chunked-prefill --enable-prefix-caching --enable-flashinfer-autotune $AUTO_PARALLEL}"

echo "[entrypoint] sidecar config=$CONFIG_PATH"
NO_COLOR=1 /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH" 2>&1 \
  | sed -u 's/^/[sidecar] /' &
SIDECAR_PID=$!
echo "[entrypoint] sidecar PID=$SIDECAR_PID"

echo "[entrypoint] vllm serve $MODEL na 127.0.0.1:$VLLM_PORT (sm_121a)"
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
