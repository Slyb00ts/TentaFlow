#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/native-libs/common.sh
# Opis: Wspólne funkcje dla skryptów budujących natywne biblioteki TentaFlow.
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
NATIVE_ROOT="$ROOT/native-libs"
NATIVE_CACHE="${TENTAFLOW_NATIVE_CACHE:-/tmp/tentaflow-native-libs}"

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
    aarch64|arm64) arch="arm64" ;;
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

prepare_layout() {
  local platform="$1"
  mkdir -p \
    "$NATIVE_ROOT/$platform/include" \
    "$NATIVE_ROOT/$platform/lib-static" \
    "$NATIVE_ROOT/$platform/lib-dynamic" \
    "$NATIVE_CACHE/src" \
    "$NATIVE_CACHE/build"
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
  find "$src" -type f \( "$@" \) -exec cp {} "$dst/" \;
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
