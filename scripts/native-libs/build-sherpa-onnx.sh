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

case "$PLATFORM" in
  ios-arm64|ios-sim-arm64)
    # iOS: sherpa-onnx cross-compiluje przez własny toolchain (ios.toolchain.cmake),
    # a onnxruntime NIE jest budowane ze źródeł — sherpa linkuje prebuilt statyczny
    # onnxruntime z xcframework (csukuangfj/onnxruntime-libs). Wskazujemy slice przez
    # SHERPA_ONNXRUNTIME_LIB_DIR. PLATFORM=OS64 to device, SIMULATORARM64 to symulator.
    [ "$(uname -s)" = "Darwin" ] || { echo "Build iOS ($PLATFORM) wymaga macOS + Xcode." >&2; exit 1; }
    require_cmd curl tar
    ORT_IOS_VERSION="${SHERPA_ONNX_IOS_ORT_VERSION:-1.17.1}"
    if [ "$PLATFORM" = "ios-arm64" ]; then
      IOS_CMAKE_PLATFORM="OS64"
      ORT_SLICE="ios-arm64"
    else
      IOS_CMAKE_PLATFORM="SIMULATORARM64"
      ORT_SLICE="ios-arm64_x86_64-simulator"
    fi

    ORT_DIR="$NATIVE_CACHE/downloads/onnxruntime-ios-$ORT_IOS_VERSION"
    ORT_XCF="$ORT_DIR/onnxruntime.xcframework"
    if [ ! -f "$ORT_XCF/$ORT_SLICE/onnxruntime.a" ]; then
      mkdir -p "$ORT_DIR"
      ORT_TARBALL="$NATIVE_CACHE/downloads/onnxruntime.xcframework-$ORT_IOS_VERSION.tar.bz2"
      if [ ! -f "$ORT_TARBALL" ] || [ "${TENTAFLOW_NATIVE_UPDATE:-0}" = "1" ]; then
        curl -fL "https://github.com/csukuangfj/onnxruntime-libs/releases/download/v$ORT_IOS_VERSION/onnxruntime.xcframework-$ORT_IOS_VERSION.tar.bz2" -o "$ORT_TARBALL"
      fi
      tar xjf "$ORT_TARBALL" -C "$ORT_DIR"
    fi

    export SHERPA_ONNXRUNTIME_LIB_DIR="$ORT_XCF/$ORT_SLICE"
    export SHERPA_ONNXRUNTIME_INCLUDE_DIR="$ORT_XCF/Headers"

    cmake \
      -S "$SRC" -B "$BUILD" \
      -DCMAKE_TOOLCHAIN_FILE="$SRC/toolchains/ios.toolchain.cmake" \
      -DPLATFORM="$IOS_CMAKE_PLATFORM" \
      -DDEPLOYMENT_TARGET=13.0 \
      -DENABLE_BITCODE=0 \
      -DENABLE_ARC=1 \
      -DENABLE_VISIBILITY=0 \
      -DCMAKE_BUILD_TYPE=Release \
      -DBUILD_SHARED_LIBS=OFF \
      -DBUILD_PIPER_PHONMIZE_EXE=OFF \
      -DBUILD_PIPER_PHONMIZE_TESTS=OFF \
      -DBUILD_ESPEAK_NG_EXE=OFF \
      -DBUILD_ESPEAK_NG_TESTS=OFF \
      -DSHERPA_ONNX_ENABLE_TTS=ON \
      -DSHERPA_ONNX_ENABLE_PYTHON=OFF \
      -DSHERPA_ONNX_ENABLE_TESTS=OFF \
      -DSHERPA_ONNX_ENABLE_CHECK=OFF \
      -DSHERPA_ONNX_ENABLE_PORTAUDIO=OFF \
      -DSHERPA_ONNX_ENABLE_JNI=OFF \
      -DSHERPA_ONNX_ENABLE_C_API=ON \
      -DSHERPA_ONNX_ENABLE_WEBSOCKET=OFF \
      -DCMAKE_C_COMPILER_LAUNCHER= \
      -DCMAKE_CXX_COMPILER_LAUNCHER=
    cmake --build "$BUILD" -j"$(platform_cpu_count)"

    copy_matching "$BUILD" "$NATIVE_ROOT/$PLATFORM/lib-static" -name '*.a'
    cp "$SHERPA_ONNXRUNTIME_LIB_DIR/onnxruntime.a" "$NATIVE_ROOT/$PLATFORM/lib-static/libonnxruntime.a"

    mkdir -p "$NATIVE_ROOT/$PLATFORM/include/sherpa-onnx"
    find "$SRC/sherpa-onnx/c-api" "$SRC/sherpa-onnx/csrc" -type f -name '*.h' -exec cp {} "$NATIVE_ROOT/$PLATFORM/include/sherpa-onnx/" \;
    mkdir -p "$NATIVE_ROOT/$PLATFORM/include/onnxruntime"
    cp -R "$ORT_XCF/Headers/." "$NATIVE_ROOT/$PLATFORM/include/onnxruntime/"

    append_manifest_library "$PLATFORM" "sherpa-onnx" "static" "$SHERPA_ONNX_REF" "iOS (PLATFORM=$IOS_CMAKE_PLATFORM); TTS ON."
    append_manifest_library "$PLATFORM" "onnxruntime" "static" "v$ORT_IOS_VERSION" "iOS static z csukuangfj/onnxruntime-libs (slice $ORT_SLICE)."
    exit 0
    ;;
esac

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
