#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Serwer Nemotron-Parse (FastAPI, direct-http, bez sidecara). Core gada
#       HTTP wprost do host-mapped portu. server.py laduje model leniwie na GPU
#       (CUDA). Bind 0.0.0.0 wewnatrz kontenera.
# =============================================================================

set -uo pipefail

export MODEL="${MODEL:-nvidia/NVIDIA-Nemotron-Parse-v1.2}"
PORT="${PORT:-${PARSE_PORT:-8094}}"
export PORT

echo "[entrypoint] nemotron-parse server na 0.0.0.0:$PORT (model=$MODEL)"
exec uvicorn --app-dir /app server:app --host 0.0.0.0 --port "$PORT"
