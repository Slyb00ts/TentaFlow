#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: kyutai-tts uvicorn server — direct-http (bez sidecara). Core gada HTTP
#       wprost do host-mapped portu. Server binduje 0.0.0.0:${PORT}.
# =============================================================================

set -uo pipefail

PORT="${PORT:-8088}"

echo "[entrypoint] start kyutai-tts server (uvicorn 0.0.0.0:$PORT)"
exec uvicorn server:app --host 0.0.0.0 --port "$PORT"
