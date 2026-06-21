#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/native-libs/build-onnxruntime.sh
# Opis: Buduje ONNX Runtime statycznie albo pobiera oficjalny runtime dynamiczny.
# =============================================================================

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

PLATFORM="${1:-$(detect_platform)}"
# v1.26.0: 1.22.0/1.23.x maja hang w tworzeniu sesji na niektorych grafach (MoveNet,
# duze modele Supertone) — naprawione w 1.24-1.26. 1.26.0 ma prebuilty dla wszystkich
# platform (1.27.0 nie ma juz CPU win-x64). Konsument to ort (supertonic); sherpa ma
# wlasny onnxruntime (xcframework), wiec bump nie dotyka STT.
ONNXRUNTIME_REF="${ONNXRUNTIME_REF:-v1.26.0}"
MODE="${ONNXRUNTIME_MODE:-dynamic}"
prepare_layout "$PLATFORM"
require_cmd git

if [ "$MODE" = "static" ]; then
  require_cmd python3 cmake
  SRC="$(repo_checkout onnxruntime https://github.com/microsoft/onnxruntime.git "$ONNXRUNTIME_REF")"
  BUILD="$NATIVE_CACHE/build/onnxruntime-$PLATFORM-static"
  reset_dir "$BUILD"
  (
    cd "$SRC"
    ./build.sh \
      --config Release \
      --build_dir "$BUILD" \
      --parallel "$(platform_cpu_count)" \
      --skip_tests \
      --build_shared_lib off \
      --compile_no_warning_as_error
  )
  copy_matching "$BUILD" "$NATIVE_ROOT/$PLATFORM/lib-static" -name '*.a' -o -name '*.lib'
  append_manifest_library "$PLATFORM" "onnxruntime" "static" "$ONNXRUNTIME_REF" "Zbudowane z source przez build.sh."
  exit 0
fi

require_cmd curl tar
VERSION="${ONNXRUNTIME_REF#v}"
case "$PLATFORM" in
  linux-x86_64) ARCHIVE="onnxruntime-linux-x64-$VERSION.tgz" ;;
  linux-aarch64) ARCHIVE="onnxruntime-linux-aarch64-$VERSION.tgz" ;;
  macos-arm64) ARCHIVE="onnxruntime-osx-arm64-$VERSION.tgz" ;;
  windows-x86_64) ARCHIVE="onnxruntime-win-x64-$VERSION.zip" ;;
  *) echo "Brak oficjalnej paczki ONNX Runtime dla $PLATFORM" >&2; exit 1 ;;
esac

URL="https://github.com/microsoft/onnxruntime/releases/download/$ONNXRUNTIME_REF/$ARCHIVE"
DOWNLOAD_DIR="$NATIVE_CACHE/downloads"
mkdir -p "$DOWNLOAD_DIR"
ARCHIVE_PATH="$DOWNLOAD_DIR/$ARCHIVE"

if [ ! -f "$ARCHIVE_PATH" ] || [ "${TENTAFLOW_NATIVE_UPDATE:-0}" = "1" ]; then
  curl -fL "$URL" -o "$ARCHIVE_PATH"
fi

UNPACK="$NATIVE_CACHE/build/onnxruntime-$PLATFORM-dynamic"
reset_dir "$UNPACK"
case "$ARCHIVE" in
  *.tgz) tar -xzf "$ARCHIVE_PATH" -C "$UNPACK" --strip-components=1 ;;
  *.zip)
    require_cmd unzip
    unzip -q "$ARCHIVE_PATH" -d "$UNPACK/raw"
    first_dir="$(find "$UNPACK/raw" -mindepth 1 -maxdepth 1 -type d | head -n1)"
    cp -Rf "$first_dir/"* "$UNPACK/"
    ;;
esac

mkdir -p "$NATIVE_ROOT/$PLATFORM/include/onnxruntime"
cp -Rf "$UNPACK/include/"* "$NATIVE_ROOT/$PLATFORM/include/onnxruntime/"
# Usun stare wersje runtime'u zanim skopiujemy nowa — inaczej po bumpie wersji w
# lib-dynamic leza dwa pliki (np. libonnxruntime.so.1.22.0 + .1.26.0), a ort
# load-dynamic (probe w supertonic.rs) wybiera niedeterministycznie.
rm -f "$NATIVE_ROOT/$PLATFORM/lib-dynamic"/libonnxruntime*.so* \
      "$NATIVE_ROOT/$PLATFORM/lib-dynamic"/libonnxruntime*.dylib \
      "$NATIVE_ROOT/$PLATFORM/lib-dynamic"/onnxruntime.dll 2>/dev/null || true
copy_matching "$UNPACK" "$NATIVE_ROOT/$PLATFORM/lib-dynamic" -name 'libonnxruntime*.so*' -o -name 'libonnxruntime*.dylib' -o -name 'onnxruntime.dll'

append_manifest_library "$PLATFORM" "onnxruntime" "dynamic" "$ONNXRUNTIME_REF" "Domyślnie pobierany oficjalny runtime; ustaw ONNXRUNTIME_MODE=static aby budować ze źródeł."
