#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: vLLM OpenAI API (direct-http, bez sidecara). Core gada HTTP wprost do
#       host-mapped portu. vLLM bind 0.0.0.0:$PORT i biegnie jako PID1.
# =============================================================================

set -uo pipefail

MODEL="${MODEL:?MODEL env required, np. 'Qwen/Qwen2.5-0.5B-Instruct'}"
VLLM_PORT="${PORT:-${VLLM_PORT:-8000}}"

# Argumenty silnika przychodza jako "$@" (bollard Cmd array zbudowany przez
# Rust docker.rs). JSON `--speculative-config {...}` jest pojedynczym argv-em i
# zostaje nietkniety — zadnej re-tokenizacji (xargs zdejmowal wewnetrzne
# cudzyslowy -> zepsuty JSON). Defaulty (--dtype, --max-model-len, gpu-memory)
# pochodza teraz z Rust/bundle, nie z tego skryptu.
ENGINE_ARGS=("$@")

# Auto-detect liczby GPU widzialnych dla kontenera (CUDA_VISIBLE_DEVICES albo
# wszystkie z --gpus all). Default TP/PP dorzucamy TYLKO gdy user nie podal
# --tensor-parallel-size / --pipeline-parallel-size w przekazanych argach.
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
  # TP musi dzielic num_attention_heads, PP musi dzielic num_hidden_layers.
  # Dla 3/6 GPU lepiej TP=2 x PP=3 niz TP=3.
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

echo "[entrypoint] vllm serve $MODEL na 0.0.0.0:$VLLM_PORT (${#ENGINE_ARGS[@]} args)"
exec vllm serve "$MODEL" \
  --host 0.0.0.0 \
  --port "$VLLM_PORT" \
  --served-model-name "${SERVED_MODEL_NAME:-$MODEL}" \
  "${ENGINE_ARGS[@]}"
