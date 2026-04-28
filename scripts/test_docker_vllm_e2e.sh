#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/test_docker_vllm_e2e.sh
# Opis: End-to-end real test wdrozenia vllm w Dockerze + polaczenie iroh.
#       1. Generuje Ed25519 secret key + config.toml dla sidecara
#       2. Uruchamia kontener vllm z mountowanym /data
#       3. Czeka az sidecar zgloci "iroh service server akceptuje" w logach
#       4. Uruchamia tentaflow-transport example iroh_test_client z hosta
#       5. Cleanup
# =============================================================================

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CONTAINER=tentaflow-vllm-e2e
HOST_PORT=${HOST_PORT:-58000}
MODEL=${MODEL:-Qwen/Qwen2.5-0.5B-Instruct}
PROMPT=${PROMPT:-"Say hello in one short sentence."}
DATA_DIR=$(mktemp -d -t tentaflow-vllm-e2e-XXXXXX)
HF_CACHE=${HF_CACHE:-$HOME/.cache/huggingface}
SIDECAR_TIMEOUT=${SIDECAR_TIMEOUT:-30}
VLLM_TIMEOUT=${VLLM_TIMEOUT:-600}

cleanup() {
  echo ""
  echo "[test] === cleanup ==="
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  rm -rf "$DATA_DIR"
}
trap cleanup EXIT

echo "[test] data dir: $DATA_DIR"
echo "[test] host port: $HOST_PORT (UDP)"
echo "[test] model: $MODEL"

# 1. Wygeneruj config.toml dla sidecara (klucz wygeneruje sam sidecar przy starcie)
cat >"$DATA_DIR/config.toml" <<EOF
service_name = "vllm-e2e-test"
model_aliases = ["$MODEL"]

[transport]
port = 5000
secret_key_path = "/data/endpoint-key.bin"
enable_lan_discovery = false
enable_dht_discovery = false

[role]
kind = "reverse_proxy"
upstream_url = "http://127.0.0.1:8000/v1"
api = "open_ai"
timeout_ms = 600000
EOF
echo "[test] sidecar config wygenerowany"

# 3. Run kontener
docker rm -f "$CONTAINER" >/dev/null 2>&1 || true

docker run -d --name "$CONTAINER" \
  --gpus all \
  -p "${HOST_PORT}:5000/udp" \
  -v "$DATA_DIR:/data" \
  -v "$HF_CACHE:/root/.cache/huggingface" \
  -e MODEL="$MODEL" \
  -e VLLM_ARGS="--dtype auto --gpu-memory-utilization 0.5 --max-model-len 4096 --enforce-eager" \
  tentaflow/vllm:latest >/dev/null

echo "[test] kontener wystartowal: $CONTAINER"

# 4. Czekaj na sidecar gotowy - parsuj endpoint_id_full z logow
echo "[test] czekam na sidecar (max ${SIDECAR_TIMEOUT}s)..."
ENDPOINT_ID=""
for i in $(seq 1 "$SIDECAR_TIMEOUT"); do
  # Strip ANSI codes z logu zanim grep
  EP=$(docker logs "$CONTAINER" 2>&1 | sed -r 's/\x1b\[[0-9;]*[A-Za-z]//g' | grep -oE 'endpoint_id_full=[0-9a-f]{64}' | head -1 | cut -d= -f2 || true)
  if [[ -n "$EP" ]]; then
    ENDPOINT_ID="$EP"
    echo "[test] sidecar gotowy po ${i}s, endpoint_id=$ENDPOINT_ID"
    break
  fi
  sleep 1
done

if [[ -z "$ENDPOINT_ID" ]]; then
  echo "[test] FAIL: sidecar nie zglocil sie w ${SIDECAR_TIMEOUT}s. Ostatnie logi:"
  docker logs --tail 60 "$CONTAINER" 2>&1
  exit 1
fi

# 5. Czekaj na vLLM gotowy (sidecar zwraca blad upstream jesli model jeszcze laduje)
echo "[test] czekam na vLLM (max ${VLLM_TIMEOUT}s, model loading może długo trwać)..."
VLLM_READY=0
for i in $(seq 1 "$VLLM_TIMEOUT"); do
  if docker logs "$CONTAINER" 2>&1 | grep -qE "Application startup complete|Uvicorn running on http://127.0.0.1:8000|Started server process"; then
    echo "[test] vLLM gotowy po ${i}s"
    VLLM_READY=1
    break
  fi
  if (( i % 10 == 0 )); then
    LAST=$(docker logs --tail 2 "$CONTAINER" 2>&1 | tr '\n' ' ' | head -c 200)
    echo "[test] (${i}s) $LAST"
  fi
  sleep 1
done

if [[ "$VLLM_READY" != "1" ]]; then
  echo "[test] FAIL: vLLM nie wstal w ${VLLM_TIMEOUT}s. Ostatnie logi:"
  docker logs --tail 60 "$CONTAINER" 2>&1
  exit 1
fi

# 6. Real test - send Completion przez iroh
CLIENT_BIN="$REPO_ROOT/tentaflow-transport/target/release/examples/iroh_test_client"
if [[ ! -x "$CLIENT_BIN" ]]; then
  echo "[test] FAIL: brak binary $CLIENT_BIN - uruchom: cd tentaflow-transport && cargo build --release --example iroh_test_client"
  exit 1
fi

echo ""
echo "[test] === REAL inference test via iroh ==="
RUST_LOG=info "$CLIENT_BIN" "$ENDPOINT_ID" "127.0.0.1:${HOST_PORT}" "$MODEL" "$PROMPT" 60
RC=$?

echo ""
echo "[test] === Sidecar logs (last 30) ==="
docker logs --tail 30 "$CONTAINER" 2>&1 | grep -E "\[sidecar\]|\[entrypoint\]" | tail -20

echo ""
if [[ "$RC" == "0" ]]; then
  echo "[test] SUCCESS"
else
  echo "[test] FAIL (exit=$RC)"
fi
exit "$RC"
