#!/usr/bin/env bash
set -eo pipefail
CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

PORT="${COMFY_PORT:-8188}"

cd /opt/ComfyUI
python3 main.py --listen 127.0.0.1 --port "$PORT" \
  >/tmp/comfy.log 2>&1 &
PID=$!

for i in $(seq 1 120); do
  if curl -fsS "http://127.0.0.1:$PORT/" >/dev/null 2>&1; then break; fi
  sleep 1
done

cleanup() { kill -TERM "$PID" 2>/dev/null || true; wait "$PID" 2>/dev/null || true; }
trap cleanup SIGTERM SIGINT
exec /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH"
