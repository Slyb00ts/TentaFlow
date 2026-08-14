#!/usr/bin/env bash
set -euo pipefail

MODEL_DIR="${TENTAFLOW_MUSE_DIR:-/opt/TentaFlow/.runtime/models/muse-glimmer-30b}"
mkdir -p "$MODEL_DIR"

BASE_URL="https://huggingface.co/meta-models/Muse-Glimmer-30B-GGUF/resolve/main"
ASSISTANT_URL="https://huggingface.co/meta-models/Muse-Glimmer-30B-assistant/resolve/main"

download() {
  local url="$1"
  local name="$2"
  local expected_bytes="$3"
  local destination="$MODEL_DIR/$name"

  echo "Downloading $name"
  curl --fail --location --retry 5 --retry-delay 3 --continue-at - \
    "$url/$name" --output "$destination"
  local actual_bytes
  actual_bytes="$(wc -c < "$destination" | tr -d '[:space:]')"
  if [[ "$actual_bytes" != "$expected_bytes" ]]; then
    echo "size check failed for $name: expected $expected_bytes, got $actual_bytes" >&2
    exit 1
  fi
}

# The 17 GB target is the variant intended for 24 GB cards. Set
# TENTAFLOW_MUSE_INCLUDE_DYNAMIC=1 to also fetch the 32 GB target.
download "$BASE_URL" "muse-glimmer-30B-kquant-17gb.gguf" 16756681056
download "$BASE_URL" "dflash-kquant.gguf" 1631205312
download "$BASE_URL" "mmproj-kquant.gguf" 1400328928
download "$ASSISTANT_URL" "config.json" 883
download "$ASSISTANT_URL" "model.safetensors" 5111976608

if [[ "${TENTAFLOW_MUSE_INCLUDE_DYNAMIC:-0}" == 1 ]]; then
  download "$BASE_URL" "muse-glimmer-30B-kquant-dynamic.gguf" 19653957984
fi

echo "Muse Glimmer files are available in $MODEL_DIR"
