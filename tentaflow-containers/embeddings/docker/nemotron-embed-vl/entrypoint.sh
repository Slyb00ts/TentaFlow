#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Uruchamia sidecar QUIC + vLLM w trybie embeddings multimodalnych
#       (vllm --task embed) rownolegle. Sidecar nasluchuje iroh natychmiast;
#       vLLM laduje multimodalny model Nemotron Embed VL w tle i serwuje
#       /v1/embeddings dla tekstu i obrazow. Flagi --trust-remote-code oraz
#       --limit-mm-per-prompt wlaczaja sciezke obrazow w vLLM. Logi obu procesow
#       trafiaja na stdout z prefixem. PID 1 czeka na pierwszy padly proces,
#       drugi konczy grzecznie i wychodzi z jego kodem.
# =============================================================================

set -uo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

MODEL="${MODEL:?MODEL env required, np. 'nvidia/llama-nemotron-embed-vl-1b-v2'}"
VLLM_PORT="${VLLM_PORT:-8000}"

# Maksymalna liczba obrazow na pojedynczy request embeddingu. Konfigurowalne
# przez env z deploy configu; domyslnie 1 (typowy embedding pojedynczego obrazu).
MM_IMAGE_LIMIT="${MM_IMAGE_LIMIT:-1}"

# Argumenty silnika przychodza jako "$@" (bollard Cmd array zbudowany przez
# Rust docker.rs). Defaulty (--dtype, --max-model-len, gpu-memory) pochodza
# z Rust/bundle, nie z tego skryptu.
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

echo "[entrypoint] sidecar config=$CONFIG_PATH"
NO_COLOR=1 /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH" 2>&1 \
  | sed -u 's/^/[sidecar] /' &
SIDECAR_PID=$!
echo "[entrypoint] sidecar PID=$SIDECAR_PID"

echo "[entrypoint] vllm serve $MODEL na 127.0.0.1:$VLLM_PORT (--task embed multimodal, ${#ENGINE_ARGS[@]} args)"
vllm serve "$MODEL" \
  --host 127.0.0.1 \
  --port "$VLLM_PORT" \
  --trust-remote-code \
  --limit-mm-per-prompt "{\"image\": ${MM_IMAGE_LIMIT}}" \
  --served-model-name "${SERVED_MODEL_NAME:-$MODEL}" \
  "${ENGINE_ARGS[@]}" 2>&1 \
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
