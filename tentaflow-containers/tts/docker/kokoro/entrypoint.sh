#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: kokoro uvicorn server — direct-http (bez sidecara). Core gada HTTP
#       wprost do host-mapped portu. Server binduje 0.0.0.0:${PORT}.
# =============================================================================

set -uo pipefail

PORT="${PORT:-8880}"

echo "[entrypoint] start kokoro server (uvicorn 0.0.0.0:$PORT)"
exec uvicorn server:app --host 0.0.0.0 --port "$PORT"
