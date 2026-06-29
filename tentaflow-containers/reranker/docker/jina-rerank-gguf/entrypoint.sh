#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: llama-server w trybie rerankingu (--reranking --pooling rank),
#       direct-http (bez sidecara). Serwuje OpenAI-compatible /v1/rerank na
#       0.0.0.0:${LLAMA_PORT}. v3 jest listwise — patrz UWAGA w manifescie
#       jina-rerank-gguf.toml (mainline llama.cpp moze wymagac forka hanxiao).
# =============================================================================

set -uo pipefail

if [ -n "${ENGINE_LAUNCH_CMD:-}" ]; then
  echo "[entrypoint] ENGINE_LAUNCH_CMD override"
  exec sh -c "$ENGINE_LAUNCH_CMD"
fi

MODEL_PATH="${MODEL_PATH:-/data/models/model.gguf}"
LLAMA_PORT="${PORT:-${LLAMA_PORT:-8080}}"

ENGINE_ARGS=("$@")
if [[ "${#ENGINE_ARGS[@]}" -eq 0 ]]; then
  ENGINE_ARGS=(--reranking --pooling rank --ctx-size 8192 --ubatch-size 8192)
fi

if [[ ! -f "$MODEL_PATH" ]]; then
  echo "[entrypoint] ERROR: MODEL_PATH=$MODEL_PATH nie istnieje - zamontuj /data/models z plikiem GGUF"
  exit 1
fi

echo "[entrypoint] start llama-server (reranking) na 0.0.0.0:$LLAMA_PORT (${#ENGINE_ARGS[@]} args)"
exec llama-server \
  --host 0.0.0.0 \
  --port "$LLAMA_PORT" \
  --model "$MODEL_PATH" \
  "${ENGINE_ARGS[@]}"
