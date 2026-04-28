#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/test_docker_engine_e2e.sh
# Opis: Generic e2e test dowolnego docker silnika z sidecarem iroh.
#       Args via env:
#         ENGINE_ID    np. vllm | sglang | llama-cpp | ollama
#         IMAGE        np. tentaflow/vllm:latest
#         MODEL        np. Qwen/Qwen2.5-0.5B-Instruct
#         PROMPT       prompt do testu
#         API          api type: openai-compat (default) | llama_cpp | sherpa | raw_http
#         UPSTREAM_PORT  wewnetrzny port HTTP engine (vllm:8000, sglang:30000, ...)
#         HOST_PORT    host UDP port (default 58000)
#         SIDECAR_TIMEOUT  default 20s
#         ENGINE_TIMEOUT   default 600s
#         EXTRA_ENV    "-e KEY=VAL -e KEY2=VAL2"
#         EXTRA_VOLS   "-v /host:/cont"
#         GPUS         "all" | "" (default "all")
#         READY_PATTERN  regex w docker logs ze swiadczy o gotowosci engine
#                        (default: "Application startup complete|Uvicorn running|Started server process|started successfully|listening on")
# =============================================================================

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ENGINE_ID="${ENGINE_ID:?ENGINE_ID required}"
IMAGE="${IMAGE:-tentaflow/${ENGINE_ID}:latest}"
MODEL="${MODEL:-default}"
PROMPT="${PROMPT:-Say hello in one short sentence.}"
API="${API:-openai-compat}"
UPSTREAM_PORT="${UPSTREAM_PORT:?UPSTREAM_PORT required}"
HOST_PORT="${HOST_PORT:-58000}"
SIDECAR_TIMEOUT="${SIDECAR_TIMEOUT:-20}"
ENGINE_TIMEOUT="${ENGINE_TIMEOUT:-600}"
EXTRA_ENV="${EXTRA_ENV:-}"
EXTRA_VOLS="${EXTRA_VOLS:-}"
GPUS="${GPUS:-all}"
READY_PATTERN="${READY_PATTERN:-Application startup complete|Uvicorn running|Started server process|started successfully|listening on|server listening}"
TEST_KIND="${TEST_KIND:-chat}"  # chat | skip

CONTAINER="tentaflow-${ENGINE_ID}-e2e"
DATA_DIR=$(mktemp -d -t "tentaflow-${ENGINE_ID}-e2e-XXXXXX")
HF_CACHE=${HF_CACHE:-$HOME/.cache/huggingface}

# Sidecar API name dla configu
case "$API" in
  openai-compat|open_ai) SIDECAR_API="open_ai"; UPSTREAM_PATH="/v1" ;;
  llama_cpp)             SIDECAR_API="llama_cpp"; UPSTREAM_PATH="" ;;
  sherpa)                SIDECAR_API="sherpa"; UPSTREAM_PATH="" ;;
  raw_http)              SIDECAR_API="raw_http"; UPSTREAM_PATH="" ;;
  *) echo "[test] zly API: $API"; exit 2 ;;
esac

cleanup() {
  echo ""
  echo "[test] === cleanup ==="
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  rm -rf "$DATA_DIR"
}
trap cleanup EXIT

echo "[test] engine=$ENGINE_ID image=$IMAGE"
echo "[test] data=$DATA_DIR  host_port=${HOST_PORT}/udp  upstream=$UPSTREAM_PORT"
echo "[test] model=$MODEL  api=$SIDECAR_API"

cat >"$DATA_DIR/config.toml" <<EOF
service_name = "${ENGINE_ID}-e2e-test"
model_aliases = ["$MODEL"]

[transport]
port = 5000
secret_key_path = "/data/endpoint-key.bin"
enable_lan_discovery = false
enable_dht_discovery = false

[role]
kind = "reverse_proxy"
upstream_url = "http://127.0.0.1:${UPSTREAM_PORT}${UPSTREAM_PATH}"
api = "${SIDECAR_API}"
timeout_ms = 600000
EOF

docker rm -f "$CONTAINER" >/dev/null 2>&1 || true

GPU_FLAG=""
[[ -n "$GPUS" ]] && GPU_FLAG="--gpus $GPUS"

# shellcheck disable=SC2086
docker run -d --name "$CONTAINER" \
  $GPU_FLAG \
  -p "${HOST_PORT}:5000/udp" \
  -v "$DATA_DIR:/data" \
  -v "$HF_CACHE:/root/.cache/huggingface" \
  -e MODEL="$MODEL" \
  $EXTRA_ENV \
  $EXTRA_VOLS \
  "$IMAGE" >/dev/null

echo "[test] kontener wystartowal: $CONTAINER"

# Wait for sidecar
echo "[test] czekam na sidecar (max ${SIDECAR_TIMEOUT}s)..."
ENDPOINT_ID=""
for i in $(seq 1 "$SIDECAR_TIMEOUT"); do
  EP=$(docker logs "$CONTAINER" 2>&1 | sed -r 's/\x1b\[[0-9;]*[A-Za-z]//g' | grep -oE 'endpoint_id_full=[0-9a-f]{64}' | head -1 | cut -d= -f2 || true)
  if [[ -n "$EP" ]]; then
    ENDPOINT_ID="$EP"
    echo "[test] sidecar gotowy po ${i}s, endpoint_id=$ENDPOINT_ID"
    break
  fi
  sleep 1
done

if [[ -z "$ENDPOINT_ID" ]]; then
  echo "[test] FAIL: sidecar nie zglocil sie w ${SIDECAR_TIMEOUT}s. Logi:"
  docker logs --tail 60 "$CONTAINER" 2>&1
  exit 1
fi

# Wait for engine ready
echo "[test] czekam na ${ENGINE_ID} engine (max ${ENGINE_TIMEOUT}s)..."
READY=0
for i in $(seq 1 "$ENGINE_TIMEOUT"); do
  if docker logs "$CONTAINER" 2>&1 | grep -qE "$READY_PATTERN"; then
    echo "[test] engine gotowy po ${i}s"
    READY=1
    break
  fi
  if (( i % 15 == 0 )); then
    LAST=$(docker logs --tail 2 "$CONTAINER" 2>&1 | tr '\n' ' ' | head -c 200)
    echo "[test] (${i}s) $LAST"
  fi
  sleep 1
done

if [[ "$READY" != "1" ]]; then
  echo "[test] FAIL: engine nie wstal w ${ENGINE_TIMEOUT}s. Logi (60):"
  docker logs --tail 60 "$CONTAINER" 2>&1
  exit 1
fi

if [[ "$TEST_KIND" == "skip" ]]; then
  echo "[test] SKIP inference test (TEST_KIND=skip) - tylko walidacja sidecar+engine startup"
  echo "[test] SUCCESS"
  exit 0
fi

# Real chat test
CLIENT_BIN="$REPO_ROOT/tentaflow-transport/target/release/examples/iroh_test_client"
if [[ ! -x "$CLIENT_BIN" ]]; then
  echo "[test] FAIL: brak binary $CLIENT_BIN"
  exit 1
fi

echo ""
echo "[test] === REAL inference test via iroh ==="
RUST_LOG=info,iroh=warn "$CLIENT_BIN" "$ENDPOINT_ID" "127.0.0.1:${HOST_PORT}" "$MODEL" "$PROMPT" 120
RC=$?

echo ""
if [[ "$RC" == "0" ]]; then
  echo "[test] SUCCESS"
else
  echo "[test] FAIL (exit=$RC)"
  echo "[test] === Engine logs (last 30) ==="
  docker logs --tail 30 "$CONTAINER" 2>&1 | grep -E "\[$ENGINE_ID\]|\[sidecar\]|\[entrypoint\]" | tail -25
fi
exit "$RC"
