#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh (vllm-spark)
# Opis: Identyczny lifecycle co `llm/docker/vllm/entrypoint.sh` (sidecar QUIC
#       + vllm OpenAI API rownolegle), ale z DGX Spark env baseline dla
#       GB10/SM121 i stabilnego startu bez FlashInfer autotune.
# =============================================================================

set -uo pipefail

# Spark-specific runtime env. Te same wartosci sa w bundle.toml [launch.env]
# dla deploy.native — duplikujemy tu zeby docker dzialal niezaleznie od bundla.
export TORCH_CUDA_ARCH_LIST="${TORCH_CUDA_ARCH_LIST:-12.1a}"
export VLLM_USE_FLASHINFER_MXFP4_MOE="${VLLM_USE_FLASHINFER_MXFP4_MOE:-1}"
export VLLM_SKIP_P2P_CHECK="${VLLM_SKIP_P2P_CHECK:-1}"
export TRITON_PTXAS_PATH="${TRITON_PTXAS_PATH:-/usr/local/cuda/bin/ptxas}"

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

GPU_MEMORY_UTILIZATION="${GPU_MEMORY_UTILIZATION:-0.9}"
VLLM_ARGS="${VLLM_ARGS:---dtype auto --gpu-memory-utilization $GPU_MEMORY_UTILIZATION --max-model-len 8192 --max-num-batched-tokens 8192 --enable-chunked-prefill --enable-prefix-caching --no-enable-flashinfer-autotune $AUTO_PARALLEL}"

echo "[entrypoint] sidecar config=$CONFIG_PATH"
NO_COLOR=1 /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH" 2>&1 \
  | sed -u 's/^/[sidecar] /' &
SIDECAR_PID=$!
echo "[entrypoint] sidecar PID=$SIDECAR_PID"

# Tokenizacja VLLM_ARGS respektujaca cudzyslowy — jak native (shlex::split).
# `xargs` zdejmuje single-quotes wokol --speculative-config '{...}' i NIE
# wykonuje podstawien -> bezpieczne, bez `eval`. Surowe `$VLLM_ARGS` zostawialo
# literalne apostrofy -> zepsuty JSON. Dziala z/bez speculative, 1/wiele GPU, TP.
VLLM_ARG_ARR=()
while IFS= read -r _a; do VLLM_ARG_ARR+=("$_a"); done < <(xargs -n1 printf '%s\n' <<< "$VLLM_ARGS")

echo "[entrypoint] vllm serve $MODEL na 127.0.0.1:$VLLM_PORT (sm_121a, ${#VLLM_ARG_ARR[@]} args)"
vllm serve "$MODEL" \
  --host 127.0.0.1 \
  --port "$VLLM_PORT" \
  "${VLLM_ARG_ARR[@]}" 2>&1 \
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
