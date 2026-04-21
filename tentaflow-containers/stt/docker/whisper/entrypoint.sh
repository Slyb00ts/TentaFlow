#!/usr/bin/env bash
set -eo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

MODEL_PATH="${MODEL_PATH:-/data/models/ggml-large-v3-q5_0.bin}"
PORT="${WHISPER_PORT:-8081}"
WHISPER_ARGS="${WHISPER_ARGS:---threads 4 --processors 1}"

if [[ ! -f "$MODEL_PATH" ]]; then
  echo "[entrypoint] ERROR: MODEL_PATH=$MODEL_PATH nie istnieje"
  exit 1
fi

echo "[entrypoint] whisper-server $MODEL_PATH na 127.0.0.1:$PORT"
# shellcheck disable=SC2086
whisper-server \
  --host 127.0.0.1 --port "$PORT" \
  --model "$MODEL_PATH" \
  $WHISPER_ARGS \
  >/tmp/whisper.log 2>&1 &
PID=$!

for i in $(seq 1 60); do
  if curl -fsS "http://127.0.0.1:$PORT/" >/dev/null 2>&1; then
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
