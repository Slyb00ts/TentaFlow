#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/test_docker_sglang_e2e.sh
# Opis: e2e test sglang Docker - sidecar + sglang server + iroh chat.
# =============================================================================
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONTAINER=tentaflow-sglang-e2e
HOST_PORT=${HOST_PORT:-58003}
MODEL=${MODEL:-Qwen/Qwen2.5-0.5B-Instruct}
PROMPT=${PROMPT:-"Say hello in one short sentence."}
DATA_DIR=$(mktemp -d -t tentaflow-sglang-e2e-XXXXXX)
HF_CACHE=${HF_CACHE:-$HOME/.cache/huggingface}
SIDECAR_TIMEOUT=${SIDECAR_TIMEOUT:-20}
SGLANG_TIMEOUT=${SGLANG_TIMEOUT:-300}

cleanup() {
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  rm -rf "$DATA_DIR"
}
trap cleanup EXIT

cat >"$DATA_DIR/config.toml" <<EOF
service_name = "sglang-e2e-test"
model_aliases = ["$MODEL"]
[transport]
port = 5000
secret_key_path = "/data/endpoint-key.bin"
enable_lan_discovery = false
enable_dht_discovery = false
[role]
kind = "reverse_proxy"
upstream_url = "http://127.0.0.1:30000/v1"
api = "open_ai"
timeout_ms = 600000
EOF

docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
docker run -d --name "$CONTAINER" \
  --gpus all \
  --shm-size=2g \
  -p "${HOST_PORT}:5000/udp" \
  -v "$DATA_DIR:/data" \
  -v "$HF_CACHE:/root/.cache/huggingface" \
  -e MODEL="$MODEL" \
  -e SGLANG_ARGS="--tp 1 --mem-fraction-static 0.5" \
  tentaflow/sglang:latest >/dev/null
echo "[test] kontener up"

ENDPOINT_ID=""
for i in $(seq 1 "$SIDECAR_TIMEOUT"); do
  EP=$(docker logs "$CONTAINER" 2>&1 | sed -r 's/\x1b\[[0-9;]*[A-Za-z]//g' | grep -oE 'endpoint_id_full=[0-9a-f]{64}' | head -1 | cut -d= -f2 || true)
  [[ -n "$EP" ]] && { ENDPOINT_ID="$EP"; echo "[test] sidecar po ${i}s, ep=$ENDPOINT_ID"; break; }
  sleep 1
done
[[ -z "$ENDPOINT_ID" ]] && { echo "[test] FAIL sidecar"; docker logs --tail 30 "$CONTAINER"; exit 1; }

echo "[test] czekam na sglang (max ${SGLANG_TIMEOUT}s)..."
READY=0
for i in $(seq 1 "$SGLANG_TIMEOUT"); do
  # Sprawdz tylko logi sglang (pominij sidecar/entrypoint)
  if docker logs "$CONTAINER" 2>&1 | grep "^\[sglang\]" | grep -qE "Application startup complete|Uvicorn running on|model loaded|http://127.0.0.1:30000|server started"; then
    echo "[test] sglang gotowy po ${i}s"; READY=1; break
  fi
  # Albo realny health check przez exec
  if docker exec "$CONTAINER" curl -fsS http://127.0.0.1:30000/v1/models >/dev/null 2>&1; then
    echo "[test] sglang HTTP OK po ${i}s"; READY=1; break
  fi
  if (( i % 15 == 0 )); then
    LAST=$(docker logs --tail 1 "$CONTAINER" 2>&1 | head -c 150)
    echo "[test] (${i}s) $LAST"
  fi
  sleep 1
done
[[ "$READY" != "1" ]] && { echo "[test] FAIL sglang"; docker logs --tail 50 "$CONTAINER"; exit 1; }

echo ""
echo "[test] === REAL inference via iroh ==="
CLIENT_BIN="$REPO_ROOT/tentaflow-transport/target/release/examples/iroh_test_client"
RUST_LOG=info,iroh=warn "$CLIENT_BIN" "$ENDPOINT_ID" "127.0.0.1:${HOST_PORT}" "$MODEL" "$PROMPT" 120
RC=$?
[[ "$RC" == "0" ]] && echo "[test] SUCCESS" || { echo "[test] FAIL exit=$RC"; docker logs --tail 30 "$CONTAINER" | grep -E "\[sglang\]|\[sidecar\]" | tail -15; }
exit "$RC"
