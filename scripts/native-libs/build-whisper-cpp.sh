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
# Resolved here (parent shell) so the PATH/CUDACXX exports reach cmake.
resolve_gpu_toolchains

SRC="$(repo_checkout whisper.cpp https://github.com/ggml-org/whisper.cpp.git "$WHISPER_CPP_REF")"

android_abi_for_platform() {
  case "$1" in
    android-arm64) printf '%s\n' "arm64-v8a" ;;
    android-armv7) printf '%s\n' "armeabi-v7a" ;;
    android-x86_64) printf '%s\n' "x86_64" ;;
    *) return 1 ;;
  esac
}

android_triple_for_platform() {
  case "$1" in
    android-arm64) printf '%s\n' "aarch64-linux-android" ;;
    android-armv7) printf '%s\n' "armv7a-linux-androideabi" ;;
    android-x86_64) printf '%s\n' "x86_64-linux-android" ;;
    *) return 1 ;;
  esac
}

android_cxx_for_platform() {
  local ndk_root="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-/opt/android-ndk}}"
  local triple
  triple="$(android_triple_for_platform "$1")"
  printf '%s/bin/%s%s-clang++\n' \
    "$ndk_root/toolchains/llvm/prebuilt/linux-x86_64" \
    "$triple" \
    "${ANDROID_API_LEVEL:-26}"
}

if [ "$BACKENDS" = "auto" ]; then
  BACKEND_LIST=("multi")
  MULTI_BACKENDS=()
  # Command substitution (not process substitution) so a hard error inside
  # detect_backends aborts the script under set -e.
  detected_backends="$(detect_backends)"
  while IFS= read -r backend; do
    [ -n "$backend" ] && MULTI_BACKENDS+=("$backend")
  done <<< "$detected_backends"
else
  IFS=',' read -r -a BACKEND_LIST <<< "$BACKENDS"
  MULTI_BACKENDS=()
  for backend in "${BACKEND_LIST[@]}"; do
    case "$backend" in
      cuda|vulkan|metal) MULTI_BACKENDS+=("$backend") ;;
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

# Linkuje whisper + JEGO ggml w jeden wspoldzielony obiekt z UKRYTYMI symbolami
# ggml_* (eksportowane tylko whisper_*). To pozwala glownej binarce linkowac
# rownoczesnie STATYCZNY ggml z llama.cpp (inna wersja: master vs whisper v1.8.3)
# bez kolizji symboli — whisper uzywa swojego prywatnego ggml zamknietego w .so,
# zamiast dzielic jeden zestaw `ggml_*` z llama (co konczylo sie SIGABRT przy
# `--allow-multiple-definition`). Rust (whisper-rs-sys) woła wylacznie `whisper_*`.
build_isolated_dylib() {
  local backend="$1" static_dir="$2" out_dir="$3" build="$4"
  shift 4
  local cxx="${CXX:-c++}"

  # Wciagamy WSZYSTKIE zbudowane archiwa whisper.cpp (whisper + ggml + kazdy
  # backend: cpu/cuda/metal/blas/vulkan). Kazdy backend rejestruje sie w
  # ggml_backend_registry() przez symbol `*_reg`, wiec pominiecie ktoregos (np.
  # ggml-blas auto-wykrytego z Accelerate na macOS) = "Undefined symbols:
  # _ggml_backend_blas_reg". Glob jest odporny na to, ktore backendy cmake
  # faktycznie wlaczyl (czasem wiecej niz w MULTI_BACKENDS).
  local archives=() a
  for a in "$static_dir"/lib*.a; do
    [ -f "$a" ] && archives+=("$a")
  done
  if [ "${#archives[@]}" -eq 0 ]; then
    echo "[whisper.cpp] brak archiwow .a w $static_dir" >&2
    exit 1
  fi
  # Liby systemowe/frameworki dobieramy po OBECNOSCI archiwum backendu, nie po
  # liscie enabled — spojnie z globem powyzej.
  has_lib() { [ -f "$static_dir/lib$1.a" ]; }

  case "$PLATFORM" in
    macos-*)
      # macOS: two-level namespace sam blokuje interpozycje, ale i tak
      # eksportujemy tylko `_whisper_*`. `-all_load` wciaga calosc archiwow
      # (odpowiednik --whole-archive). Accelerate pokrywa backend BLAS.
      local list="$build/whisper_exports.list"
      printf '_whisper_*\n' > "$list"
      local frameworks=(-framework Accelerate -framework Foundation)
      has_lib ggml-metal && frameworks+=(-framework Metal -framework MetalKit)
      "$cxx" -dynamiclib -fPIC \
        -install_name "@rpath/libwhisper_tf.dylib" \
        -o "$out_dir/libwhisper_tf.dylib" \
        -Wl,-exported_symbols_list,"$list" \
        -Wl,-all_load "${archives[@]}" \
        "${frameworks[@]}"
      ;;
    windows-*)
      echo "[whisper.cpp] izolowany dylib na Windows: uzyj build-all.ps1 (.def/__declspec)" >&2
      return 0
      ;;
    android-*)
      local map="$build/whisper_exports.map"
      local android_cxx
      printf '{ global: whisper_*; local: *; };\n' > "$map"
      android_cxx="$(android_cxx_for_platform "$PLATFORM")"
      [ -x "$android_cxx" ] || {
        echo "[whisper.cpp] brak Android NDK clang++: $android_cxx" >&2
        exit 1
      }
      "$android_cxx" -shared -fPIC \
        -o "$out_dir/libwhisper_tf.so" \
        -Wl,--version-script="$map" \
        -Wl,--whole-archive "${archives[@]}" -Wl,--no-whole-archive \
        -Wl,--no-undefined \
        -lm -ldl -llog -landroid -lc++_shared
      ;;
    *)
      # Linux: version-script localizuje WSZYSTKO poza whisper_* — prywatny ggml
      # nie trafia do dynamicznej tablicy symboli, wiec nie koliduje z ggml llamy
      # w glownej binarce. --whole-archive wciaga wszystkie obiekty (rejestracje
      # backendow ggml przez static-init dzieja sie wewnatrz .so). --no-undefined
      # wymusza, by brakujacy backend-lib byl jasnym bledem linku, a nie cichym
      # crashem w runtime.
      local map="$build/whisper_exports.map"
      printf '{ global: whisper_*; local: *; };\n' > "$map"
      local syslibs=(-fopenmp -lm -ldl -lpthread -lrt)
      if has_lib ggml-cuda; then
        syslibs+=(-L/usr/local/cuda/lib64 -L/usr/local/cuda/lib64/stubs -L/opt/cuda/lib64 -L/opt/cuda/lib64/stubs)
        syslibs+=(-lcudart -lcublas -lcublasLt -lcuda -lculibos)
      fi
      has_lib ggml-vulkan && syslibs+=(-lvulkan)
      has_lib ggml-blas && syslibs+=(-lopenblas)
      "$cxx" -shared -fPIC \
        -o "$out_dir/libwhisper_tf.so" \
        -Wl,--version-script="$map" \
        -Wl,--whole-archive "${archives[@]}" -Wl,--no-whole-archive \
        -Wl,--no-undefined \
        "${syslibs[@]}"
      ;;
  esac

  append_manifest_library "$PLATFORM" "whisper-tf-isolated-$backend" "isolated-dylib" "$WHISPER_CPP_REF" "Whisper + prywatny ggml; eksport tylko whisper_*."
}

build_backend() {
  local backend="$1"
  local build="$NATIVE_CACHE/build/whisper.cpp-$PLATFORM-$backend"
  local static_dir="$NATIVE_ROOT/$PLATFORM/lib-static/whisper-cpp/$backend"
  local dynamic_dir="$NATIVE_ROOT/$PLATFORM/lib-dynamic/whisper-cpp/$backend"
  local jobs
  local enabled_backends=()
  reset_dir "$build"
  # Wipe the variant's outputs before copying: build_isolated_dylib globs
  # lib*.a, so a stale archive from a previous build (e.g. libggml-cuda.a next
  # to a fresh libggml-vulkan.a) would be linked in and clash on ggml symbols.
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
    -DWHISPER_BUILD_TESTS=OFF
    -DWHISPER_BUILD_EXAMPLES=OFF
    -DCMAKE_C_COMPILER_LAUNCHER=
    -DCMAKE_CXX_COMPILER_LAUNCHER=
    -DCMAKE_CUDA_COMPILER_LAUNCHER=
  )

  if [ "$backend" = "multi" ]; then
    enabled_backends=("${MULTI_BACKENDS[@]}")
  else
    enabled_backends=("$backend")
  fi

  case "$backend" in
    multi|cuda|metal|vulkan|cpu) ;;
    *) echo "Nieobsługiwany backend whisper.cpp: $backend" >&2; exit 1 ;;
  esac

  case "$PLATFORM" in
    android-*)
      local ndk_root="${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-/opt/android-ndk}}"
      local android_abi
      android_abi="$(android_abi_for_platform "$PLATFORM")"
      [ -f "$ndk_root/build/cmake/android.toolchain.cmake" ] || {
        echo "Build Android whisper wymaga Android NDK (ustaw ANDROID_NDK_HOME)." >&2
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

  if backend_enabled cuda "${enabled_backends[@]}"; then
    cmake_args+=(-DGGML_CUDA=ON)
    # Architektura CUDA jawnie (jak w build-llama-cpp.sh) — "native" nie wykrywa
    # nowych GPU jak Blackwell GB10/sm_121 -> build na zlej domyslnej arch.
    if [ -n "${CMAKE_CUDA_ARCHITECTURES:-}" ]; then
      cmake_args+=(-DCMAKE_CUDA_ARCHITECTURES="$CMAKE_CUDA_ARCHITECTURES")
    elif command -v nvidia-smi >/dev/null 2>&1; then
      cuda_cc="$(nvidia-smi --query-gpu=compute_cap --format=csv,noheader 2>/dev/null | head -1 | tr -d '. ' || true)"
      [[ "$cuda_cc" =~ ^[0-9]+$ ]] && cmake_args+=(-DCMAKE_CUDA_ARCHITECTURES="$cuda_cc")
    fi
  fi
  backend_enabled metal "${enabled_backends[@]}" && cmake_args+=(-DGGML_METAL=ON)
  backend_enabled vulkan "${enabled_backends[@]}" && cmake_args+=(-DGGML_VULKAN=ON)

  if backend_enabled cuda "${enabled_backends[@]}"; then
    jobs="${WHISPER_CPP_CUDA_JOBS:-4}"
  else
    jobs="$(platform_cpu_count)"
  fi

  local targets=(whisper ggml ggml-base ggml-cpu)
  backend_enabled cuda "${enabled_backends[@]}" && targets+=(ggml-cuda)
  backend_enabled metal "${enabled_backends[@]}" && targets+=(ggml-metal)
  backend_enabled vulkan "${enabled_backends[@]}" && targets+=(ggml-vulkan)

  echo "[whisper.cpp] build backend: $backend (${enabled_backends[*]:-cpu})"
  env \
    -u CMAKE_C_COMPILER_LAUNCHER \
    -u CMAKE_CXX_COMPILER_LAUNCHER \
    -u CMAKE_CUDA_COMPILER_LAUNCHER \
    cmake "${cmake_args[@]}"
  cmake --build "$build" --target "${targets[@]}" -j"$jobs"

  copy_matching "$build" "$static_dir" -name '*.a' -o -name '*.lib'

  # Z tych .a budujemy JEDEN izolowany dylib (prywatny ggml) — patrz
  # build_isolated_dylib. Glowna binarka linkuje go dynamicznie, a llama.cpp
  # zostaje statycznie ze swoim ggml. To eliminuje kolizje symboli ggml_*.
  append_manifest_library "$PLATFORM" "whisper-cpp-$backend" "static-input-for-dylib" "$WHISPER_CPP_REF" "Backend: ${enabled_backends[*]:-cpu}. Wejscie do izolowanego dylib."
  build_isolated_dylib "$backend" "$static_dir" "$dynamic_dir" "$build"
}

for backend in "${BACKEND_LIST[@]}"; do
  build_backend "$backend"
done

mkdir -p "$NATIVE_ROOT/$PLATFORM/include/whisper"
find "$SRC/include" "$SRC/ggml/include" -type f -name '*.h' -exec cp -f {} "$NATIVE_ROOT/$PLATFORM/include/whisper/" \;
