#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/native-libs/common.sh
# Opis: Wspólne funkcje dla skryptów budujących natywne biblioteki TentaFlow.
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
NATIVE_ROOT="$ROOT/native-libs"
# Scratch (klony git + rozpakowane FetchContent _deps + obiekty buildu) potrafi
# urosnac do wielu GB. NIE trzymamy go w /tmp: na Linuksie to czesto tmpfs w RAM,
# wiec na maszynie z malym RAM-em rozpakowanie urywa pliki do zer i CMake pada na
# "Parse error ... bad character". Domyslnie celujemy w trwaly cache na dysku.
NATIVE_CACHE="${TENTAFLOW_NATIVE_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/tentaflow-native-libs}"

detect_platform() {
  local os arch
  case "$(uname -s)" in
    Linux*) os="linux" ;;
    Darwin*) os="macos" ;;
    MINGW*|MSYS*|CYGWIN*) os="windows" ;;
    *) echo "Nieobsługiwany system: $(uname -s)" >&2; return 1 ;;
  esac

  case "$(uname -m | tr '[:upper:]' '[:lower:]')" in
    x86_64|amd64) arch="x86_64" ;;
    aarch64|arm64)
      # Nazwa arch zalezy od OS — konsumenci (build.rs zvec/llama, setup.sh,
      # build-zvec.sh) oczekuja "linux-aarch64" ale "macos-arm64". Bez tego
      # rozroznienia producent pisal do native-libs/linux-arm64/, a build.rs
      # szukal w linux-aarch64/ → "missing libzvec_c_api.so" na ARM Linux.
      if [ "$os" = "linux" ]; then arch="aarch64"; else arch="arm64"; fi
      ;;
    *) echo "Nieobsługiwana architektura: $(uname -m)" >&2; return 1 ;;
  esac

  printf '%s-%s\n' "$os" "$arch"
}

platform_cpu_count() {
  nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4
}

# Resolves a compiler binary: PATH first, then the given fallback locations.
# Prints the absolute path on success.
find_toolchain_compiler() {
  local name="$1"
  shift
  local found candidate
  if found="$(command -v "$name" 2>/dev/null)"; then
    printf '%s\n' "$found"
    return 0
  fi
  for candidate in "$@"; do
    [ -n "$candidate" ] && [ -x "$candidate" ] || continue
    printf '%s\n' "$candidate"
    return 0
  done
  return 1
}

# Prepends the compiler's directory to PATH when it was found outside PATH, so
# cmake's CUDA language detection sees the same compiler.
expose_compiler_on_path() {
  local compiler="$1"
  command -v "$(basename "$compiler")" >/dev/null 2>&1 && return 0
  export PATH="$(dirname "$compiler"):$PATH"
}

# Locates nvcc (PATH or the usual toolkit prefixes) and exports what cmake
# needs to pick it up (PATH, CUDACXX). Idempotent: the result is cached in the
# exported NATIVE_NVCC, so it can be called from the parent shell (where the
# exports must land) and again from subshells.
resolve_gpu_toolchains() {
  [ "${NATIVE_TOOLCHAINS_RESOLVED:-0}" = "1" ] && return 0
  export NATIVE_TOOLCHAINS_RESOLVED=1
  export NATIVE_NVCC=""

  local nvcc
  if nvcc="$(find_toolchain_compiler nvcc \
      "${CUDA_HOME:+$CUDA_HOME/bin/nvcc}" \
      "${CUDA_PATH:+$CUDA_PATH/bin/nvcc}" \
      /usr/local/cuda/bin/nvcc \
      /opt/cuda/bin/nvcc)"; then
    expose_compiler_on_path "$nvcc"
    export NATIVE_NVCC="$nvcc"
    export CUDACXX="${CUDACXX:-$nvcc}"
  fi
}

nvidia_gpu_visible() {
  [ "$(nvidia-smi -L 2>/dev/null | grep -c '^GPU ')" -ge 1 ] 2>/dev/null
}


# Prints the auto-detected GPU backends for $PLATFORM, one per line, and a
# one-line summary (with compiler paths) on stderr so a silent miss is visible.
# On Linux a visible NVIDIA GPU without a detected CUDA toolkit is a hard error:
# the toolkit is almost certainly installed but not found.
detect_backends() {
  case "$PLATFORM" in
    linux-*|windows-*)
      resolve_gpu_toolchains
      local detected=() summary=()
      # Shipping policy: NVIDIA runs on CUDA, everything else (AMD, Intel) runs
      # on the portable Vulkan backend. There is no HIP/ROCm build target.
      # CUDA is built for an NVIDIA GPU that is actually here. A toolkit alone is
      # not the signal: this repo is built on machines that keep /opt/cuda around
      # with no NVIDIA card in the slot.
      if [ -n "$NATIVE_NVCC" ] && ! nvidia_gpu_visible; then
        echo "[native-libs] cuda toolkit found but no NVIDIA GPU is visible — building vulkan instead. Force it with LLAMA_CPP_BACKENDS=cuda." >&2
        NATIVE_NVCC=""
      fi
      if [ -n "$NATIVE_NVCC" ]; then
        detected+=("cuda")
        summary+=("cuda($NATIVE_NVCC)")
      fi
      if command -v glslc >/dev/null 2>&1 || command -v vulkaninfo >/dev/null 2>&1 || [ -n "${VULKAN_SDK:-}" ]; then
        detected+=("vulkan")
        summary+=("vulkan")
      fi
      echo "[native-libs] backends: ${summary[*]:-none}" >&2
      case "$PLATFORM" in
        linux-*)
          if [ -z "$NATIVE_NVCC" ] && nvidia_gpu_visible; then
            echo "[native-libs] ERROR: an NVIDIA GPU is visible (nvidia-smi -L) but no CUDA toolkit was found" >&2
            echo "  (looked for nvcc on PATH, \$CUDA_HOME/bin, \$CUDA_PATH/bin, /usr/local/cuda/bin, /opt/cuda/bin)." >&2
            echo "  Install the toolkit or export CUDA_HOME; to build without CUDA on purpose set" >&2
            echo "  LLAMA_CPP_BACKENDS / WHISPER_CPP_BACKENDS explicitly instead of 'auto'." >&2
            exit 1
          fi
          ;;
      esac
      printf '%s\n' "${detected[@]}"
      ;;
    macos-*|ios-*)
      printf '%s\n' "metal"
      ;;
    *)
      true
      ;;
  esac
}

# SHA256 pliku. macOS nie ma `sha256sum` (tam jest `shasum -a 256`), a suma
# jest weryfikowana przy KAZDYM pobieranym prebuilcie — bez tego build na Apple
# Silicon wywala sie na pdfium zaraz po llama.cpp.
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    echo "Brak sha256sum i shasum — nie moge zweryfikowac $1" >&2
    return 1
  fi
}

require_cmd() {
  local missing=0
  for cmd in "$@"; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
      echo "Brak wymaganego polecenia: $cmd" >&2
      missing=1
    fi
  done
  [ "$missing" -eq 0 ]
}

# Zapewnia, ze katalog istnieje i jest WLASNOSCIA biezacego usera. Jesli istnieje,
# ale jest root-owned / niezapisywalny (np. po wczesniejszym sudo-buildzie albo
# klonie repo jako root w /opt), odzyskuje wlasnosc przez `sudo chown`. Dzieki
# temu kolejne user-owe mkdir/cp NIE padaja na "Permission denied" — niezaleznie
# od tego jak repo zostalo sklonowane (uniwersalnie). Bez tego buildy zostawialy
# root-owned artefakty i nastepne uruchomienia jako user sie wywalaly.
ensure_owned_dir() {
  local dir="$1"
  if mkdir -p "$dir" 2>/dev/null && [ -w "$dir" ]; then
    return 0
  fi
  # Katalog jest root-owned / niezapisywalny. Odzyskujemy wlasnosc przez sudo —
  # WIDOCZNIE (bez 2>/dev/null), zeby ewentualny prompt o haslo sie pokazal i
  # zeby blad sudo nie zostal po cichu polkniety (wczesniej dlatego "self-heal"
  # nie dzialal). Po probie weryfikujemy i failujemy GLOSNO z gotowa komenda.
  local owner; owner="$(id -un):$(id -gn)"
  echo ">>> $dir nie jest zapisywalny (root-owned?) — odzyskuje wlasnosc dla $owner (sudo moze poprosic o haslo)..." >&2
  if command -v sudo >/dev/null 2>&1; then
    sudo mkdir -p "$dir" || true
    sudo chown -R "$owner" "$dir" || true
  fi
  if [ ! -w "$dir" ]; then
    echo "BLAD: nadal brak zapisu w $dir." >&2
    echo "      Uruchom recznie:  sudo chown -R $owner \"$dir\"" >&2
    return 1
  fi
}

# Cache buildu rozpakowuje wiele GB zrodel. Jesli laduje na tmpfs (np. /tmp w RAM)
# albo zostalo <8 GB wolnego, ostrzegamy GLOSNO — bo brak miejsca objawia sie nie
# jako "No space left", lecz jako urwane/wyzerowane pliki i pozniejszy "Parse error
# ... bad character" w CMake, co jest mylace i trudne do zdiagnozowania.
check_cache_space() {
  mkdir -p "$NATIVE_CACHE"
  local fstype avail_kb
  fstype="$(df -PT "$NATIVE_CACHE" 2>/dev/null | awk 'NR==2 {print $2}')"
  avail_kb="$(df -Pk "$NATIVE_CACHE" 2>/dev/null | awk 'NR==2 {print $4}')"
  if [ "$fstype" = "tmpfs" ] || [ "$fstype" = "ramfs" ]; then
    echo ">>> UWAGA: cache buildu ($NATIVE_CACHE) jest na $fstype (RAM)." >&2
    echo "    Przy malym RAM-ie rozpakowanie zrodel sie urwie i CMake padnie na" >&2
    echo "    'Parse error ... bad character'. Ustaw cache na dysk:" >&2
    echo "      export TENTAFLOW_NATIVE_CACHE=\"\$HOME/.cache/tentaflow-native-libs\"" >&2
  fi
  if [ -n "$avail_kb" ] && [ "$avail_kb" -lt 8388608 ]; then
    echo ">>> UWAGA: tylko $((avail_kb / 1024 / 1024)) GB wolnego w $NATIVE_CACHE (zalecane >=8 GB)." >&2
  fi
}

prepare_layout() {
  local platform="$1"
  check_cache_space
  ensure_owned_dir "$NATIVE_ROOT/$platform/include"
  ensure_owned_dir "$NATIVE_ROOT/$platform/lib-static"
  ensure_owned_dir "$NATIVE_ROOT/$platform/lib-dynamic"
  mkdir -p "$NATIVE_CACHE/src" "$NATIVE_CACHE/build"
}

repo_checkout() {
  local name="$1"
  local url="$2"
  local ref="$3"
  local dir="$NATIVE_CACHE/src/$name"

  if [ ! -d "$dir/.git" ]; then
    git clone "$url" "$dir"
  fi

  (
    cd "$dir"
    git fetch --tags origin -q
    if [ "${TENTAFLOW_NATIVE_UPDATE:-0}" = "1" ]; then
      git fetch origin -q
    fi
    git checkout -fq "$ref"
    git submodule update --init --recursive --depth 1 --force -q
  )

  printf '%s\n' "$dir"
}

reset_dir() {
  local dir="$1"
  rm -rf "$dir"
  mkdir -p "$dir"
}

copy_matching() {
  local src="$1"
  local dst="$2"
  shift 2
  mkdir -p "$dst"
  find "$src" -type f \( "$@" \) -exec cp -f {} "$dst/" \;
}

write_manifest_header() {
  local platform="$1"
  local manifest="$NATIVE_ROOT/$platform/manifest.toml"
  mkdir -p "$(dirname "$manifest")"
  {
    printf '# Wygenerowane przez scripts/native-libs/build-all.sh\n'
    printf 'platform = "%s"\n' "$platform"
    printf 'cache_dir = "%s"\n' "$NATIVE_CACHE"
    printf 'generated_at_unix = %s\n\n' "$(date +%s)"
  } > "$manifest"
}

append_manifest_library() {
  local platform="$1"
  local name="$2"
  local linkage="$3"
  local ref="$4"
  local note="$5"
  local manifest="$NATIVE_ROOT/$platform/manifest.toml"
  {
    printf '[[library]]\n'
    printf 'name = "%s"\n' "$name"
    printf 'linkage = "%s"\n' "$linkage"
    printf 'ref = "%s"\n' "$ref"
    printf 'note = "%s"\n\n' "$note"
  } >> "$manifest"
}

android_host_tag() {
  case "$(uname -s)" in
    Linux*) printf '%s\n' "linux-x86_64" ;;
    Darwin*) printf '%s\n' "darwin-x86_64" ;;
    *) echo "Nieobsługiwany host Android NDK: $(uname -s)" >&2; return 1 ;;
  esac
}

find_android_ndk() {
  local candidate
  local candidates=()
  if [ -n "${ANDROID_NDK_HOME:-}" ]; then candidates+=("$ANDROID_NDK_HOME"); fi
  if [ -n "${ANDROID_NDK_ROOT:-}" ]; then candidates+=("$ANDROID_NDK_ROOT"); fi
  if [ -n "${ANDROID_HOME:-}" ]; then candidates+=("$ANDROID_HOME/ndk"/*); fi
  if [ -n "${ANDROID_SDK_ROOT:-}" ]; then candidates+=("$ANDROID_SDK_ROOT/ndk"/*); fi
  candidates+=("$HOME/Android/Sdk/ndk"/*)
  candidates+=("/opt/android-sdk/ndk"/*)
  candidates+=("/opt/android-ndk")

  for candidate in "${candidates[@]}"; do
    [ -f "$candidate/build/cmake/android.toolchain.cmake" ] || continue
    printf '%s\n' "$candidate"
    return 0
  done
  return 1
}

require_android_ndk() {
  local ndk_root
  if ! ndk_root="$(find_android_ndk)"; then
    echo "ERROR: Nie znaleziono Android NDK." >&2
    echo "Uruchom ./scripts/setup.sh albo zainstaluj NDK przez Android SDK Manager." >&2
    return 1
  fi
  export ANDROID_NDK_HOME="$ndk_root"
  export ANDROID_NDK_ROOT="$ndk_root"
  printf '%s\n' "$ndk_root"
}
