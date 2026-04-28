#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/test_docker_ollama_e2e.sh
# Opis: e2e test ollama: provision sidecar, run kontener, ollama pull MODEL,
#       chat completion przez iroh.
# =============================================================================
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONTAINER=tentaflow-ollama-e2e
HOST_PORT=${HOST_PORT:-58001}
MODEL=${MODEL:-qwen2.5:0.5b}
PROMPT=${PROMPT:-"Say hello in one short sentence."}
DATA_DIR=$(mktemp -d -t tentaflow-ollama-e2e-XXXXXX)
SIDECAR_TIMEOUT=${SIDECAR_TIMEOUT:-20}
PULL_TIMEOUT=${PULL_TIMEOUT:-300}

cleanup() {
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  rm -rf "$DATA_DIR"
}
trap cleanup EXIT

cat >"$DATA_DIR/config.toml" <<EOF
service_name = "ollama-e2e-test"
model_aliases = ["$MODEL"]
[transport]
port = 5000
secret_key_path = "/data/endpoint-key.bin"
enable_lan_discovery = false
enable_dht_discovery = false
[role]
kind = "reverse_proxy"
upstream_url = "http://127.0.0.1:11434/v1"
api = "open_ai"
timeout_ms = 600000
EOF

docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
docker run -d --name "$CONTAINER" \
  --gpus all \
  -p "${HOST_PORT}:5000/udp" \
  -v "$DATA_DIR:/data" \
  tentaflow/ollama:latest >/dev/null

echo "[test] kontener up"

ENDPOINT_ID=""
for i in $(seq 1 "$SIDECAR_TIMEOUT"); do
  EP=$(docker logs "$CONTAINER" 2>&1 | sed -r 's/\x1b\[[0-9;]*[A-Za-z]//g' | grep -oE 'endpoint_id_full=[0-9a-f]{64}' | head -1 | cut -d= -f2 || true)
  [[ -n "$EP" ]] && { ENDPOINT_ID="$EP"; echo "[test] sidecar gotowy po ${i}s, ep=$ENDPOINT_ID"; break; }
  sleep 1
done
[[ -z "$ENDPOINT_ID" ]] && { echo "[test] FAIL sidecar"; docker logs --tail 30 "$CONTAINER"; exit 1; }

echo "[test] czekam na ollama serve gotowe..."
for i in $(seq 1 60); do
  docker exec "$CONTAINER" curl -fsS http://127.0.0.1:11434/api/tags >/dev/null 2>&1 && { echo "[test] ollama API ready po ${i}s"; break; }
  sleep 1
done

echo "[test] ollama pull $MODEL (max ${PULL_TIMEOUT}s)..."
timeout "$PULL_TIMEOUT" docker exec "$CONTAINER" ollama pull "$MODEL" 2>&1 | tail -5
RC=${PIPESTATUS[0]}
[[ "$RC" != "0" ]] && { echo "[test] FAIL pull (rc=$RC)"; exit 1; }

echo ""
echo "[test] === REAL inference via iroh ==="
CLIENT_BIN="$REPO_ROOT/tentaflow-transport/target/release/examples/iroh_test_client"
RUST_LOG=info,iroh=warn "$CLIENT_BIN" "$ENDPOINT_ID" "127.0.0.1:${HOST_PORT}" "$MODEL" "$PROMPT" 180
RC=$?
[[ "$RC" == "0" ]] && echo "[test] SUCCESS" || { echo "[test] FAIL exit=$RC"; docker logs --tail 30 "$CONTAINER" | grep -E "\[ollama\]|\[sidecar\]" | tail -15; }
exit "$RC"
