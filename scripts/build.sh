#!/usr/bin/env bash
# =============================================================================
# Plik: build.sh
# Opis: Wspólny punkt wejścia Cargo z retencją cache na Linux i macOS.
# Przykład: scripts/build.sh test -p tentaflow-core --lib
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BUILD_COMMAND=build
if [[ "${1:-}" =~ ^(build|test|check|prune|report)$ ]]; then
    BUILD_COMMAND="$1"
    shift
fi
TENTAFLOW_PYTHON=$(bash "$SCRIPT_DIR/ensure-python.sh")
export TENTAFLOW_PYTHON
exec "$TENTAFLOW_PYTHON" "$SCRIPT_DIR/cargo-build.py" "$BUILD_COMMAND" "$@"
