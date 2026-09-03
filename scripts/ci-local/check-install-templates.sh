#!/usr/bin/env bash
# =============================================================================
# File: scripts/ci-local/check-install-templates.sh
# Purpose: The unit and plist templates are filled in by install.sh with a list
#          of sed expressions. Adding a placeholder to a template without adding
#          its expression leaves a literal @PLACEHOLDER@ in a systemd unit or a
#          launchd plist — which fails at service start, on a user's machine,
#          long after CI was green. This checks the two lists agree.
# =============================================================================
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
INSTALLER="$ROOT/scripts/install/install.sh"
rc=0

for template in "$ROOT"/scripts/install/*.in; do
  name=$(basename "$template")
  placeholders=$(grep -o '@[A-Z_]\+@' "$template" | sort -u)
  [ -n "$placeholders" ] || { echo "$name: brak placeholderow"; continue; }
  for ph in $placeholders; do
    if grep -q -- "s|$ph|" "$INSTALLER"; then
      echo "ok   $name $ph"
    else
      echo "BRAK $name $ph — install.sh nie podstawia tego placeholdera" >&2
      rc=1
    fi
  done
done

exit "$rc"
