#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh — ds4-server (DeepSeek V4) w kontenerze, direct-http.
# Opis: pobiera wagi GGUF (download_model.sh wg MODEL), mapuje env DS4_* na
#       flagi ds4-server, bind 0.0.0.0:$PORT jako PID1.
#       Env od Core: PORT, MODEL (target download_model.sh), DS4_BACKEND,
#       DS4_CTX, DS4_SSD_STREAMING, DS4_SSD_CACHE_EXPERTS, DS4_MTP,
#       DS4_MTP_DRAFT, DS4_POWER, DS4_THREADS, HF_TOKEN.
# =============================================================================

set -uo pipefail
log() { echo "[entrypoint] $*"; }

# Override z wizarda — odpal verbatim.
if [ -n "${ENGINE_LAUNCH_CMD:-}" ]; then
  log "ENGINE_LAUNCH_CMD override"
  exec sh -c "$ENGINE_LAUNCH_CMD"
fi

PORT="${PORT:-8000}"
TARGET="${MODEL:-q2-imatrix}"
GGUF_DIR="${DS4_GGUF_DIR:-/models}"
mkdir -p "$GGUF_DIR"

# Wagi: download_model.sh linkuje /opt/ds4/ds4flash.gguf dla flash + pro-q2.
log "model target: $TARGET → $GGUF_DIR"
DS4_GGUF_DIR="$GGUF_DIR" HF_TOKEN="${HF_TOKEN:-}" \
  sh /opt/ds4/download_model.sh "$TARGET" ${HF_TOKEN:+--token "$HF_TOKEN"}

MODEL_GGUF="/opt/ds4/ds4flash.gguf"
[ -e "$MODEL_GGUF" ] || { log "FATAL: brak $MODEL_GGUF po pobraniu"; exit 1; }

# CUDA obraz → backend cuda (override DS4_BACKEND).
BACKEND="${DS4_BACKEND:-cuda}"
[ "$BACKEND" = "auto" ] && BACKEND=cuda

ARGS=(-m "$MODEL_GGUF" --host 0.0.0.0 --port "$PORT")
case "$BACKEND" in
  cuda) ARGS+=(--cuda) ;;
  rocm) ARGS+=(--rocm) ;;
  cpu)  ARGS+=(--cpu) ;;
  *)    ARGS+=(--cuda) ;;
esac

[ -n "${DS4_CTX:-}" ] && [ "${DS4_CTX}" != "0" ] && ARGS+=(--ctx "$DS4_CTX")

if [ "${DS4_SSD_STREAMING:-auto}" = "on" ]; then
  ARGS+=(--ssd-streaming)
  if [ -n "${DS4_SSD_CACHE_EXPERTS:-}" ] && [ "${DS4_SSD_CACHE_EXPERTS}" != "auto" ]; then
    ARGS+=(--ssd-streaming-cache-experts "$DS4_SSD_CACHE_EXPERTS")
  fi
fi

if [ "${DS4_MTP:-off}" = "on" ]; then
  DS4_GGUF_DIR="$GGUF_DIR" HF_TOKEN="${HF_TOKEN:-}" \
    sh /opt/ds4/download_model.sh mtp ${HF_TOKEN:+--token "$HF_TOKEN"}
  MTP_FILE="$(ls -1 "$GGUF_DIR"/*MTP*.gguf 2>/dev/null | head -1 || true)"
  if [ -n "$MTP_FILE" ]; then
    ARGS+=(--mtp "$MTP_FILE" --mtp-draft "${DS4_MTP_DRAFT:-2}")
  else
    log "WARN: DS4_MTP=on ale brak pliku MTP — pomijam"
  fi
fi

[ -n "${DS4_POWER:-}" ] && [ "${DS4_POWER}" != "0" ] && ARGS+=(--power "$DS4_POWER")
[ -n "${DS4_THREADS:-}" ] && [ "${DS4_THREADS}" != "0" ] && ARGS+=(--threads "$DS4_THREADS")

log "exec ds4-server ${ARGS[*]}"
exec /opt/ds4/ds4-server "${ARGS[@]}"
