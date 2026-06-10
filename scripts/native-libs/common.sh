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
