#!/usr/bin/env bash
set -eo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

echo "[entrypoint] ollama serve"
ollama serve >/tmp/ollama.log 2>&1 &
PID=$!

for i in $(seq 1 60); do
  if curl -fsS http://127.0.0.1:11434/api/tags >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

# Opcjonalnie: preload modeli z MODEL_PULL="llama3:8b,qwen2.5:7b"
if [[ -n "${MODEL_PULL:-}" ]]; then
  IFS=',' read -ra MODELS <<<"$MODEL_PULL"
  for m in "${MODELS[@]}"; do
    echo "[entrypoint] ollama pull $m"
    ollama pull "$m" || echo "[entrypoint] pull $m nieudany"
  done
fi

cleanup() {
  echo "[entrypoint] shutdown: kill ollama ($PID)"
  kill -TERM "$PID" 2>/dev/null || true
  wait "$PID" 2>/dev/null || true
}
trap cleanup SIGTERM SIGINT

exec /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH"
