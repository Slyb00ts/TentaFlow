#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: XTTS v2 (coqui) — direct-http (bez sidecara). Core gada HTTP wprost do
#       host-mapped portu. Server binduje 0.0.0.0:${PORT}.
# =============================================================================

set -uo pipefail

PORT="${PORT:-8085}"

echo "[entrypoint] start xtts na 0.0.0.0:$PORT"
exec uvicorn --app-dir /app server:app --host 0.0.0.0 --port "$PORT"
