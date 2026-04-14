#!/usr/bin/env bash
# =============================================================================
# Plik: sherpa-onnx/build.sh
# Opis: Buduje sherpa-onnx-offline-tts-server z sherpa-onnx HEAD.
#       Output: binarka + libonnxruntime.so/.dylib/.dll.
# =============================================================================

set -eo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUTPUT_DIR="${OUTPUT_DIR:-$SCRIPT_DIR/../output}"
mkdir -p "$OUTPUT_DIR"

OS="${1:-auto}"
ARCH="${2:-$(uname -m)}"
BACKEND="${3:-cpu}"   # sherpa-onnx z CUDA wymaga onnxruntime-gpu, na razie CPU default

if [[ "$OS" == "auto" ]]; then
  case "$(uname -s)" in
    Linux*)  OS=linux ;;
    Darwin*) OS=macos ;;
    MINGW*|CYGWIN*|MSYS*) OS=windows ;;
  esac
fi

BUILD_DIR="$SCRIPT_DIR/.build-$OS-$ARCH-$BACKEND"
SRC_DIR="$BUILD_DIR/sherpa-onnx"
mkdir -p "$BUILD_DIR"

if [[ ! -d "$SRC_DIR" ]]; then
  git clone --depth 1 https://github.com/k2-fsa/sherpa-onnx.git "$SRC_DIR"
else
  git -C "$SRC_DIR" pull --depth 1 origin master || true
fi

CMAKE_ARGS=(-S "$SRC_DIR" -B "$SRC_DIR/build" -DCMAKE_BUILD_TYPE=Release \
  -DSHERPA_ONNX_ENABLE_TTS=ON -DBUILD_SHARED_LIBS=OFF)
[[ "$BACKEND" == "cuda" ]] && CMAKE_ARGS+=(-DSHERPA_ONNX_ENABLE_GPU=ON)

cmake "${CMAKE_ARGS[@]}"
# sherpa-onnx-offline-tts-server byl w starszych wersjach; aktualnie zostal zastapiony
# przez offline-websocket-server + CLI sherpa-onnx-offline-tts. Sidecar doklada HTTP shim.
cmake --build "$SRC_DIR/build" \
  --target sherpa-onnx sherpa-onnx-offline-tts sherpa-onnx-offline-websocket-server \
  -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu || echo 4)"

STAGE="$BUILD_DIR/stage"
rm -rf "$STAGE"
mkdir -p "$STAGE"
# Kopiujemy obie binarki + libs
for bin in sherpa-onnx sherpa-onnx-offline-tts sherpa-onnx-offline-websocket-server; do
  cp "$SRC_DIR/build/bin/$bin"* "$STAGE/" 2>/dev/null || true
done
find "$SRC_DIR/build" -maxdepth 3 -type f \( -name "libonnxruntime*" -o -name "*.dylib" -o -name "*.dll" \) \
  -exec cp {} "$STAGE/" \; 2>/dev/null || true

OUT="$OUTPUT_DIR/sherpa-tts-$OS-$ARCH-$BACKEND.tar.gz"
(cd "$STAGE" && tar -czf "$OUT" .)
echo "[sherpa-onnx] done: $OUT"
