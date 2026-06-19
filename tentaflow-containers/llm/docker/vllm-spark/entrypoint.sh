#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh (vllm-spark)
# Opis: vLLM OpenAI API (direct-http, bez sidecara). Core gada HTTP wprost do
#       host-mapped portu. DGX Spark env baseline dla GB10/SM121 i stabilnego
#       startu bez FlashInfer autotune.
# =============================================================================

set -uo pipefail

# Spark-specific runtime env. Te same wartosci sa w bundle.toml [launch.env]
# dla deploy.native — duplikujemy tu zeby docker dzialal niezaleznie od bundla.
export TORCH_CUDA_ARCH_LIST="${TORCH_CUDA_ARCH_LIST:-12.1a}"
export VLLM_USE_FLASHINFER_MXFP4_MOE="${VLLM_USE_FLASHINFER_MXFP4_MOE:-1}"
export VLLM_SKIP_P2P_CHECK="${VLLM_SKIP_P2P_CHECK:-1}"
export TRITON_PTXAS_PATH="${TRITON_PTXAS_PATH:-/usr/local/cuda/bin/ptxas}"

MODEL="${MODEL:?MODEL env required, np. 'Qwen/Qwen3.5-0.8B'}"
VLLM_PORT="${VLLM_PORT:-8000}"

# Argi silnika jako "$@" (bollard Cmd z Rust). JSON --speculative-config
# nietkniety, bez re-tokenizacji. Defaulty (--no-enable-flashinfer-autotune,
# gpu-memory, --enforce-eager) dodaje Rust docker.rs.
ENGINE_ARGS=("$@")

# DGX Spark to single-GPU SoC (jeden GB10) — TP=1 default. Dorzucamy tylko gdy
# user nie podal TP/PP. Multi-Spark mesh nie idzie przez jeden kontener.
HAS_PARALLEL=0
for _a in "${ENGINE_ARGS[@]}"; do
  case "$_a" in
    --tensor-parallel-size|--tensor-parallel-size=*|-tp|--pipeline-parallel-size|--pipeline-parallel-size=*) HAS_PARALLEL=1 ;;
  esac
done
if [[ "$HAS_PARALLEL" -eq 0 ]]; then
  GPU_COUNT=$(nvidia-smi -L 2>/dev/null | wc -l || echo 1)
  [[ "$GPU_COUNT" -lt 1 ]] && GPU_COUNT=1
  echo "[entrypoint] DGX Spark vllm — GPU widocznych: $GPU_COUNT"
  ENGINE_ARGS+=(--tensor-parallel-size "$GPU_COUNT")
fi

# Bind 0.0.0.0 WEWNATRZ kontenera: ruch z docker-publish (host 127.0.0.1:host_http)
# trafia na interfejs kontenera, nie na jego loopback. Containment robi host bind.
echo "[entrypoint] vllm serve $MODEL na 0.0.0.0:$VLLM_PORT (sm_121a, ${#ENGINE_ARGS[@]} args)"
exec vllm serve "$MODEL" \
  --host 0.0.0.0 \
  --port "$VLLM_PORT" \
  --served-model-name "${SERVED_MODEL_NAME:-$MODEL}" \
  "${ENGINE_ARGS[@]}"
