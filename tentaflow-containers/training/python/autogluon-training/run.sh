#!/usr/bin/env bash
# =============================================================================
# Plik: run.sh
# Opis: Uruchamia natywny tabularny serwer treningowy AutoGluon przez `uv`
#       (izolowany Python 3.11, tylko CPU). Port nadpisywalny przez PORT
#       (domyślnie 8201).
# =============================================================================

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"

PORT="${PORT:-8201}"
HOST="${HOST:-0.0.0.0}"

exec uv run --project "$HERE" uvicorn server:app --host "$HOST" --port "$PORT"
