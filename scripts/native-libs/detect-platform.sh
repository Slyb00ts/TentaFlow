#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/native-libs/detect-platform.sh
# Opis: Wykrywa identyfikator platformy używany przez katalog native-libs.
# =============================================================================

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/common.sh"

detect_platform
