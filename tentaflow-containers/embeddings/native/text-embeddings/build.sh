#!/usr/bin/env bash
# =============================================================================
# Plik: text-embeddings/build.sh
# Opis: Buduje text-embeddings-router (HF TEI, Rust + Candle) dla embeddings
#       i reranker. Single binarka dzieki `cargo build --release`.
# =============================================================================

set -eo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT_DIR="${OUTPUT_DIR:-$SCRIPT_DIR/../../../output}"
mkdir -p "$OUTPUT_DIR"

OS="${1:-auto}"
ARCH="${2:-$(uname -m)}"
BACKEND="${3:-auto}"

if [[ "$OS" == "auto" ]]; then
  case "$(uname -s)" in
    Linux*)  OS=linux ;;
    Darwin*) OS=macos ;;
    MINGW*|CYGWIN*|MSYS*) OS=windows ;;
  esac
fi
if [[ "$BACKEND" == "auto" ]]; then
  case "$OS" in
    linux|windows) command -v nvcc >/dev/null 2>&1 && BACKEND=cuda || BACKEND=cpu ;;
    macos)         BACKEND=metal ;;
  esac
fi

BUILD_DIR="$SCRIPT_DIR/.build-$OS-$ARCH-$BACKEND"
SRC_DIR="$BUILD_DIR/text-embeddings-inference"
mkdir -p "$BUILD_DIR"

if [[ ! -d "$SRC_DIR" ]]; then
  git clone --depth 1 https://github.com/huggingface/text-embeddings-inference.git "$SRC_DIR"
else
  git -C "$SRC_DIR" pull --depth 1 origin main || true
fi

FEATURES=()
case "$BACKEND" in
  # candle-cuda zamiast candle-cuda-turing + flash-attn bo flash-attn
  # na bleeding-edge CUDA 13+ nie buduje sie (stary cutlass w candle).
  cuda)   FEATURES+=(--no-default-features --features "candle-cuda") ;;
  metal)  FEATURES+=(--no-default-features --features "metal") ;;
  cpu|*)  FEATURES+=(--no-default-features --features "candle") ;;
esac

(cd "$SRC_DIR" && cargo build --release --bin text-embeddings-router "${FEATURES[@]}")

STAGE="$BUILD_DIR/stage"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp "$SRC_DIR/target/release/text-embeddings-router"* "$STAGE/" 2>/dev/null || true

OUT="$OUTPUT_DIR/text-embeddings-router-$OS-$ARCH-$BACKEND.tar.gz"
(cd "$STAGE" && tar -czf "$OUT" .)
echo "[text-embeddings] done: $OUT"
