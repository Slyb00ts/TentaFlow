#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: VoxCPM2 — direct-http (bez sidecara). Core gada HTTP wprost do
#       host-mapped portu. Server binduje 0.0.0.0:${PORT}.
# =============================================================================

set -uo pipefail

PORT="${PORT:-8086}"

echo "[entrypoint] start voxcpm na 0.0.0.0:$PORT"
exec uvicorn --app-dir /app server:app --host 0.0.0.0 --port "$PORT"
