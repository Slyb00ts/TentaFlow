#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Serwer Nemotron-OCR (FastAPI, direct-http, bez sidecara). Core gada HTTP
#       wprost do host-mapped portu. server.py laduje model leniwie na GPU.
#       Bind 0.0.0.0 wewnatrz kontenera.
# =============================================================================

set -uo pipefail

export MODEL="${MODEL:-nvidia/nemotron-ocr-v1}"
PORT="${PORT:-${OCR_PORT:-8093}}"
export PORT

echo "[entrypoint] nemotron-ocr server na 0.0.0.0:$PORT (model=$MODEL)"
exec uvicorn --app-dir /app server:app --host 0.0.0.0 --port "$PORT"
