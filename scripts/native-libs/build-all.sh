#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/native-libs/build-all.sh
# Opis: Buduje natywne biblioteki dla automatycznie wykrytej platformy.
# =============================================================================

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

ONLY=""
PLATFORM="$(detect_platform)"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --platform)
      PLATFORM="${2:?Brak wartości dla --platform}"
      shift 2
      ;;
    --only)
      ONLY="${2:?Brak wartości dla --only}"
      shift 2
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

prepare_layout "$PLATFORM"
write_manifest_header "$PLATFORM"

run_step() {
  local name="$1"
  shift
  if [ -n "$ONLY" ] && [ "$ONLY" != "$name" ]; then
    return
  fi
  "$@"
}

echo "Platforma: $PLATFORM"
echo "Cache:     $NATIVE_CACHE"
echo "Output:    $NATIVE_ROOT/$PLATFORM"

run_step zvec "$SCRIPT_DIR/build-zvec.sh" "$PLATFORM"
run_step llama-cpp "$SCRIPT_DIR/build-llama-cpp.sh" "$PLATFORM"
run_step whisper-cpp "$SCRIPT_DIR/build-whisper-cpp.sh" "$PLATFORM"
run_step sherpa-onnx "$SCRIPT_DIR/build-sherpa-onnx.sh" "$PLATFORM"
run_step onnxruntime "$SCRIPT_DIR/build-onnxruntime.sh" "$PLATFORM"

echo "Gotowe: $NATIVE_ROOT/$PLATFORM"
