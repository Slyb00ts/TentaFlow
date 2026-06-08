#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/native-libs/build-zvec.sh
# Opis: Buduje lub pobiera artefakt zvec i zapisuje go w native-libs.
# =============================================================================

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

PLATFORM="${1:-$(detect_platform)}"
prepare_layout "$PLATFORM"

"$ROOT/scripts/build-zvec.sh" "$PLATFORM"

SRC_DIR="$ROOT/tentaflow-zvec-sys/vendor/lib/$PLATFORM"
STATIC_DIR="$NATIVE_ROOT/$PLATFORM/lib-static"
DYNAMIC_DIR="$NATIVE_ROOT/$PLATFORM/lib-dynamic"

case "$PLATFORM" in
  linux-*|macos-*)
    copy_matching "$SRC_DIR" "$DYNAMIC_DIR" -name 'libzvec_c_api.so' -o -name 'libzvec_c_api.dylib'
    append_manifest_library "$PLATFORM" "zvec" "dynamic" "${ZVEC_REF:-f562bdd636d454f18128cb18b41578128d1415a4}" "Desktopowy build upstream produkuje samowystarczalną bibliotekę współdzieloną."
    ;;
  windows-*)
    copy_matching "$SRC_DIR" "$STATIC_DIR" -name 'zvec_c_api.lib'
    copy_matching "$SRC_DIR" "$DYNAMIC_DIR" -name 'zvec_c_api.dll'
    append_manifest_library "$PLATFORM" "zvec" "dynamic-import-lib" "${ZVEC_REF:-f562bdd636d454f18128cb18b41578128d1415a4}" "MSVC używa import library, a DLL musi być obok binarki."
    ;;
  ios-*|android-*)
    copy_matching "$SRC_DIR" "$STATIC_DIR" -name 'libzvec_c_api.a' -o -name 'libzvec_deps.a'
    append_manifest_library "$PLATFORM" "zvec" "static" "${ZVEC_REF:-f562bdd636d454f18128cb18b41578128d1415a4}" "Mobile używa archiwów statycznych."
    ;;
  *)
    echo "Nieobsługiwana platforma zvec: $PLATFORM" >&2
    exit 1
    ;;
esac

mkdir -p "$NATIVE_ROOT/$PLATFORM/include/zvec"
cp -f "$ROOT/tentaflow-zvec-sys/vendor/include/zvec/c_api.h" "$NATIVE_ROOT/$PLATFORM/include/zvec/c_api.h"
