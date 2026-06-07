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
LLAMA_CPP_REF="${LLAMA_CPP_REF:-6b80c74f285390368b3c99c5e750f19e9b096e98}"
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

  if [ "$backend" = "multi" ]; then
    enabled_backends=("${MULTI_BACKENDS[@]}")
  else
    enabled_backends=("$backend")
  fi

  case "$backend" in
    multi|cuda|metal|vulkan|rocm|cpu) ;;
    *) echo "Nieobsługiwany backend llama.cpp: $backend" >&2; exit 1 ;;
  esac

  backend_enabled cuda "${enabled_backends[@]}" && cmake_args+=(-DGGML_CUDA=ON)
  backend_enabled metal "${enabled_backends[@]}" && cmake_args+=(-DGGML_METAL=ON)
  backend_enabled vulkan "${enabled_backends[@]}" && cmake_args+=(-DGGML_VULKAN=ON)
  backend_enabled rocm "${enabled_backends[@]}" && cmake_args+=(-DGGML_HIP=ON)

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
  if cmake --build "$build" --target help | grep -q '^... llama-common$'; then
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
find "$SRC/include" "$SRC/ggml/include" -type f -name '*.h' -exec cp {} "$NATIVE_ROOT/$PLATFORM/include/llama/" \;
mkdir -p "$NATIVE_ROOT/$PLATFORM/include/llama/common" "$NATIVE_ROOT/$PLATFORM/include/llama/nlohmann"
while IFS= read -r header; do
  target="$NATIVE_ROOT/$PLATFORM/include/llama/common/${header#"$SRC/common/"}"
  mkdir -p "$(dirname "$target")"
  cp "$header" "$target"
done < <(find "$SRC/common" -type f \( -name '*.h' -o -name '*.hpp' \))
cp "$SRC/vendor/nlohmann/json.hpp" "$NATIVE_ROOT/$PLATFORM/include/llama/nlohmann/json.hpp"
cp "$SRC/vendor/nlohmann/json_fwd.hpp" "$NATIVE_ROOT/$PLATFORM/include/llama/nlohmann/json_fwd.hpp"
