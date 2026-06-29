#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: vLLM w trybie rerankingu (vllm serve --task score), direct-http (bez
#       sidecara). vLLM laduje Qwen3 Reranker jako sequence-classification i
#       serwuje /v1/rerank + /v1/score na 0.0.0.0:${VLLM_PORT}.
#       Qwen3 Reranker wymaga --hf-overrides (architektura
#       Qwen3ForSequenceClassification + mapowanie tokenow yes/no) — Core
#       przekazuje go w ENGINE_ARGS dla presetu qwen3.
# =============================================================================

set -uo pipefail

MODEL="${MODEL:?MODEL env required, np. 'Qwen/Qwen3-Reranker-0.6B'}"
VLLM_PORT="${VLLM_PORT:-8000}"

ENGINE_ARGS=("$@")

HAS_PARALLEL=0
for _a in "${ENGINE_ARGS[@]}"; do
  case "$_a" in
    --tensor-parallel-size|--tensor-parallel-size=*|-tp|--pipeline-parallel-size|--pipeline-parallel-size=*) HAS_PARALLEL=1 ;;
  esac
done
if [[ "$HAS_PARALLEL" -eq 0 ]]; then
  GPU_COUNT=$(nvidia-smi -L 2>/dev/null | wc -l || echo 1)
  [[ "$GPU_COUNT" -lt 1 ]] && GPU_COUNT=1
  echo "[entrypoint] wykryto $GPU_COUNT GPU — auto TP/PP"
  case "$GPU_COUNT" in
    1) ENGINE_ARGS+=(--tensor-parallel-size 1) ;;
    2) ENGINE_ARGS+=(--tensor-parallel-size 2) ;;
    3) ENGINE_ARGS+=(--tensor-parallel-size 1 --pipeline-parallel-size 3) ;;
    4) ENGINE_ARGS+=(--tensor-parallel-size 4) ;;
    6) ENGINE_ARGS+=(--tensor-parallel-size 2 --pipeline-parallel-size 3) ;;
    8) ENGINE_ARGS+=(--tensor-parallel-size 8) ;;
    *) ENGINE_ARGS+=(--tensor-parallel-size "$GPU_COUNT") ;;
  esac
fi

echo "[entrypoint] vllm serve $MODEL na 0.0.0.0:$VLLM_PORT (--task score, ${#ENGINE_ARGS[@]} args)"
exec vllm serve "$MODEL" \
  --host 0.0.0.0 \
  --port "$VLLM_PORT" \
  --trust-remote-code \
  --served-model-name "${SERVED_MODEL_NAME:-$MODEL}" \
  "${ENGINE_ARGS[@]}"
