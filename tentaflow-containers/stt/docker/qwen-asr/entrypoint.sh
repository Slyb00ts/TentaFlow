#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: qwen-asr FastAPI wrapper direct-http (bez sidecara). Core gada HTTP
#       wprost do host-mapped portu. Bind 0.0.0.0 wewnatrz kontenera;
#       containment robi host bind. Engine laduje model przy pierwszym request.
# =============================================================================

set -uo pipefail

PORT="${PORT:-8083}"

echo "[entrypoint] start qwen-asr na 0.0.0.0:$PORT"
exec uvicorn --app-dir /app server:app --host 0.0.0.0 --port "$PORT"
