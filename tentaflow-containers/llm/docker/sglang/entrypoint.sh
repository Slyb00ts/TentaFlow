#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: Uruchamia SGLang serwer (127.0.0.1:30000) + sidecar QUIC.
# =============================================================================

set -eo pipefail

CONFIG_PATH="${CONFIG_PATH:-/data/config.toml}"
[[ -f "$CONFIG_PATH" ]] || CONFIG_PATH=/app/config.default.toml

MODEL="${MODEL:?MODEL env required}"
SGLANG_PORT="${SGLANG_PORT:-30000}"
SGLANG_ARGS="${SGLANG_ARGS:---tp 1 --mem-fraction-static 0.85}"

echo "[entrypoint] sglang serve $MODEL"
# shellcheck disable=SC2086
python3 -m sglang.launch_server \
  --model-path "$MODEL" \
  --host 127.0.0.1 \
  --port "$SGLANG_PORT" \
  $SGLANG_ARGS \
  >/tmp/sglang.log 2>&1 &
PID=$!

for i in $(seq 1 600); do
  if curl -fsS "http://127.0.0.1:$SGLANG_PORT/v1/models" >/dev/null 2>&1; then
    echo "[entrypoint] sglang gotowy po ${i}s"
    break
  fi
  sleep 1
done

cleanup() {
  echo "[entrypoint] shutdown: zabijam sglang ($PID)"
  kill -TERM "$PID" 2>/dev/null || true
  wait "$PID" 2>/dev/null || true
}
trap cleanup SIGTERM SIGINT

exec /usr/local/bin/tentaflow-sidecar --config "$CONFIG_PATH"
