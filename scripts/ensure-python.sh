#!/usr/bin/env bash
# =============================================================================
# Plik: ensure-python.sh
# Opis: Wskazuje Python 3.11+; tryb --install zapewnia zarządzany Python 3.12.
# Przykład: TENTAFLOW_PYTHON="$(scripts/ensure-python.sh --install)"
# =============================================================================
set -euo pipefail

for PYTHON_CANDIDATE in "${TENTAFLOW_PYTHON:-}" python3 python3.12; do
    if [[ -n "$PYTHON_CANDIDATE" ]] && command -v "$PYTHON_CANDIDATE" >/dev/null 2>&1; then
        if PYTHON_PATH=$("$PYTHON_CANDIDATE" -c 'import sys; sys.exit(1) if sys.version_info < (3, 11) else print(sys.executable)' 2>/dev/null); then
            printf '%s\n' "$PYTHON_PATH"
            exit 0
        fi
    fi
done

UV_COMMAND=$(command -v uv || true)
if [[ -z "$UV_COMMAND" && -x "$HOME/.local/bin/uv" ]]; then
    UV_COMMAND="$HOME/.local/bin/uv"
fi
if [[ -n "$UV_COMMAND" ]]; then
    if PYTHON_PATH=$("$UV_COMMAND" python find --no-project --managed-python --no-python-downloads 3.12 2>/dev/null); then
        printf '%s\n' "$PYTHON_PATH"
        exit 0
    fi
fi
if [[ "${1:-}" != --install ]]; then
    echo "Wymagany Python 3.11+; uruchom scripts/ensure-python.sh --install lub scripts/setup.sh." >&2
    exit 1
fi
if [[ -z "$UV_COMMAND" ]]; then
    INSTALLER_PATH=$(mktemp)
    trap 'rm -f "$INSTALLER_PATH"' EXIT
    curl --fail --silent --show-error --location https://astral.sh/uv/install.sh --output "$INSTALLER_PATH"
    sh "$INSTALLER_PATH" >&2
    UV_COMMAND="$HOME/.local/bin/uv"
fi
"$UV_COMMAND" python install 3.12 >&2
"$UV_COMMAND" python find --no-project --managed-python --no-python-downloads 3.12
