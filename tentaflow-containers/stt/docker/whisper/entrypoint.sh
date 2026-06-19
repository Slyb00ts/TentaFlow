#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: whisper.cpp server direct-http (bez sidecara). Core gada HTTP wprost do
#       host-mapped portu. Bind 0.0.0.0 wewnatrz kontenera; containment robi
#       host bind. Engine laduje model i serwuje OpenAI-compatible API.
# =============================================================================

set -uo pipefail

MODEL_PATH="${MODEL_PATH:-/data/models/ggml-large-v3-q5_0.bin}"
PORT="${PORT:-8081}"
WHISPER_ARGS="${WHISPER_ARGS:---threads 4 --processors 1}"

if [[ ! -f "$MODEL_PATH" ]]; then
  echo "[entrypoint] ERROR: MODEL_PATH=$MODEL_PATH nie istnieje - zamontuj /data/models"
  exit 1
fi

echo "[entrypoint] start whisper na 0.0.0.0:$PORT"
# shellcheck disable=SC2086
exec whisper-server \
  --host 0.0.0.0 --port "$PORT" \
  --model "$MODEL_PATH" \
  $WHISPER_ARGS
