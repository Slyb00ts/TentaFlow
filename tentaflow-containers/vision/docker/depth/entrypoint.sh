#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Punkt wejścia kontenera serwisu głębi. MODEL wskazuje repo HF (z presetu
#       deployu), PORT to port HTTP nasłuchu (default 8096). Uruchamia uvicorn z
#       server.py bezpośrednio (direct-http, bez sidecara).
# =============================================================================
set -euo pipefail

export MODEL="${MODEL:-depth-anything/Depth-Anything-V2-Metric-Indoor-Large-hf}"
export PORT="${PORT:-8096}"

exec uvicorn --app-dir /app server:app --host 0.0.0.0 --port "$PORT"
