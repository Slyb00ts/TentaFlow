#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Punkt wejścia kontenera DA3. MODEL = repo HF z presetu (domyślnie
#       DA3-LARGE, relatywny), PORT to port HTTP (default 8097). Uruchamia uvicorn
#       z server.py wprost (direct-http).
# =============================================================================
set -euo pipefail

export MODEL="${MODEL:-depth-anything/DA3-LARGE}"
export PORT="${PORT:-8097}"

exec uvicorn --app-dir /app server:app --host 0.0.0.0 --port "$PORT"
