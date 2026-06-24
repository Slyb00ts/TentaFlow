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
# Wiele workerow = OSOBNE PROCESY = osobne GIL = PRAWDZIWA rownoleglosc requestow.
# Z 1 workerem uvicorn obsluguje wspolbiezne requesty w WATKACH, ale generate()
# ma ciezki narzut Pythona per-token (petla + logits-processory) -> watki
# kontenduja na GIL -> serial -> GPU ~12-25%. N workerow (kazdy wlasna kopia
# modelu ~3GB na GPU) obsluguje N stron NAPRAWDE rownolegle, wypelniajac GPU.
# Konfigurowalne (PARSE_WORKERS); domyslnie 4 (4x3GB=12GB miesci sie na 3090/24GB).
WORKERS="${PARSE_WORKERS:-4}"
export WORKERS

echo "[entrypoint] nemotron-parse server na 0.0.0.0:$PORT (model=$MODEL, workers=$WORKERS)"
exec uvicorn --app-dir /app server:app --host 0.0.0.0 --port "$PORT" --workers "$WORKERS"
