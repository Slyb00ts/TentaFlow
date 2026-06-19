#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: sherpa-onnx TTS (python FastAPI) — direct-http (bez sidecara). server.py
#       pobiera model VITS z env MODEL i wystawia /audio/speech na 0.0.0.0:PORT.
# =============================================================================

set -uo pipefail

PORT="${PORT:-8084}"

echo "[entrypoint] start sherpa-onnx TTS (uvicorn 0.0.0.0:$PORT, model=${MODEL:-?})"
exec uvicorn server:app --host 0.0.0.0 --port "$PORT"
