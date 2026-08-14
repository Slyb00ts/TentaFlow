#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/native-libs/build-llama-cpp.sh
# Opis: Buduje llama.cpp jako wariant multi-backend albo wymuszone warianty backendów.
# =============================================================================

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

PLATFORM="${1:-$(detect_platform)}"
# Pinowany commit llama.cpp — wszyscy budują dokładnie tę samą wersję jako prebuilt
# biblioteki w native-libs. Świeży master da się wymusić przez LLAMA_CPP_REF=origin/master.
LLAMA_CPP_REF="${LLAMA_CPP_REF:-689e227db485c6b33d061555e74034c93a867649}"
BACKENDS="${LLAMA_CPP_BACKENDS:-auto}"
prepare_layout "$PLATFORM"
require_cmd git cmake

if [ "$LLAMA_CPP_REF" = "vendored" ]; then
  SRC="$ROOT/vendor/crates/llama-cpp-sys-2/llama.cpp"
  [ -d "$SRC" ] || { echo "Brak vendored llama.cpp: $SRC" >&2; exit 1; }
else
  SRC="$(repo_checkout llama.cpp https://github.com/ggml-org/llama.cpp.git "$LLAMA_CPP_REF")"
fi

apply_llama_patches() {
  local patch
  for patch in "$SCRIPT_DIR"/patches/llama-cpp/*.patch; do
    [ -e "$patch" ] || continue
    git -C "$SRC" apply --check "$patch"
    git -C "$SRC" apply "$patch"
  done
}

apply_llama_patches

detect_backends() {
  case "$PLATFORM" in
    linux-*|windows-*)
      local detected=()
      command -v nvcc >/dev/null 2>&1 && detected+=("cuda")
      if command -v hipcc >/dev/null 2>&1 || command -v amdclang++ >/dev/null 2>&1; then
        detected+=("rocm")
      fi
      if command -v glslc >/dev/null 2>&1 || command -v vulkaninfo >/dev/null 2>&1 || [ -n "${VULKAN_SDK:-}" ]; then
        detected+=("vulkan")
      fi
      printf '%s\n' "${detected[@]}"
      ;;
    macos-*|ios-*)
      printf '%s\n' "metal"
      ;;
    android-*)
      true
      ;;
    *)
      true
      ;;
  esac
}

android_abi_for_platform() {
  case "$1" in
    android-arm64) printf '%s\n' "arm64-v8a" ;;
    android-armv7) printf '%s\n' "armeabi-v7a" ;;
    android-x86_64) printf '%s\n' "x86_64" ;;
    *) return 1 ;;
  esac
}

if [ "$BACKENDS" = "auto" ]; then
  BACKEND_LIST=("multi")
  MULTI_BACKENDS=()
  while IFS= read -r backend; do
    [ -n "$backend" ] && MULTI_BACKENDS+=("$backend")
  done < <(detect_backends)
else
  IFS=',' read -r -a BACKEND_LIST <<< "$BACKENDS"
  MULTI_BACKENDS=()
  for backend in "${BACKEND_LIST[@]}"; do
    case "$backend" in
      cuda|vulkan|rocm|metal) MULTI_BACKENDS+=("$backend") ;;
    esac
  done
fi

backend_enabled() {
  local needle="$1"
  shift
  local backend
  for backend in "$@"; do
    [ "$backend" = "$needle" ] && return 0
  done
  return 1
}

build_backend() {
  local backend="$1"
  local build="$NATIVE_CACHE/build/llama.cpp-$PLATFORM-$backend"
  local static_dir="$NATIVE_ROOT/$PLATFORM/lib-static/llama-cpp/$backend"
  local dynamic_dir="$NATIVE_ROOT/$PLATFORM/lib-dynamic/llama-cpp/$backend"
  local jobs
  local enabled_backends=()
  reset_dir "$build"
  reset_dir "$static_dir"
  reset_dir "$dynamic_dir"

  local cmake_args=(
    -S "$SRC"
    -B "$build"
    -DCMAKE_BUILD_TYPE=Release
    -DBUILD_SHARED_LIBS=OFF
    # PIC wymagany — statyczne obiekty linkowane do binarki PIE (tentaflow).
    # Bez tego rust-lld zglasza R_X86_64_32 against local symbol (jak sherpa).
    -DCMAKE_POSITION_INDEPENDENT_CODE=ON
    -DGGML_CCACHE=OFF
    -DLLAMA_BUILD_TESTS=OFF
    -DLLAMA_BUILD_EXAMPLES=OFF
    -DLLAMA_BUILD_TOOLS=OFF
    -DLLAMA_BUILD_SERVER=OFF
    -DCMAKE_C_COMPILER_LAUNCHER=
    -DCMAKE_CXX_COMPILER_LAUNCHER=
    -DCMAKE_CUDA_COMPILER_LAUNCHER=
    -DCMAKE_HIP_COMPILER_LAUNCHER=
  )

  case "$PLATFORM" in
    ios-arm64|ios-sim-arm64)
      # Cross-compile na iOS: metallib wbudowujemy w bibliotekę (EMBED), żeby
      # prebuilt .a był samowystarczalny i nie wymagał osobnego pliku .metallib
      # obok aplikacji. Generator domyślny (Makefiles) + CMAKE_SYSTEM_NAME=iOS.
      [ "$(uname -s)" = "Darwin" ] || { echo "Build iOS llama wymaga macOS + Xcode." >&2; exit 1; }
      local ios_sdk
      if [ "$PLATFORM" = "ios-arm64" ]; then ios_sdk="iphoneos"; else ios_sdk="iphonesimulator"; fi
      cmake_args+=(
        -DCMAKE_SYSTEM_NAME=iOS
        -DCMAKE_OSX_ARCHITECTURES=arm64
        -DCMAKE_OSX_SYSROOT="$(xcrun --sdk "$ios_sdk" --show-sdk-path)"
        -DCMAKE_OSX_DEPLOYMENT_TARGET=13.0
        -DGGML_METAL_EMBED_LIBRARY=ON
      )
      ;;
    android-*)
      local ndk_root="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-/opt/android-ndk}}"
      local android_abi
      android_abi="$(android_abi_for_platform "$PLATFORM")"
      [ -f "$ndk_root/build/cmake/android.toolchain.cmake" ] || {
        echo "Build Android llama wymaga Android NDK (ustaw ANDROID_NDK_HOME)." >&2
        exit 1
      }
      cmake_args+=(
        -DCMAKE_TOOLCHAIN_FILE="$ndk_root/build/cmake/android.toolchain.cmake"
        -DANDROID_ABI="$android_abi"
        -DANDROID_NATIVE_API_LEVEL="${ANDROID_API_LEVEL:-26}"
        -DANDROID_STL=c++_shared
        -DGGML_OPENMP=OFF
      )
      ;;
  esac

  if [ "$backend" = "multi" ]; then
    enabled_backends=("${MULTI_BACKENDS[@]}")
  else
    enabled_backends=("$backend")
  fi

  case "$backend" in
    multi|cuda|metal|vulkan|rocm|cpu) ;;
    *) echo "Nieobsługiwany backend llama.cpp: $backend" >&2; exit 1 ;;
  esac

  if backend_enabled cuda "${enabled_backends[@]}"; then
    cmake_args+=(-DGGML_CUDA=ON)
    # Architektura CUDA. llama.cpp domyslnie uzywa "native" (nvcc -arch=native),
    # ktory na nowych GPU (np. Blackwell GB10 / DGX Spark = sm_121) czesto nie
    # jest wykrywany przez nvcc -> "Cannot find valid GPU for '-arch=native'" i
    # build leci na ZLEJ domyslnej architekturze (lib nie ruszy na tym GPU).
    # Ustawiamy jawnie: z env CMAKE_CUDA_ARCHITECTURES, albo z realnego GPU przez
    # nvidia-smi compute_cap (np. "12.1" -> "121").
    if [ -n "${CMAKE_CUDA_ARCHITECTURES:-}" ]; then
      cmake_args+=(-DCMAKE_CUDA_ARCHITECTURES="$CMAKE_CUDA_ARCHITECTURES")
    elif command -v nvidia-smi >/dev/null 2>&1; then
      cuda_cc="$(nvidia-smi --query-gpu=compute_cap --format=csv,noheader 2>/dev/null | head -1 | tr -d '. ')"
      if [ -n "$cuda_cc" ]; then
        cmake_args+=(-DCMAKE_CUDA_ARCHITECTURES="$cuda_cc")
        echo "[llama.cpp] CMAKE_CUDA_ARCHITECTURES=$cuda_cc (z nvidia-smi compute_cap)"
      fi
    fi
  fi
  backend_enabled metal "${enabled_backends[@]}" && cmake_args+=(-DGGML_METAL=ON)
  backend_enabled vulkan "${enabled_backends[@]}" && cmake_args+=(-DGGML_VULKAN=ON)
  if backend_enabled rocm "${enabled_backends[@]}"; then
    cmake_args+=(-DGGML_HIP=ON)
    [ -n "${CMAKE_HIP_ARCHITECTURES:-}" ] && cmake_args+=(-DCMAKE_HIP_ARCHITECTURES="$CMAKE_HIP_ARCHITECTURES")
  fi

  if backend_enabled cuda "${enabled_backends[@]}"; then
    jobs="${LLAMA_CPP_CUDA_JOBS:-4}"
  else
    jobs="$(platform_cpu_count)"
  fi

  local targets=(llama ggml ggml-base ggml-cpu)
  backend_enabled cuda "${enabled_backends[@]}" && targets+=(ggml-cuda)
  backend_enabled metal "${enabled_backends[@]}" && targets+=(ggml-metal)
  backend_enabled vulkan "${enabled_backends[@]}" && targets+=(ggml-vulkan)
  backend_enabled rocm "${enabled_backends[@]}" && targets+=(ggml-hip)

  echo "[llama.cpp] build backend: $backend (${enabled_backends[*]:-cpu})"
  env \
    -u CMAKE_C_COMPILER_LAUNCHER \
    -u CMAKE_CXX_COMPILER_LAUNCHER \
    -u CMAKE_CUDA_COMPILER_LAUNCHER \
    -u CMAKE_HIP_COMPILER_LAUNCHER \
    cmake "${cmake_args[@]}"
  # Output łapiemy do zmiennej i grepujemy z here-stringa, a nie przez potok:
  # pod `set -o pipefail` `grep -q` zamyka potok po dopasowaniu, cmake dostaje
  # SIGPIPE (exit 141) i pipefail zgłasza błąd całego potoku mimo trafienia.
  local target_help
  target_help="$(cmake --build "$build" --target help 2>/dev/null)"
  if grep -q '^... llama-common$' <<<"$target_help"; then
    targets+=(llama-common)
  else
    targets+=(common)
  fi
  cmake --build "$build" --target "${targets[@]}" -j"$jobs"

  copy_matching "$build" "$static_dir" -name '*.a' -o -name '*.lib'
  copy_matching "$build" "$dynamic_dir" -name '*.so*' -o -name '*.dylib' -o -name '*.dll'

  # llama-common dostarcza speculative decoding (MTP/ngram/Eagle3). Bez tej
  # biblioteki C-ABI z wrapper_speculative.* nie zlinkuje się, więc wymuszamy
  # jej obecność w eksporcie native-libs.
  if [ ! -f "$static_dir/libllama-common.a" ] && [ ! -f "$static_dir/llama-common.lib" ]; then
    echo "[llama.cpp] brak llama-common w $static_dir (target llama-common nie zbudowany?)" >&2
    exit 1
  fi

  append_manifest_library "$PLATFORM" "llama-cpp-$backend" "static-preferred" "$LLAMA_CPP_REF" "Backend: ${enabled_backends[*]:-cpu}. CUDA/ROCm/Vulkan mogą nadal wymagać dynamicznych bibliotek sterownika/runtime."
}

for backend in "${BACKEND_LIST[@]}"; do
  build_backend "$backend"
done

mkdir -p "$NATIVE_ROOT/$PLATFORM/include/llama"
find "$SRC/include" "$SRC/ggml/include" -type f -name '*.h' -exec cp -f {} "$NATIVE_ROOT/$PLATFORM/include/llama/" \;
mkdir -p "$NATIVE_ROOT/$PLATFORM/include/llama/common" "$NATIVE_ROOT/$PLATFORM/include/llama/nlohmann"
while IFS= read -r header; do
  target="$NATIVE_ROOT/$PLATFORM/include/llama/common/${header#"$SRC/common/"}"
  mkdir -p "$(dirname "$target")"
  cp -f "$header" "$target"
done < <(find "$SRC/common" -type f \( -name '*.h' -o -name '*.hpp' \))
cp -f "$SRC/vendor/nlohmann/json.hpp" "$NATIVE_ROOT/$PLATFORM/include/llama/nlohmann/json.hpp"
cp -f "$SRC/vendor/nlohmann/json_fwd.hpp" "$NATIVE_ROOT/$PLATFORM/include/llama/nlohmann/json_fwd.hpp"
