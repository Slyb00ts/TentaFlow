#!/usr/bin/env bash
# =============================================================================
# Plik: prepare-vllm-model.sh
# Opis: Provisioning modelu dla vLLM gdy wymagana jest wlasna kwantyzacja NVFP4.
#       Pobiera zrodlo safetensors z HF, kwantyzuje do NVFP4 (quantize_nvfp4.py)
#       i usuwa zrodlo (oszczednosc miejsca). Idempotentne — gotowy checkpoint
#       w /data/models nie jest liczony ponownie. Logi na stderr; jedyne na
#       stdout to finalna sciezka checkpointu (entrypoint czyta przez $(...)).
#       Bez VLLM_QUANTIZE_SCHEME skrypt nie robi nic (vLLM laduje repo wprost).
# Przyklad:
#   MODEL_LOCAL=$(prepare-vllm-model.sh "$REPO" "$SCHEME")
# =============================================================================

set -euo pipefail

REPO="${1:?usage: prepare-vllm-model.sh <hf_repo> <scheme>}"
SCHEME="${2:?usage: prepare-vllm-model.sh <hf_repo> <scheme>}"

MODELS_DIR="${MODELS_DIR:-/data/models}"
QUANT_SCRIPT="${QUANT_SCRIPT:-/app/quantize_nvfp4.py}"

log() { echo "[prepare-vllm-model] $*" >&2; }

SLUG="$(echo "${REPO}__${SCHEME}" | tr '/' '_' | tr -cd 'A-Za-z0-9._-')"
DEST_DIR="$MODELS_DIR/$SLUG"
OUT_DIR="$DEST_DIR/nvfp4"
SRC_DIR="$DEST_DIR/src"

# config.json w katalogu wyjsciowym = checkpoint gotowy (idempotencja).
if [[ -f "$OUT_DIR/config.json" ]]; then
  log "NVFP4 checkpoint juz obecny: $OUT_DIR"
  echo "$OUT_DIR"
  exit 0
fi

mkdir -p "$DEST_DIR"

HF_ARGS=()
[[ -n "${HF_TOKEN:-}" ]] && HF_ARGS+=(--token "$HF_TOKEN")

log "pobieram zrodlo $REPO -> $SRC_DIR"
hf download "$REPO" --local-dir "$SRC_DIR" \
  --exclude "*.pth" "*.gguf" "original/*" "*.onnx" \
  "${HF_ARGS[@]}" >&2

log "kwantyzacja $REPO -> $SCHEME"
python3 "$QUANT_SCRIPT" --src "$SRC_DIR" --out "$OUT_DIR" --scheme "$SCHEME" >&2

log "usuwam zrodlo $SRC_DIR"
rm -rf "$SRC_DIR"

echo "$OUT_DIR"
