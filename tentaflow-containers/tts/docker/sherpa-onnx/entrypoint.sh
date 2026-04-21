#!/usr/bin/env bash
set -eo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

PORT="${SHERPA_PORT:-8084}"
TOKENS="${SHERPA_TOKENS:-/data/models/tokens.txt}"
ACOUSTIC="${SHERPA_ACOUSTIC:-/data/models/model.onnx}"
LEXICON="${SHERPA_LEXICON:-}"

ARGS=(--port "$PORT" --vits-model="$ACOUSTIC" --vits-tokens="$TOKENS")
[[ -n "$LEXICON" ]] && ARGS+=(--vits-lexicon="$LEXICON")

echo "[entrypoint] sherpa-tts ${ARGS[*]}"
sherpa-tts "${ARGS[@]}" >/tmp/sherpa.log 2>&1 &
PID=$!

for i in $(seq 1 30); do
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
