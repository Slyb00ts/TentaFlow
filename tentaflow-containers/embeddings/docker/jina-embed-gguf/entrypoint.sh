#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: llama-server w trybie embeddings (--embedding --pooling last),
#       direct-http (bez sidecara). Modele Jina v5 wymagaja last-token pooling,
#       dlatego --pooling last jest w baseline. Core gada HTTP wprost do
#       host-mapped portu; serwowane jest OpenAI-compatible /v1/embeddings.
# =============================================================================

set -uo pipefail

# Edytowalna komenda z wizarda (Override): gdy Core ustawi ENGINE_LAUNCH_CMD,
# odpalamy ja verbatim zamiast budowanej nizej komendy.
if [ -n "${ENGINE_LAUNCH_CMD:-}" ]; then
  echo "[entrypoint] ENGINE_LAUNCH_CMD override"
  exec sh -c "$ENGINE_LAUNCH_CMD"
fi

MODEL_PATH="${MODEL_PATH:-/data/models/model.gguf}"
LLAMA_PORT="${PORT:-${LLAMA_PORT:-8080}}"

# Argi silnika jako "$@" (bollard Cmd array z Rust), bez re-tokenizacji. Pusto
# → bezpieczny baseline embeddingow (last-token pooling wymagany przez Jina v5).
ENGINE_ARGS=("$@")
if [[ "${#ENGINE_ARGS[@]}" -eq 0 ]]; then
  ENGINE_ARGS=(--embedding --pooling last --ctx-size 8192 --ubatch-size 8192)
fi

if [[ ! -f "$MODEL_PATH" ]]; then
  echo "[entrypoint] ERROR: MODEL_PATH=$MODEL_PATH nie istnieje - zamontuj /data/models z plikiem GGUF"
  exit 1
fi

echo "[entrypoint] start llama-server (embeddings) na 0.0.0.0:$LLAMA_PORT (${#ENGINE_ARGS[@]} args)"
exec llama-server \
  --host 0.0.0.0 \
  --port "$LLAMA_PORT" \
  --model "$MODEL_PATH" \
  "${ENGINE_ARGS[@]}"
