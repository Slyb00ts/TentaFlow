#!/usr/bin/env bash
# ===== File: scripts/lib/versions.sh — loads scripts/versions.env into the environment =====
#
# Sourced by every bash build/setup script. The file is parsed, never sourced,
# so a malformed line cannot execute anything. A variable already present in
# the environment wins over the file (one-off override for bisecting).

TENTAFLOW_VERSIONS_FILE="${TENTAFLOW_VERSIONS_FILE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/versions.env}"
# Keys whose value came from the environment instead of the file.
TENTAFLOW_VERSION_OVERRIDES=" "

load_tentaflow_versions() {
  local line key value lineno=0
  [ -f "$TENTAFLOW_VERSIONS_FILE" ] || {
    echo "Missing $TENTAFLOW_VERSIONS_FILE" >&2
    return 1
  }
  while IFS= read -r line || [ -n "$line" ]; do
    lineno=$((lineno + 1))
    line="${line%$'\r'}"
    case "$line" in
      ''|'#'*) continue ;;
    esac
    if [[ ! "$line" =~ ^([A-Z][A-Z0-9_]*)=([^[:space:]\"\'\`\$]*)$ ]]; then
      echo "$TENTAFLOW_VERSIONS_FILE:$lineno: malformed line: $line" >&2
      return 1
    fi
    key="${BASH_REMATCH[1]}"
    value="${BASH_REMATCH[2]}"
    if [ -n "${!key+x}" ]; then
      [ "${!key}" = "$value" ] || TENTAFLOW_VERSION_OVERRIDES+="$key "
    else
      export "$key=$value"
    fi
  done < "$TENTAFLOW_VERSIONS_FILE"
}

# Prints the pinned value of KEY or fails loudly: a missing pin must stop the
# build instead of silently downloading something unverified.
require_version() {
  local key="$1"
  if [ -z "${!key:-}" ]; then
    echo "$key is not defined in $TENTAFLOW_VERSIONS_FILE" >&2
    return 1
  fi
  printf '%s\n' "${!key}"
}

# Checksum of an artifact whose version may have been overridden from the
# environment. The file's checksum belongs to the file's version only, so an
# overridden version needs its checksum overridden too.
pinned_checksum() {
  local checksum_key="$1" version_key="$2"
  if [[ "$TENTAFLOW_VERSION_OVERRIDES" == *" $version_key "* ]] \
     && [[ "$TENTAFLOW_VERSION_OVERRIDES" != *" $checksum_key "* ]]; then
    echo "$version_key=${!version_key} overrides $TENTAFLOW_VERSIONS_FILE; export $checksum_key=<sha256> of that artifact as well." >&2
    return 1
  fi
  require_version "$checksum_key"
}

load_tentaflow_versions
