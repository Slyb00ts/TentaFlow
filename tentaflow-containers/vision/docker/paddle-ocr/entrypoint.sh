#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Serwer PaddleOCR (FastAPI, direct-http, bez sidecara). Core gada HTTP
#       wprost do host-mapped portu. server.py inicjalizuje silnik OCR przy
#       pierwszym zadaniu. Bind 0.0.0.0 wewnatrz kontenera.
# =============================================================================

set -uo pipefail

PORT="${PORT:-${OCR_PORT:-8095}}"
export PORT
export OCR_LANG="${OCR_LANG:-en}"

echo "[entrypoint] paddle-ocr server na 0.0.0.0:$PORT (lang=$OCR_LANG)"
exec uvicorn --app-dir /app server:app --host 0.0.0.0 --port "$PORT"
