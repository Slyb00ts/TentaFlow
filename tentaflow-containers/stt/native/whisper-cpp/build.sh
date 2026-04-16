#!/usr/bin/env bash
# =============================================================================
# Plik: whisper-cpp/build.sh
# Opis: Buduje whisper-server z whisper.cpp HEAD (GGML backend shared z llama.cpp).
#       Wywolanie identyczne jak llama-cpp/build.sh.
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
    linux|windows) command -v nvcc >/dev/null 2>&1 && BACKEND=cuda || BACKEND=vulkan ;;
    macos)         BACKEND=metal ;;
  esac
fi

BUILD_DIR="$SCRIPT_DIR/.build-$OS-$ARCH-$BACKEND"
SRC_DIR="$BUILD_DIR/whisper.cpp"
mkdir -p "$BUILD_DIR"

if [[ ! -d "$SRC_DIR" ]]; then
  git clone --depth 1 https://github.com/ggml-org/whisper.cpp.git "$SRC_DIR"
else
  git -C "$SRC_DIR" pull --depth 1 origin master || true
fi

CMAKE_ARGS=(-S "$SRC_DIR" -B "$SRC_DIR/build" -DCMAKE_BUILD_TYPE=Release -DWHISPER_SERVER=ON)
case "$BACKEND" in
  cuda)   CMAKE_ARGS+=(-DGGML_CUDA=ON) ;;
  metal)  CMAKE_ARGS+=(-DGGML_METAL=ON) ;;
  vulkan) CMAKE_ARGS+=(-DGGML_VULKAN=ON) ;;
  cpu)    : ;;
esac

cmake "${CMAKE_ARGS[@]}"
cmake --build "$SRC_DIR/build" --target whisper-server -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu || echo 4)"

STAGE="$BUILD_DIR/stage"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp "$SRC_DIR/build/bin/whisper-server"* "$STAGE/" 2>/dev/null || true
find "$SRC_DIR/build" -maxdepth 3 -type f \( -name "*.so*" -o -name "*.dylib" -o -name "*.dll" \) \
  -exec cp {} "$STAGE/" \; 2>/dev/null || true

OUT="$OUTPUT_DIR/whisper-server-$OS-$ARCH-$BACKEND.tar.gz"
(cd "$STAGE" && tar -czf "$OUT" .)
echo "[whisper-cpp] done: $OUT"
