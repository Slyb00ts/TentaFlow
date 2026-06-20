#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: whisper.cpp server direct-http (bez sidecara). Core gada HTTP wprost do
#       host-mapped portu. whisper.cpp uzywa modeli w formacie ggml (NIE HF
#       safetensors), wiec entrypoint pobiera odpowiedni ggml z repo
#       ggerganov/whisper.cpp gdy go brak. Preset (env MODEL = repo openai/...)
#       mapujemy na wariant ggml; mozna nadpisac WHISPER_GGML_MODEL.
# =============================================================================

set -uo pipefail

PORT="${PORT:-8081}"
WHISPER_ARGS="${WHISPER_ARGS:---threads 4 --processors 1}"
MODELS_DIR="/data/models"

# Wybor pliku ggml: jawny WHISPER_GGML_MODEL > heurystyka z MODEL (preset) > turbo.
GGML_FILE="${WHISPER_GGML_MODEL:-}"
if [[ -z "$GGML_FILE" ]]; then
  case "${MODEL:-}" in
    *base*)        GGML_FILE="ggml-base.bin" ;;
    *turbo*)       GGML_FILE="ggml-large-v3-turbo-q5_0.bin" ;;
    *large-v3*|*) GGML_FILE="ggml-large-v3-turbo-q5_0.bin" ;;
  esac
fi

MODEL_PATH="${MODEL_PATH:-$MODELS_DIR/$GGML_FILE}"

if [[ ! -f "$MODEL_PATH" ]]; then
  mkdir -p "$MODELS_DIR"
  URL="https://huggingface.co/ggerganov/whisper.cpp/resolve/main/$GGML_FILE"
  echo "[entrypoint] pobieram model ggml: $URL"
  curl -fL --retry 3 -o "$MODEL_PATH" "$URL" || {
    echo "[entrypoint] ERROR: pobranie modelu $GGML_FILE nie powiodlo sie"
    exit 1
  }
fi

echo "[entrypoint] start whisper na 0.0.0.0:$PORT (model=$MODEL_PATH)"
# --inference-path: whisper.cpp domyslnie serwuje na /inference, a Core
# (api=openai-compatible) POST-uje na /v1/audio/transcriptions. Multipart `file`
# + odpowiedz {"text":...} sa zgodne, wiec wystarczy zmienic sciezke.
# shellcheck disable=SC2086
exec whisper-server \
  --host 0.0.0.0 --port "$PORT" \
  --model "$MODEL_PATH" \
  --inference-path /v1/audio/transcriptions \
  $WHISPER_ARGS
