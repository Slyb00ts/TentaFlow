#!/usr/bin/env bash
# =============================================================================
# File:        scripts/release.sh
# Description: Bumps TentaFlow version, updates CHANGELOG, commits, tags
#              and pushes. GitHub Actions workflow (.github/workflows/release.yml)
#              picks up the `v*` tag and builds + publishes the release.
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
[[ "$MISSING" == "1" ]] && exit 1

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARGO_TOML="$REPO_ROOT/tentaflow/Cargo.toml"
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
CURRENT=$(grep -E '^version\s*=' "$CARGO_TOML" | head -1 | sed -E 's/.*"([^"]+)".*/\1/')
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

# --- Bump Cargo.toml ----------------------------------------------------------
echo ""
echo "==> Updating $CARGO_TOML"
# Only the FIRST `version = "..."` (under [package]).
# portable sed: write to tmp file
tmp="$CARGO_TOML.tmp.$$"
awk -v new_ver="$NEXT" '
  !done && /^version[[:space:]]*=/ {
    sub(/"[^"]+"/, "\"" new_ver "\"")
    done = 1
  }
  { print }
' "$CARGO_TOML" > "$tmp"
mv "$tmp" "$CARGO_TOML"

# --- Prepend a new section to CHANGELOG ---------------------------------------
echo "==> Updating $CHANGELOG"
tmp_log="$CHANGELOG.tmp.$$"
awk -v ver="$NEXT" -v date="$DATE" '
  BEGIN { inserted = 0 }
  /^## \[Unreleased\]/ && !inserted {
    print
    print ""
    print "## [" ver "] - " date
    print ""
    print "### Added"
    print "- TBD — describe the changes before tagging."
    print ""
    inserted = 1
    next
  }
  { print }
  END {
    if (!inserted) {
      print ""
      print "## [" ver "] - " date
      print ""
      print "### Added"
      print "- TBD — describe the changes before tagging."
    }
  }
' "$CHANGELOG" > "$tmp_log"
mv "$tmp_log" "$CHANGELOG"

echo ""
echo "Please review CHANGELOG.md entry for $NEXT and adjust the 'TBD' bullet."
echo "Opening editor..."
${EDITOR:-nano} "$CHANGELOG" || true

# --- Commit + tag + push ------------------------------------------------------
echo ""
echo "==> git commit + tag + push"
cd "$REPO_ROOT"
git add "$CARGO_TOML" "$CHANGELOG"
git commit -m "release: $TAG"
git tag -a "$TAG" -m "$TAG"
git push origin "$(git rev-parse --abbrev-ref HEAD)"
git push origin "$TAG"

echo ""
echo "==> Done. Track the build here:"
ORIGIN=$(git config --get remote.origin.url | sed -E 's#(git@github.com:|https://github.com/)([^.]+)(\.git)?#\2#')
echo "    https://github.com/$ORIGIN/actions"
echo "    https://github.com/$ORIGIN/releases/tag/$TAG"
