#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/native-libs/build-sherpa-onnx.sh
# Opis: Buduje sherpa-onnx; statyczne archiwa trafiają do lib-static, runtime do lib-dynamic.
# =============================================================================

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

PLATFORM="${1:-$(detect_platform)}"
SHERPA_ONNX_REF="${SHERPA_ONNX_REF:-v1.12.9}"
BACKEND="${SHERPA_ONNX_BACKEND:-cpu}"
prepare_layout "$PLATFORM"
require_cmd git cmake

SRC="$(repo_checkout sherpa-onnx https://github.com/k2-fsa/sherpa-onnx.git "$SHERPA_ONNX_REF")"
BUILD="$NATIVE_CACHE/build/sherpa-onnx-$PLATFORM-$BACKEND"
reset_dir "$BUILD"

CMAKE_ARGS=(
  -S "$SRC"
  -B "$BUILD"
  -DCMAKE_BUILD_TYPE=Release
  -DCMAKE_POSITION_INDEPENDENT_CODE=ON
  -DBUILD_SHARED_LIBS=OFF
  -DSHERPA_ONNX_ENABLE_TTS=ON
  -DSHERPA_ONNX_ENABLE_PYTHON=OFF
  -DSHERPA_ONNX_ENABLE_TESTS=OFF
  -DCMAKE_C_COMPILER_LAUNCHER=
  -DCMAKE_CXX_COMPILER_LAUNCHER=
  -DCMAKE_CUDA_COMPILER_LAUNCHER=
)

if [ "$BACKEND" = "cuda" ]; then
  CMAKE_ARGS+=(-DSHERPA_ONNX_ENABLE_GPU=ON)
fi

cmake "${CMAKE_ARGS[@]}"
cmake --build "$BUILD" -j"$(platform_cpu_count)"

copy_matching "$BUILD" "$NATIVE_ROOT/$PLATFORM/lib-static" -name '*.a' -o -name '*.lib'
copy_matching "$BUILD" "$NATIVE_ROOT/$PLATFORM/lib-dynamic" -name 'libonnxruntime*' -o -name '*.dll' -o -name '*.dylib' -o -name '*.so*'

mkdir -p "$NATIVE_ROOT/$PLATFORM/include/sherpa-onnx"
find "$SRC/sherpa-onnx/c-api" "$SRC/sherpa-onnx/csrc" -type f -name '*.h' -exec cp {} "$NATIVE_ROOT/$PLATFORM/include/sherpa-onnx/" \;

append_manifest_library "$PLATFORM" "sherpa-onnx" "static-preferred" "$SHERPA_ONNX_REF" "Backend: $BACKEND. ONNX Runtime może pozostać biblioteką dynamiczną."
