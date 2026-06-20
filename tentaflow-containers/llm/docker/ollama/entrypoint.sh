#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: ollama (direct-http, bez sidecara). Core gada HTTP wprost do
#       host-mapped portu. ollama bind przez OLLAMA_HOST=0.0.0.0:$PORT i biegnie
#       jako PID1. Opcjonalny preload modeli leci w tle przez MODEL_PULL.
# =============================================================================

set -uo pipefail

# Edytowalna komenda z wizarda (Override): gdy Core ustawi ENGINE_LAUNCH_CMD,
# odpalamy ja verbatim zamiast budowanej nizej komendy.
if [ -n "${ENGINE_LAUNCH_CMD:-}" ]; then
  echo "[entrypoint] ENGINE_LAUNCH_CMD override"
  exec sh -c "$ENGINE_LAUNCH_CMD"
fi

OLLAMA_PORT="${PORT:-${OLLAMA_PORT:-11434}}"
# ollama nie ma flag --host/--port; bind ustawia OLLAMA_HOST.
export OLLAMA_HOST="0.0.0.0:${OLLAMA_PORT}"

# Opcjonalny preload modeli (w tle, czeka az ollama wstanie) przez
# MODEL_PULL="llama3:8b,qwen2.5:7b". Nie blokuje startu serwera.
if [[ -n "${MODEL_PULL:-}" ]]; then
  (
    for i in $(seq 1 60); do
      curl -fsS "http://127.0.0.1:${OLLAMA_PORT}/api/tags" >/dev/null 2>&1 && break
      sleep 1
    done
    IFS=',' read -ra MODELS <<<"$MODEL_PULL"
    for m in "${MODELS[@]}"; do
      echo "[entrypoint] ollama pull $m"
      ollama pull "$m" 2>&1 | sed -u 's/^/[ollama-pull] /' || echo "[entrypoint] pull $m nieudany"
    done
  ) &
fi

echo "[entrypoint] start ollama na ${OLLAMA_HOST}"
exec ollama serve
