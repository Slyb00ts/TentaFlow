#!/usr/bin/env bash
# =============================================================================
# Plik: llama-cpp/build.sh
# Opis: Buduje llama-server z llama.cpp HEAD dla podanej platformy/backendu
#       i zapisuje binarke + jej zaleznosci do ../output/<nazwa>.tar.gz
#
# Uzycie:
#   ./build.sh linux x86_64 cuda
#   ./build.sh linux aarch64 vulkan
#   ./build.sh macos aarch64 metal
#   ./build.sh windows x86_64 cuda     # (tylko w srodowisku MSVC)
#   ./build.sh auto                     # wykryj host + preferowany backend
# =============================================================================

set -eo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT_DIR="${OUTPUT_DIR:-$SCRIPT_DIR/../output}"
mkdir -p "$OUTPUT_DIR"

OS="${1:-auto}"
ARCH="${2:-$(uname -m)}"
BACKEND="${3:-auto}"

if [[ "$OS" == "auto" ]]; then
  case "$(uname -s)" in
    Linux*)   OS=linux ;;
    Darwin*)  OS=macos ;;
    MINGW*|CYGWIN*|MSYS*) OS=windows ;;
  esac
fi

if [[ "$BACKEND" == "auto" ]]; then
  case "$OS" in
    linux)   command -v nvcc >/dev/null 2>&1 && BACKEND=cuda || BACKEND=vulkan ;;
    macos)   BACKEND=metal ;;
    windows) command -v nvcc >/dev/null 2>&1 && BACKEND=cuda || BACKEND=vulkan ;;
  esac
fi

BUILD_DIR="$SCRIPT_DIR/.build-$OS-$ARCH-$BACKEND"
SRC_DIR="$BUILD_DIR/llama.cpp"

echo "[llama-cpp] os=$OS arch=$ARCH backend=$BACKEND"
mkdir -p "$BUILD_DIR"

if [[ ! -d "$SRC_DIR" ]]; then
  git clone --depth 1 https://github.com/ggml-org/llama.cpp.git "$SRC_DIR"
else
  git -C "$SRC_DIR" pull --depth 1 origin master || true
fi

CMAKE_ARGS=(-S "$SRC_DIR" -B "$SRC_DIR/build" -DCMAKE_BUILD_TYPE=Release -DLLAMA_CURL=OFF -DLLAMA_SERVER=ON)
case "$BACKEND" in
  cuda)    CMAKE_ARGS+=(-DGGML_CUDA=ON) ;;
  metal)   CMAKE_ARGS+=(-DGGML_METAL=ON) ;;
  vulkan)  CMAKE_ARGS+=(-DGGML_VULKAN=ON) ;;
  cpu)     : ;;
  *) echo "nieznany backend: $BACKEND" >&2; exit 1 ;;
esac

cmake "${CMAKE_ARGS[@]}"
cmake --build "$SRC_DIR/build" --target llama-server -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu || echo 4)"

STAGE="$BUILD_DIR/stage"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp "$SRC_DIR/build/bin/llama-server"* "$STAGE/" 2>/dev/null || true
# libs (CUDA dynamic, Vulkan loader itp.) — obok binarki
find "$SRC_DIR/build" -maxdepth 3 -type f \( -name "*.so*" -o -name "*.dylib" -o -name "*.dll" \) \
  -exec cp {} "$STAGE/" \; 2>/dev/null || true

OUT="$OUTPUT_DIR/llama-server-$OS-$ARCH-$BACKEND.tar.gz"
(cd "$STAGE" && tar -czf "$OUT" .)
echo "[llama-cpp] done: $OUT"
ls -lh "$OUT"
