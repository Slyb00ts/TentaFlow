#!/usr/bin/env bash
set -eo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

PORT="${PARAKEET_PORT:-8082}"

echo "[entrypoint] uvicorn parakeet server na 127.0.0.1:$PORT"
uvicorn --app-dir /app server:app --host 127.0.0.1 --port "$PORT" \
  >/tmp/parakeet.log 2>&1 &
PID=$!

for i in $(seq 1 120); do
  if curl -fsS "http://127.0.0.1:$PORT/v1/models" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

cleanup() {
  kill -TERM "$PID" 2>/dev/null || true
  wait "$PID" 2>/dev/null || true
}
trap cleanup SIGTERM SIGINT

exec /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH"
