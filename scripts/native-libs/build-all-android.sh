#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/native-libs/build-all-android.sh
# Opis: Buduje natywne biblioteki wymagane przez aplikacje Android
#       (zvec, llama.cpp, whisper.cpp) dla wybranych ABI.
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
source "$SCRIPT_DIR/common.sh"

ANDROID_API_LEVEL="${ANDROID_API_LEVEL:-26}"
ONLY=""
PLATFORMS=("android-arm64" "android-armv7" "android-x86_64")

usage() {
  cat <<'EOF'
Uzycie:
  scripts/native-libs/build-all-android.sh [opcje]

Opcje:
  --platform <id>    Buduj tylko jeden target: android-arm64 | android-armv7 | android-x86_64
  --only <name>      Buduj tylko: zvec | llama-cpp | whisper-cpp | pdfium
  --api <level>      Android API level dla NDK/CMake (domyslnie 26)
  --cache <dir>      Katalog cache dla zrodel i buildow

Wymagane:
  Android NDK w standardowej lokalizacji Android SDK albo /opt/android-ndk
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --platform)
      PLATFORMS=("${2:?Brak wartosci dla --platform}")
      shift 2
      ;;
    --only)
      ONLY="${2:?Brak wartosci dla --only}"
      shift 2
      ;;
    --api)
      ANDROID_API_LEVEL="${2:?Brak wartosci dla --api}"
      shift 2
      ;;
    --cache)
      export TENTAFLOW_NATIVE_CACHE="${2:?Brak wartosci dla --cache}"
      NATIVE_CACHE="$TENTAFLOW_NATIVE_CACHE"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Nieznany argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

ANDROID_NDK_HOME="$(require_android_ndk)"
ANDROID_HOST_TAG="$(android_host_tag)"
ANDROID_TOOLCHAIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$ANDROID_HOST_TAG"
if [ ! -f "$ANDROID_NDK_HOME/build/cmake/android.toolchain.cmake" ]; then
  echo "ERROR: Nie znaleziono Android NDK: $ANDROID_NDK_HOME" >&2
  exit 1
fi
if [ ! -x "$ANDROID_TOOLCHAIN/bin/llvm-ar" ]; then
  echo "ERROR: Nie znaleziono llvm-ar w $ANDROID_TOOLCHAIN/bin" >&2
  exit 1
fi

export ANDROID_NDK_HOME
export ANDROID_API_LEVEL

# Android C/C++ cross-buildy musza isc bez sccache/ccache. Zvec i jego
# thirdparty projekty cache'uja compiler launcher w CMakeCache, a sccache czesto
# nie moze uruchomic NDK clang w sandboxie albo z absolutnej sciezki.
unset RUSTC_WRAPPER
unset CMAKE_C_COMPILER_LAUNCHER
unset CMAKE_CXX_COMPILER_LAUNCHER
unset CMAKE_CUDA_COMPILER_LAUNCHER
export CCACHE_DISABLE=1
export SCCACHE_DISABLE=1

android_triple_for_platform() {
  case "$1" in
    android-arm64) printf '%s\n' "aarch64-linux-android" ;;
    android-armv7) printf '%s\n' "arm-linux-androideabi" ;;
    android-x86_64) printf '%s\n' "x86_64-linux-android" ;;
    *) return 1 ;;
  esac
}

copy_android_runtime() {
  local platform="$1"
  local triple
  triple="$(android_triple_for_platform "$platform")"
  local src="$ANDROID_TOOLCHAIN/sysroot/usr/lib/$triple/libc++_shared.so"
  local dst="$NATIVE_ROOT/$platform/lib-dynamic"
  if [ -f "$src" ]; then
    mkdir -p "$dst"
    cp -f "$src" "$dst/libc++_shared.so"
  else
    echo "WARN: Brak libc++_shared.so dla $platform ($src)" >&2
  fi
}

run_step() {
  local name="$1"
  shift
  if [ -n "$ONLY" ] && [ "$ONLY" != "$name" ]; then
    return
  fi
  "$@"
}

for platform in "${PLATFORMS[@]}"; do
  case "$platform" in
    android-arm64|android-armv7|android-x86_64) ;;
    *)
      echo "Nieobslugiwany Android platform id: $platform" >&2
      exit 1
      ;;
  esac

  echo "============================================================"
  echo " Android native-libs: $platform"
  echo " API:   $ANDROID_API_LEVEL"
  echo " NDK:   $ANDROID_NDK_HOME"
  echo " Cache: $NATIVE_CACHE"
  echo " Out:   $NATIVE_ROOT/$platform"
  echo "============================================================"

  prepare_layout "$platform"
  write_manifest_header "$platform"

  run_step zvec "$SCRIPT_DIR/build-zvec.sh" "$platform"
  run_step llama-cpp "$SCRIPT_DIR/build-llama-cpp.sh" "$platform"
  run_step whisper-cpp "$SCRIPT_DIR/build-whisper-cpp.sh" "$platform"
  # pdfium (rasteryzacja PDF w RAG) — prebuilt android-arm64/armv7/x86_64.
  # PDF musi działać też na telefonie, więc krok bezwarunkowy.
  run_step pdfium "$SCRIPT_DIR/build-pdfium.sh" "$platform"
  copy_android_runtime "$platform"

  echo "Gotowe: $NATIVE_ROOT/$platform"
done
