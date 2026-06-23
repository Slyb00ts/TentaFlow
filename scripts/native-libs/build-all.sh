#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/native-libs/build-all.sh
# Opis: Buduje natywne biblioteki dla wykrytej platformy. Na macOS bez jawnego
#       --platform buduje też artefakty iOS (device), bo deweloperka iOS odbywa
#       się z Maca — inaczej build wersji iOS pada na brak native-libs/ios-arm64/.
# =============================================================================

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

ONLY=""
EXPLICIT_PLATFORM=""
BUILD_IOS_SIM=0

while [ "$#" -gt 0 ]; do
  case "$1" in
    --platform)
      EXPLICIT_PLATFORM="${2:?Brak wartości dla --platform}"
      shift 2
      ;;
    --only)
      ONLY="${2:?Brak wartości dla --only}"
      shift 2
      ;;
    --ios-sim)
      # Dołóż slice symulatora iOS (ios-sim-arm64) obok device'a.
      BUILD_IOS_SIM=1
      shift
      ;;
    --update)
      export TENTAFLOW_NATIVE_UPDATE=1
      shift
      ;;
    --cache)
      export TENTAFLOW_NATIVE_CACHE="${2:?Brak wartości dla --cache}"
      NATIVE_CACHE="$TENTAFLOW_NATIVE_CACHE"
      shift 2
      ;;
    *)
      echo "Nieznany argument: $1" >&2
      exit 1
      ;;
  esac
done

HOST_PLATFORM="$(detect_platform)"

# Lista platform do zbudowania. Jawny --platform wygrywa i ogranicza do jednej.
# Na macOS bez --platform budujemy host (macos-arm64) ORAZ iOS device, bo to jest
# maszyna deweloperska iOS — bez native-libs/ios-arm64/ build wersji iOS pada na
# llama-cpp-sys-2, sherpa-rs-sys i tentaflow-zvec-sys.
PLATFORMS=()
if [ -n "$EXPLICIT_PLATFORM" ]; then
  PLATFORMS=("$EXPLICIT_PLATFORM")
elif [ "$HOST_PLATFORM" = "macos-arm64" ]; then
  PLATFORMS=("macos-arm64" "ios-arm64")
  [ "$BUILD_IOS_SIM" = "1" ] && PLATFORMS+=("ios-sim-arm64")
else
  PLATFORMS=("$HOST_PLATFORM")
fi

run_step() {
  local name="$1"
  shift
  if [ -n "$ONLY" ] && [ "$ONLY" != "$name" ]; then
    return 0
  fi
  "$@"
}

# Kroki per platforma. iOS NIE linkuje whisper.cpp i NIE buduje onnxruntime ze
# źródeł — build-sherpa-onnx.sh sam pobiera prebuilt ONNX Runtime xcframework.
build_platform() {
  local platform="$1"
  prepare_layout "$platform"
  write_manifest_header "$platform"
  echo "=========================================="
  echo "Platforma: $platform"
  echo "Cache:     $NATIVE_CACHE"
  echo "Output:    $NATIVE_ROOT/$platform"
  echo "=========================================="

  run_step zvec "$SCRIPT_DIR/build-zvec.sh" "$platform"
  run_step llama-cpp "$SCRIPT_DIR/build-llama-cpp.sh" "$platform"
  # pdfium (rasteryzacja PDF w RAG) — prebuilt dla każdej platformy (linux/macos/
  # android/ios). PDF musi działać na każdym urządzeniu, więc krok bezwarunkowy.
  run_step pdfium "$SCRIPT_DIR/build-pdfium.sh" "$platform"
  case "$platform" in
    ios-*)
      run_step sherpa-onnx "$SCRIPT_DIR/build-sherpa-onnx.sh" "$platform"
      ;;
    *)
      run_step whisper-cpp "$SCRIPT_DIR/build-whisper-cpp.sh" "$platform"
      run_step sherpa-onnx "$SCRIPT_DIR/build-sherpa-onnx.sh" "$platform"
      run_step onnxruntime "$SCRIPT_DIR/build-onnxruntime.sh" "$platform"
      ;;
  esac

  echo "Gotowe: $NATIVE_ROOT/$platform"
}

for platform in "${PLATFORMS[@]}"; do
  build_platform "$platform"
done
