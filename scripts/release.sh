#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/release.sh
# Opis: Wymaga gotowych notatek wydania, aktualizuje centralną wersję i lockfile,
#       zapisuje zmiany, tworzy tag i wypycha wydanie. Workflow release.yml
#       buduje oraz publikuje artefakty po otrzymaniu tagu v*.
#
# Usage:
#   ./scripts/release.sh                    # bump patch, keep pre-release suffix
#   ./scripts/release.sh --minor            # bump minor, reset patch
#   ./scripts/release.sh --major            # bump major, reset minor/patch
#   ./scripts/release.sh --finalize         # strip -alpha/-beta/-rc suffix
#   ./scripts/release.sh --set X.Y.Z[-tag]  # set an explicit version
#   ./scripts/release.sh --dry-run          # print the plan and exit
# =============================================================================

set -euo pipefail

# ---- Preflight: required tools ----------------------------------------------
install_hint() {
  if   command -v apt-get >/dev/null 2>&1; then echo "sudo apt-get update && sudo apt-get install -y $1"
  elif command -v dnf     >/dev/null 2>&1; then echo "sudo dnf install -y $1"
  elif command -v pacman  >/dev/null 2>&1; then echo "sudo pacman -S --noconfirm $1"
  elif command -v zypper  >/dev/null 2>&1; then echo "sudo zypper install -y $1"
  elif command -v brew    >/dev/null 2>&1; then echo "brew install $1"
  else echo "install $1 via your package manager"
  fi
}

MISSING=0
check_tool() {
  local tool="$1" pkg="$2"
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "Missing required tool: $tool" >&2
    echo "  Install with: $(install_hint "$pkg")" >&2
    MISSING=1
  fi
}
check_tool git  git
check_tool awk  gawk
check_tool sed  sed
check_tool curl curl
check_tool tar  tar
check_tool python3 python3
check_tool cargo cargo
[[ "$MISSING" == "1" ]] && exit 1

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARGO_TOML="$REPO_ROOT/Cargo.toml"
CARGO_LOCK="$REPO_ROOT/Cargo.lock"
CHANGELOG="$REPO_ROOT/CHANGELOG.md"

MODE="patch"
EXPLICIT=""
DRY_RUN="0"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --patch)    MODE="patch"; shift ;;
    --minor)    MODE="minor"; shift ;;
    --major)    MODE="major"; shift ;;
    --finalize) MODE="finalize"; shift ;;
    --set)      MODE="set"; EXPLICIT="${2:-}"; shift 2 ;;
    --dry-run)  DRY_RUN="1"; shift ;;
    -h|--help)
      grep -E "^#" "$0" | head -20
      exit 0 ;;
    *) echo "Unknown argument: $1" >&2; exit 1 ;;
  esac
done

# --- Read current version -----------------------------------------------------
CURRENT=$(python3 "$REPO_ROOT/scripts/workspace-version.py" --package)
if [[ -z "$CURRENT" ]]; then
  echo "Could not read current version from $CARGO_TOML" >&2
  exit 1
fi
echo "Current version: $CURRENT"

# Parse semver: major.minor.patch[-prerelease]
if [[ ! "$CURRENT" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)(-([A-Za-z0-9.\-]+))?$ ]]; then
  echo "Cannot parse version '$CURRENT' (expected X.Y.Z or X.Y.Z-tag)" >&2
  exit 1
fi
CUR_MAJOR="${BASH_REMATCH[1]}"
CUR_MINOR="${BASH_REMATCH[2]}"
CUR_PATCH="${BASH_REMATCH[3]}"
CUR_PRE="${BASH_REMATCH[5]:-}"

# --- Compute next -------------------------------------------------------------
next_version() {
  local major="$CUR_MAJOR" minor="$CUR_MINOR" patch="$CUR_PATCH" pre="$CUR_PRE"
  case "$MODE" in
    patch)
      patch=$((patch + 1)) ;;
    minor)
      minor=$((minor + 1)); patch=0 ;;
    major)
      major=$((major + 1)); minor=0; patch=0 ;;
    finalize)
      pre="" ;;
    set)
      echo "$EXPLICIT"; return ;;
  esac
  local out="$major.$minor.$patch"
  [[ -n "$pre" ]] && out="$out-$pre"
  echo "$out"
}

NEXT=$(next_version)
if [[ -z "$NEXT" ]]; then
  echo "Next version is empty" >&2; exit 1
fi
if [[ ! "$NEXT" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.\-]+)?$ ]]; then
  echo "Niepoprawna wersja '$NEXT' (oczekiwano X.Y.Z lub X.Y.Z-tag)." >&2; exit 1
fi
TAG="v$NEXT"
DATE="$(date -u +%Y-%m-%d)"

echo "Next version:    $NEXT"
echo "Tag:             $TAG"
echo "Date:            $DATE"
echo "Mode:            $MODE"

# --- Dry run? -----------------------------------------------------------------
if [[ "$DRY_RUN" == "1" ]]; then
  echo ""
  echo "Dry run — nothing written or pushed."
  exit 0
fi

# --- Safety: clean working tree -----------------------------------------------
if [[ -n "$(git -C "$REPO_ROOT" status --porcelain)" ]]; then
  echo "Working tree is dirty. Commit or stash your changes first." >&2
  git -C "$REPO_ROOT" status --short >&2
  exit 1
fi

if git -C "$REPO_ROOT" rev-parse "$TAG" >/dev/null 2>&1; then
  echo "Tag $TAG already exists. Pick a different version." >&2
  exit 1
fi

# Notatki wydania muszą być przygotowane przed uruchomieniem skryptu.
if ! awk -v heading="## [$NEXT]" '$0 == heading || index($0, heading " ") == 1 { found = 1 } END { exit !found }' "$CHANGELOG"; then
  echo "Brak gotowych notatek ## [$NEXT] w CHANGELOG.md. Przygotuj i skomituj je przed wydaniem." >&2
  exit 1
fi

# --- Bump Cargo.toml ----------------------------------------------------------
echo ""
echo "==> Updating $CARGO_TOML"
# Aktualizujemy wyłącznie wersję pakietu workspace, zachowując wersje zależności.
tmp="$CARGO_TOML.tmp.$$"
awk -v new_ver="$NEXT" '
  /^\[/ { in_package = ($0 ~ /^\[workspace\.package\][[:space:]]*(#.*)?$/) }
  in_package && !done && /^version[[:space:]]*=/ {
    sub(/"[^"]+"/, "\"" new_ver "\"")
    done = 1
  }
  { print }
' "$CARGO_TOML" > "$tmp"
mv "$tmp" "$CARGO_TOML"

echo "==> Aktualizacja wersji pakietów workspace w Cargo.lock"
cargo update --manifest-path "$CARGO_TOML" --workspace --offline
cargo metadata --manifest-path "$CARGO_TOML" --locked --offline --format-version 1 >/dev/null

# --- Commit + tag + push ------------------------------------------------------
echo ""
echo "==> git commit + tag + push"
cd "$REPO_ROOT"
git add "$CARGO_TOML" "$CARGO_LOCK"
if ! git diff --cached --quiet; then
  git commit -m "release: wydaj $TAG"
fi
git tag -a "$TAG" -m "$TAG"
git push origin "$(git rev-parse --abbrev-ref HEAD)"
git push origin "$TAG"

echo ""
echo "==> Done. Track the build here:"
ORIGIN=$(git config --get remote.origin.url | sed -E 's#(git@github.com:|https://github.com/)([^.]+)(\.git)?#\2#')
echo "    https://github.com/$ORIGIN/actions"
echo "    https://github.com/$ORIGIN/releases/tag/$TAG"
