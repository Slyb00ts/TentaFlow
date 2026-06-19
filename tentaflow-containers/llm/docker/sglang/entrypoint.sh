#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: sglang (direct-http, bez sidecara). Core gada HTTP wprost do
#       host-mapped portu. sglang bind 0.0.0.0:$PORT i biegnie jako PID1.
# =============================================================================

set -uo pipefail

MODEL="${MODEL:?MODEL env required, np. 'Qwen/Qwen2.5-0.5B-Instruct'}"
SGLANG_PORT="${PORT:-${SGLANG_PORT:-30000}}"

# Argi silnika jako "$@" (bollard Cmd array z Rust). JSON-owe argi (np.
# speculative) zostaja nietkniete — zadnej re-tokenizacji. Gdy Rust nie podal
# nic, uzywamy bezpiecznego baseline single-GPU.
ENGINE_ARGS=("$@")
if [[ "${#ENGINE_ARGS[@]}" -eq 0 ]]; then
  ENGINE_ARGS=(--tp 1 --mem-fraction-static 0.85)
fi

echo "[entrypoint] start sglang na 0.0.0.0:$SGLANG_PORT (${#ENGINE_ARGS[@]} args)"
exec python3 -m sglang.launch_server \
  --model-path "$MODEL" \
  --host 0.0.0.0 \
  --port "$SGLANG_PORT" \
  "${ENGINE_ARGS[@]}"
