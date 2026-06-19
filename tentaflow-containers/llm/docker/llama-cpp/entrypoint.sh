#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: llama-server (direct-http, bez sidecara). Core gada HTTP wprost do
#       host-mapped portu. llama-server bind 0.0.0.0:$PORT i biegnie jako PID1.
# =============================================================================

set -uo pipefail

MODEL_PATH="${MODEL_PATH:-/data/models/model.gguf}"
LLAMA_PORT="${PORT:-${LLAMA_PORT:-8080}}"

# Argi silnika jako "$@" (bollard Cmd array z Rust), bez re-tokenizacji. Pusto
# → bezpieczny baseline.
ENGINE_ARGS=("$@")
if [[ "${#ENGINE_ARGS[@]}" -eq 0 ]]; then
  ENGINE_ARGS=(--n-gpu-layers 99 --ctx-size 8192)
fi

if [[ ! -f "$MODEL_PATH" ]]; then
  echo "[entrypoint] ERROR: MODEL_PATH=$MODEL_PATH nie istnieje - zamontuj /data/models z plikiem GGUF"
  exit 1
fi

echo "[entrypoint] start llama na 0.0.0.0:$LLAMA_PORT (${#ENGINE_ARGS[@]} args)"
exec llama-server \
  --host 0.0.0.0 \
  --port "$LLAMA_PORT" \
  --model "$MODEL_PATH" \
  "${ENGINE_ARGS[@]}"
