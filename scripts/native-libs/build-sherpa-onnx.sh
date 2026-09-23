#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/native-libs/build-sherpa-onnx.sh
# Opis: Buduje sherpa-onnx; statyczne archiwa trafiają do lib-static, runtime do lib-dynamic.
# =============================================================================

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

PLATFORM="${1:-$(detect_platform)}"
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
    ORT_IOS_VERSION="$(require_version SHERPA_ONNX_IOS_ORT_VERSION)"
    if [ "$PLATFORM" = "ios-arm64" ]; then
      IOS_CMAKE_PLATFORM="OS64"
      ORT_SLICE="ios-arm64"
    else
      IOS_CMAKE_PLATFORM="SIMULATORARM64"
      ORT_SLICE="ios-arm64_x86_64-simulator"
    fi

    # Static xcframework: every slice holds onnxruntime.framework whose
    # `onnxruntime` member is the static archive (same layout build-ios.sh of
    # sherpa-onnx consumes).
    ORT_DIR="$NATIVE_CACHE/downloads/onnxruntime-ios-$ORT_IOS_VERSION"
    ORT_XCF="$ORT_DIR/onnxruntime.xcframework"
    ORT_FRAMEWORK="$ORT_XCF/$ORT_SLICE/onnxruntime.framework"
    if [ ! -f "$ORT_FRAMEWORK/onnxruntime" ]; then
      require_cmd unzip
      ORT_ZIP="$NATIVE_CACHE/downloads/onnxruntime-ios-static-xcframework-$ORT_IOS_VERSION.xcframework.zip"
      mkdir -p "$ORT_DIR"
      if [ ! -f "$ORT_ZIP" ] || [ "${TENTAFLOW_NATIVE_UPDATE:-0}" = "1" ]; then
        curl -fL "https://github.com/csukuangfj/onnxruntime-libs/releases/download/v$ORT_IOS_VERSION/$(basename "$ORT_ZIP")" -o "$ORT_ZIP"
      fi
      ORT_EXPECTED="$(pinned_checksum SHERPA_ONNX_IOS_ORT_SHA256 SHERPA_ONNX_IOS_ORT_VERSION)"
      if [ "$(sha256_of "$ORT_ZIP")" != "$ORT_EXPECTED" ]; then
        echo "BLAD: SHA256 $(basename "$ORT_ZIP") nie zgadza sie z scripts/versions.env" >&2
        rm -f "$ORT_ZIP"
        exit 1
      fi
      unzip -qo "$ORT_ZIP" -d "$ORT_DIR"
    fi

    export SHERPA_ONNXRUNTIME_LIB_DIR="$ORT_XCF/$ORT_SLICE"
    export SHERPA_ONNXRUNTIME_INCLUDE_DIR="$ORT_FRAMEWORK/Headers"

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
    # Slice symulatora w xcframework jest fat (x86_64 + arm64), a rustc/LLVM nie
    # linkuje uniwersalnych archiwów ("Unsupported archive identifier") — wycinamy
    # czysty arm64. Slice device jest już single-arch, więc cp wystarcza.
    ORT_DST="$NATIVE_ROOT/$PLATFORM/lib-static/libonnxruntime.a"
    # Uwaga: dla cienkiego archiwum lipo wypisuje "Non-fat file: ...", co zawiera
    # podłańcuch "fat file" — dlatego dopasowujemy dokładną frazę pliku uniwersalnego.
    if lipo -info "$ORT_FRAMEWORK/onnxruntime" 2>/dev/null | grep -q 'Architectures in the fat file'; then
      lipo "$ORT_FRAMEWORK/onnxruntime" -thin arm64 -output "$ORT_DST"
    else
      cp "$ORT_FRAMEWORK/onnxruntime" "$ORT_DST"
    fi

    mkdir -p "$NATIVE_ROOT/$PLATFORM/include/sherpa-onnx"
    find "$SRC/sherpa-onnx/c-api" "$SRC/sherpa-onnx/csrc" -type f -name '*.h' -exec cp {} "$NATIVE_ROOT/$PLATFORM/include/sherpa-onnx/" \;
    mkdir -p "$NATIVE_ROOT/$PLATFORM/include/onnxruntime"
    cp -R "$SHERPA_ONNXRUNTIME_INCLUDE_DIR/." "$NATIVE_ROOT/$PLATFORM/include/onnxruntime/"

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

case "$PLATFORM" in
  windows-*)
    CMAKE_ARGS+=(
      # Rust links the dynamic CRT (/MD); sherpa defaults to /MT, and one
      # static library built against the other CRT fails the final link with
      # LNK2038. This also selects the MD flavour of its static onnxruntime.
      -DSHERPA_ONNX_USE_STATIC_CRT=OFF
      # cmake/onnxruntime-win-x64-static.cmake accepts only x64 and reads the
      # platform from this variable, which only the Visual Studio generator
      # sets; the Ninja build is x64 all the same.
      -DCMAKE_VS_PLATFORM_NAME=x64
      # Only the libraries are linked; the servers, binaries and JNI would add
      # asio/websocket builds for nothing.
      -DSHERPA_ONNX_ENABLE_WEBSOCKET=OFF
      -DSHERPA_ONNX_ENABLE_BINARY=OFF
      -DSHERPA_ONNX_ENABLE_JNI=OFF
    )
    ;;
esac

cmake "${CMAKE_ARGS[@]}"
cmake --build "$BUILD" -j"$(platform_cpu_count)"

copy_matching "$BUILD" "$NATIVE_ROOT/$PLATFORM/lib-static" -name '*.a' -o -name '*.lib'
copy_matching "$BUILD" "$NATIVE_ROOT/$PLATFORM/lib-dynamic" -name 'libonnxruntime*' -o -name '*.dll' -o -name '*.dylib' -o -name '*.so*'

mkdir -p "$NATIVE_ROOT/$PLATFORM/include/sherpa-onnx"
find "$SRC/sherpa-onnx/c-api" "$SRC/sherpa-onnx/csrc" -type f -name '*.h' -exec cp -f {} "$NATIVE_ROOT/$PLATFORM/include/sherpa-onnx/" \;

append_manifest_library "$PLATFORM" "sherpa-onnx" "static-preferred" "$SHERPA_ONNX_REF" "Backend: $BACKEND. ONNX Runtime może pozostać biblioteką dynamiczną."
