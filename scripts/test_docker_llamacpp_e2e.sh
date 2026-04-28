#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/test_docker_llamacpp_e2e.sh
# Opis: e2e test llama.cpp Docker - z pre-mountowanym GGUF, sidecar iroh,
#       chat completion via test client.
# =============================================================================
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONTAINER=tentaflow-llamacpp-e2e
HOST_PORT=${HOST_PORT:-58002}
GGUF_PATH=${GGUF_PATH:-/tmp/tentaflow-models/qwen2.5-0.5b-instruct-q4_k_m.gguf}
PROMPT=${PROMPT:-"Say hello in one short sentence."}
DATA_DIR=$(mktemp -d -t tentaflow-llamacpp-e2e-XXXXXX)
SIDECAR_TIMEOUT=${SIDECAR_TIMEOUT:-20}
LLAMA_TIMEOUT=${LLAMA_TIMEOUT:-90}

[[ ! -f "$GGUF_PATH" ]] && { echo "[test] FAIL: brak GGUF: $GGUF_PATH"; exit 1; }

cleanup() {
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  rm -rf "$DATA_DIR"
}
trap cleanup EXIT

# Sidecar config: llama.cpp api (sidecar tlumaczy POST /v1/chat/completions)
cat >"$DATA_DIR/config.toml" <<EOF
service_name = "llamacpp-e2e-test"
model_aliases = ["llama-cpp"]
[transport]
port = 5000
secret_key_path = "/data/endpoint-key.bin"
enable_lan_discovery = false
enable_dht_discovery = false
[role]
kind = "reverse_proxy"
upstream_url = "http://127.0.0.1:8080"
api = "llama_cpp"
timeout_ms = 600000
EOF

mkdir -p "$DATA_DIR/models"
cp "$GGUF_PATH" "$DATA_DIR/models/model.gguf"

docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
docker run -d --name "$CONTAINER" \
  --gpus all \
  -p "${HOST_PORT}:5000/udp" \
  -v "$DATA_DIR:/data" \
  -e MODEL_PATH=/data/models/model.gguf \
  -e LLAMA_ARGS="--n-gpu-layers 99 --ctx-size 4096" \
  tentaflow/llama-cpp:latest >/dev/null

echo "[test] kontener up"

ENDPOINT_ID=""
for i in $(seq 1 "$SIDECAR_TIMEOUT"); do
  EP=$(docker logs "$CONTAINER" 2>&1 | sed -r 's/\x1b\[[0-9;]*[A-Za-z]//g' | grep -oE 'endpoint_id_full=[0-9a-f]{64}' | head -1 | cut -d= -f2 || true)
  [[ -n "$EP" ]] && { ENDPOINT_ID="$EP"; echo "[test] sidecar gotowy po ${i}s, ep=$ENDPOINT_ID"; break; }
  sleep 1
done
[[ -z "$ENDPOINT_ID" ]] && { echo "[test] FAIL sidecar"; docker logs --tail 30 "$CONTAINER"; exit 1; }

echo "[test] czekam na llama-server (max ${LLAMA_TIMEOUT}s)..."
READY=0
for i in $(seq 1 "$LLAMA_TIMEOUT"); do
  if docker exec "$CONTAINER" curl -fsS http://127.0.0.1:8080/health >/dev/null 2>&1; then
    echo "[test] llama-server gotowy po ${i}s"
    READY=1
    break
  fi
  sleep 1
done
[[ "$READY" != "1" ]] && { echo "[test] FAIL llama-server"; docker logs --tail 30 "$CONTAINER"; exit 1; }

echo ""
echo "[test] === REAL inference via iroh ==="
CLIENT_BIN="$REPO_ROOT/tentaflow-transport/target/release/examples/iroh_test_client"
RUST_LOG=info,iroh=warn "$CLIENT_BIN" "$ENDPOINT_ID" "127.0.0.1:${HOST_PORT}" "qwen2.5-0.5b" "$PROMPT" 60
RC=$?
[[ "$RC" == "0" ]] && echo "[test] SUCCESS" || { echo "[test] FAIL exit=$RC"; docker logs --tail 30 "$CONTAINER" | grep -E "\[llama\]|\[sidecar\]" | tail -15; }
exit "$RC"
