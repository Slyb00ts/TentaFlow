#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/native-libs/build-whisper-cpp.sh
# Opis: Buduje whisper.cpp jako wariant multi-backend albo wymuszone warianty backendów.
# =============================================================================

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

PLATFORM="${1:-$(detect_platform)}"
WHISPER_CPP_REF="${WHISPER_CPP_REF:-v1.8.3}"
BACKENDS="${WHISPER_CPP_BACKENDS:-auto}"
prepare_layout "$PLATFORM"
require_cmd git cmake

SRC="$(repo_checkout whisper.cpp https://github.com/ggml-org/whisper.cpp.git "$WHISPER_CPP_REF")"

detect_backends() {
  case "$PLATFORM" in
    linux-*|windows-*)
      local detected=()
      command -v nvcc >/dev/null 2>&1 && detected+=("cuda")
      if command -v glslc >/dev/null 2>&1 || command -v vulkaninfo >/dev/null 2>&1 || [ -n "${VULKAN_SDK:-}" ]; then
        detected+=("vulkan")
      fi
      printf '%s\n' "${detected[@]}"
      ;;
    macos-*)
      printf '%s\n' "metal"
      ;;
    *)
      true
      ;;
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
  local build="$NATIVE_CACHE/build/whisper.cpp-$PLATFORM-$backend"
  local static_dir="$NATIVE_ROOT/$PLATFORM/lib-static/whisper-cpp/$backend"
  local dynamic_dir="$NATIVE_ROOT/$PLATFORM/lib-dynamic/whisper-cpp/$backend"
  local jobs
  local enabled_backends=()
  reset_dir "$build"
  mkdir -p "$static_dir" "$dynamic_dir"

  local cmake_args=(
    -S "$SRC"
    -B "$build"
    -DCMAKE_BUILD_TYPE=Release
    -DBUILD_SHARED_LIBS=OFF
    -DGGML_CCACHE=OFF
    -DWHISPER_BUILD_TESTS=OFF
    -DWHISPER_BUILD_EXAMPLES=OFF
    -DCMAKE_C_COMPILER_LAUNCHER=
    -DCMAKE_CXX_COMPILER_LAUNCHER=
    -DCMAKE_CUDA_COMPILER_LAUNCHER=
    -DCMAKE_HIP_COMPILER_LAUNCHER=
  )

  if [ "$backend" = "multi" ]; then
    enabled_backends=("${MULTI_BACKENDS[@]}")
  else
    enabled_backends=("$backend")
  fi

  case "$backend" in
    multi|cuda|metal|vulkan|rocm|cpu) ;;
    *) echo "Nieobsługiwany backend whisper.cpp: $backend" >&2; exit 1 ;;
  esac

  backend_enabled cuda "${enabled_backends[@]}" && cmake_args+=(-DGGML_CUDA=ON)
  backend_enabled metal "${enabled_backends[@]}" && cmake_args+=(-DGGML_METAL=ON)
  backend_enabled vulkan "${enabled_backends[@]}" && cmake_args+=(-DGGML_VULKAN=ON)
  backend_enabled rocm "${enabled_backends[@]}" && cmake_args+=(-DGGML_HIP=ON)

  if backend_enabled cuda "${enabled_backends[@]}"; then
    jobs="${WHISPER_CPP_CUDA_JOBS:-4}"
  else
    jobs="$(platform_cpu_count)"
  fi

  local targets=(whisper ggml ggml-base ggml-cpu)
  backend_enabled cuda "${enabled_backends[@]}" && targets+=(ggml-cuda)
  backend_enabled metal "${enabled_backends[@]}" && targets+=(ggml-metal)
  backend_enabled vulkan "${enabled_backends[@]}" && targets+=(ggml-vulkan)
  backend_enabled rocm "${enabled_backends[@]}" && targets+=(ggml-hip)

  echo "[whisper.cpp] build backend: $backend (${enabled_backends[*]:-cpu})"
  env \
    -u CMAKE_C_COMPILER_LAUNCHER \
    -u CMAKE_CXX_COMPILER_LAUNCHER \
    -u CMAKE_CUDA_COMPILER_LAUNCHER \
    -u CMAKE_HIP_COMPILER_LAUNCHER \
    cmake "${cmake_args[@]}"
  cmake --build "$build" --target "${targets[@]}" -j"$jobs"

  copy_matching "$build" "$static_dir" -name '*.a' -o -name '*.lib'
  copy_matching "$build" "$dynamic_dir" -name '*.so*' -o -name '*.dylib' -o -name '*.dll'

  append_manifest_library "$PLATFORM" "whisper-cpp-$backend" "static-preferred" "$WHISPER_CPP_REF" "Backend: ${enabled_backends[*]:-cpu}."
}

for backend in "${BACKEND_LIST[@]}"; do
  build_backend "$backend"
done

mkdir -p "$NATIVE_ROOT/$PLATFORM/include/whisper"
find "$SRC/include" "$SRC/ggml/include" -type f -name '*.h' -exec cp -f {} "$NATIVE_ROOT/$PLATFORM/include/whisper/" \;
