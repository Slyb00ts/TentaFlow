#!/usr/bin/env bash
# =============================================================================
# Plik: run.sh
# Opis: Uruchamia natywny serwer treningowy czytnika OCR tablic (CRNN + CTC)
#       przez `uv` (izolowany Python 3.12 + torch CUDA 13.0). Port nadpisywalny
#       przez PORT (8204).
# =============================================================================

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"

PORT="${PORT:-8204}"
HOST="${HOST:-0.0.0.0}"

exec uv run --project "$HERE" uvicorn server:app --host "$HOST" --port "$PORT"
