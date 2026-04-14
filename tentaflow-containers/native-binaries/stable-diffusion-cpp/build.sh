#!/usr/bin/env bash
# =============================================================================
# Plik: stable-diffusion-cpp/build.sh
# Opis: Buduje sd-server z stable-diffusion.cpp HEAD. Alternatywa dla ComfyUI
#       dla prostych generacji SD/Flux bez Pythona i ekosystemu node'ow.
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
SRC_DIR="$BUILD_DIR/stable-diffusion.cpp"
mkdir -p "$BUILD_DIR"

if [[ ! -d "$SRC_DIR" ]]; then
  git clone --depth 1 --recursive https://github.com/leejet/stable-diffusion.cpp.git "$SRC_DIR"
else
  git -C "$SRC_DIR" pull --depth 1 --recurse-submodules origin master || true
fi

CMAKE_ARGS=(-S "$SRC_DIR" -B "$SRC_DIR/build" -DCMAKE_BUILD_TYPE=Release)
case "$BACKEND" in
  cuda)   CMAKE_ARGS+=(-DSD_CUDA=ON) ;;
  metal)  CMAKE_ARGS+=(-DSD_METAL=ON) ;;
  vulkan) CMAKE_ARGS+=(-DSD_VULKAN=ON) ;;
esac

cmake "${CMAKE_ARGS[@]}"
cmake --build "$SRC_DIR/build" -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu || echo 4)"

STAGE="$BUILD_DIR/stage"
rm -rf "$STAGE"
mkdir -p "$STAGE"
# sd.cpp nie ma dedykowanego serwera — uzywamy CLI sd wraperowanego przez
# sidecara ktory wystawi HTTP. Na razie embedujemy samo sd + (opcjonalnie) server.
cp "$SRC_DIR/build/bin/sd"* "$STAGE/" 2>/dev/null || true
find "$SRC_DIR/build" -maxdepth 3 -type f \( -name "*.so*" -o -name "*.dylib" -o -name "*.dll" \) \
  -exec cp {} "$STAGE/" \; 2>/dev/null || true

OUT="$OUTPUT_DIR/sd-server-$OS-$ARCH-$BACKEND.tar.gz"
(cd "$STAGE" && tar -czf "$OUT" .)
echo "[stable-diffusion-cpp] done: $OUT"
