#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Sidecar QUIC + ollama startuja rownolegle. Sidecar nasluchuje
#       iroh natychmiast (klient widzi peera od razu); engine laduje model w
#       tle. Logi obu na stdout kontenera. PID1 czeka na pierwszego upadku
#       i grzecznie konczy drugiego.
# =============================================================================

set -uo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

# Opcjonalny preload modeli przez MODEL_PULL="llama3:8b,qwen2.5:7b" (po starcie ollama).

echo "[entrypoint] sidecar config=$CONFIG_PATH"
NO_COLOR=1 /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH" 2>&1 \
  | sed -u 's/^/[sidecar] /' &
SIDECAR_PID=$!
echo "[entrypoint] sidecar PID=$SIDECAR_PID"

echo "[entrypoint] start ollama"
ollama serve 2>&1 \
  | sed -u 's/^/[ollama] /' &
ENGINE_PID=$!
echo "[entrypoint] ollama PID=$ENGINE_PID"

# Opcjonalny preload modeli (w tle, nie blokuje sidecara)
if [[ -n "${MODEL_PULL:-}" ]]; then
  (
    for i in $(seq 1 60); do
      curl -fsS http://127.0.0.1:11434/api/tags >/dev/null 2>&1 && break
      sleep 1
    done
    IFS=',' read -ra MODELS <<<"$MODEL_PULL"
    for m in "${MODELS[@]}"; do
      echo "[entrypoint] ollama pull $m"
      ollama pull "$m" 2>&1 | sed -u 's/^/[ollama-pull] /' || echo "[entrypoint] pull $m nieudany"
    done
  ) &
fi

cleanup() {
  echo "[entrypoint] shutdown sidecar=$SIDECAR_PID engine=$ENGINE_PID"
  kill -TERM "$SIDECAR_PID" 2>/dev/null || true
  kill -TERM "$ENGINE_PID" 2>/dev/null || true
  wait "$SIDECAR_PID" 2>/dev/null || true
  wait "$ENGINE_PID" 2>/dev/null || true
}
trap cleanup SIGTERM SIGINT

wait -n "$SIDECAR_PID" "$ENGINE_PID"
EXIT_CODE=$?
echo "[entrypoint] proces ($EXIT_CODE) zakonczony - wychodze"
cleanup
exit $EXIT_CODE
