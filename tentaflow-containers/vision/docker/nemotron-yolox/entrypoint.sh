#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Serwer detekcji YOLOX (FastAPI, direct-http, bez sidecara). Core gada
#       HTTP wprost do host-mapped portu. Serwer pobiera wagi .pth dla MODEL_REPO,
#       buduje siec YOLOX i laduje ja na GPU (inferencja w PyTorch, bez
#       onnxruntime). Bind 0.0.0.0 wewnatrz kontenera.
# =============================================================================

set -uo pipefail

PORT="${PORT:-${NEMOTRON_YOLOX_PORT:-8086}}"
export PORT

# Core wstrzykuje repo modelu jako env MODEL; serwer yolox czyta MODEL_REPO
# (jeden obraz, trzy modele wybierane repo). Mapujemy MODEL -> MODEL_REPO.
export MODEL_REPO="${MODEL_REPO:-$MODEL}"

echo "[entrypoint] start nemotron-yolox (model=${MODEL_REPO:-?}) na 0.0.0.0:$PORT"
exec python /app/server.py
