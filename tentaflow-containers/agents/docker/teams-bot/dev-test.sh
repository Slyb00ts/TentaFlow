#!/usr/bin/env bash
# =============================================================================
# Plik: dev-test.sh
# Opis: Szybka pętla iteracji dla pipelinu video bota: zbuduj kontener,
#       wpuść go do testowego meetinga, dumpaj relevantne logi, zrzuć
#       screenshot z VNC. Bez routera, bez całego tentaflow-core stack.
# Użycie: ./dev-test.sh "<MEETING_URL>" [--seconds=30]
# =============================================================================
set -u

URL="${1:-}"
SECS="${2:-30}"
if [ -z "$URL" ]; then
  echo "Usage: $0 <MEETING_URL> [seconds]" >&2
  exit 2
fi
SECS="${SECS#--seconds=}"

cd "$(dirname "$0")"

echo "==> [1/5] cargo build --release"
cargo build --release 2>&1 | tail -3 || { echo "cargo build failed"; exit 1; }

echo "==> [2/5] docker build (context = repo root, dockerfile = teams-bot/Dockerfile)"
cd ../../../..
docker build -q \
  -t tentaflow/teams-bot:dev \
  -f tentaflow-containers/agents/docker/teams-bot/Dockerfile \
  . | tail -1 || { echo "docker build failed"; exit 1; }

echo "==> [3/5] kill previous bot-dev"
docker rm -f bot-dev 2>/dev/null || true

echo "==> [4/5] run bot-dev with meeting url"
MEETING_ID="dev-$(date +%s)"
docker run -d --name bot-dev \
  -p 5999:5900 \
  -p 6079:6080 \
  -e MEETING_URL="$URL" \
  -e MEETING_ID="$MEETING_ID" \
  -e BOT_NAME="DevBot" \
  -e BOT_VIDEO_ENABLED=true \
  -e RUST_LOG="info,chromiumoxide=error" \
  -e SKIP_ROUTER_WAIT=1 \
  tentaflow/teams-bot:dev >/dev/null

echo "    container running, waiting ${SECS}s..."
sleep "$SECS"

echo "==> [5/5] collect logs + VNC snapshot"
LOG_OUT="/tmp/bot-dev-$MEETING_ID.log"
SHOT_OUT="/tmp/bot-dev-$MEETING_ID.png"
docker logs bot-dev 2>&1 \
  | grep -vE 'chromiumoxide::handler|WS Invalid message|Capture PCM|Wyslano chunk|websockify|FramebufferUpdate' \
  > "$LOG_OUT" || true

# VNC snapshot via gvnccapture (no auth, port 5999 → display :99 inside)
gvnccapture localhost:99 "$SHOT_OUT" 2>/dev/null \
  || gvnccapture 127.0.0.1:99 "$SHOT_OUT" 2>/dev/null \
  || echo "    (gvnccapture failed, screenshot skipped)"

echo
echo "===== KLUCZOWE LINIE Z LOGÓW ====="
grep -E 'Video injection|track ready|track became|track ENDED|Przechwycono getUserMedia|setPermission|centerRGBA|Pomyslnie dolaczono|Bot w lobby|video frame error|Active device|emit_lifecycle|Post-join camera toggle' "$LOG_OUT" \
  | sed 's/.*Z\s*//' | sed 's/\[3m//g; s/\[2m//g; s/\[0m//g; s/\[32m//g; s/\[33m//g; s/\[31m//g' \
  | head -40

echo
echo "===== ARTYFAKTY ====="
echo "Pełne logi: $LOG_OUT"
echo "Screenshot: $SHOT_OUT"
ls -lh "$SHOT_OUT" 2>/dev/null

echo
echo "(zostawiam kontener bot-dev działający — kill: docker rm -f bot-dev)"
