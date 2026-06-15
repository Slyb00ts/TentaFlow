#!/usr/bin/env bash
# =============================================================================
# File: tools/rtsp-sim/simulate.sh
# Purpose: Fan out N looping RTSP camera streams from a folder of video files,
#          so the TentaFlow camera pipeline can be load-tested at fleet scale
#          without physical cameras.
#
# Each simulated camera = one ffmpeg process looping a source file into
# rtsp://<host>:8554/<prefix><NN>. With H.264 sources ffmpeg uses `-c copy`
# (stream copy, ~0% CPU per camera) so a single box can host many streams.
# Non-H.264 sources are transcoded (set --encode to force).
#
# Usage:
#   ./simulate.sh --videos ./clips --count 50
#   ./simulate.sh --videos ./clips --count 200 --prefix cam --host 127.0.0.1
#   ./simulate.sh --videos ./clips --count 16 --encode libx264 --fps 25
#
# Flags:
#   --videos DIR    folder with source video files (mp4/mkv/mov/...). Required.
#   --count N       number of simulated cameras (default 10)
#   --prefix NAME   RTSP path prefix (default "cam") → /cam01, /cam02, ...
#   --host HOST     MediaMTX host (default 127.0.0.1)
#   --port PORT     RTSP port (default 8554)
#   --encode CODEC  force transcode (e.g. libx264). Default: copy if H.264.
#   --fps N         output fps when transcoding (default: source fps)
#   --no-server     don't start MediaMTX (assume it is already running)
#
# Ctrl-C stops every publisher (and MediaMTX unless --no-server).
# =============================================================================
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

VIDEOS=""
COUNT=10
PREFIX="cam"
HOST="127.0.0.1"
PORT="8554"
ENCODE=""
OUT_FPS=""
START_SERVER=1

while [[ $# -gt 0 ]]; do
  case "$1" in
    --videos)    VIDEOS="$2"; shift 2 ;;
    --count)     COUNT="$2"; shift 2 ;;
    --prefix)    PREFIX="$2"; shift 2 ;;
    --host)      HOST="$2"; shift 2 ;;
    --port)      PORT="$2"; shift 2 ;;
    --encode)    ENCODE="$2"; shift 2 ;;
    --fps)       OUT_FPS="$2"; shift 2 ;;
    --no-server) START_SERVER=0; shift ;;
    -h|--help)   sed -n '2,40p' "$0"; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 1 ;;
  esac
done

command -v ffmpeg >/dev/null || { echo "ffmpeg not found" >&2; exit 1; }
command -v ffprobe >/dev/null || { echo "ffprobe not found" >&2; exit 1; }

if [[ -z "$VIDEOS" || ! -d "$VIDEOS" ]]; then
  echo "error: --videos must point to an existing folder" >&2
  exit 1
fi

# Collect source files.
mapfile -t FILES < <(find "$VIDEOS" -maxdepth 1 -type f \
  \( -iname '*.mp4' -o -iname '*.mkv' -o -iname '*.mov' -o -iname '*.avi' -o -iname '*.ts' -o -iname '*.webm' \) | sort)
if [[ ${#FILES[@]} -eq 0 ]]; then
  echo "error: no video files in $VIDEOS" >&2
  exit 1
fi
echo "found ${#FILES[@]} source file(s); simulating $COUNT camera(s)."

PIDS=()
cleanup() {
  echo
  echo "stopping ${#PIDS[@]} publisher(s)..."
  for pid in "${PIDS[@]}"; do kill "$pid" 2>/dev/null || true; done
  wait 2>/dev/null || true
  if [[ "$START_SERVER" -eq 1 ]]; then
    echo "stopping MediaMTX..."
    docker compose -f "$SCRIPT_DIR/docker-compose.yml" down 2>/dev/null || true
  fi
}
trap cleanup INT TERM EXIT

if [[ "$START_SERVER" -eq 1 ]]; then
  command -v docker >/dev/null || { echo "docker not found (use --no-server)" >&2; exit 1; }
  echo "starting MediaMTX (RTSP :$PORT)..."
  docker compose -f "$SCRIPT_DIR/docker-compose.yml" up -d
  # Wait for the RTSP port to accept connections.
  for _ in $(seq 1 30); do
    if (exec 3<>"/dev/tcp/$HOST/$PORT") 2>/dev/null; then exec 3>&- 3<&-; break; fi
    sleep 0.5
  done
fi

# Returns 0 if the file's video stream is already H.264 (stream-copyable).
is_h264() {
  local codec
  codec="$(ffprobe -v error -select_streams v:0 -show_entries stream=codec_name \
    -of default=nw=1:nk=1 "$1" 2>/dev/null || true)"
  [[ "$codec" == "h264" ]]
}

LOG_DIR="$(mktemp -d /tmp/rtsp-sim.XXXXXX)"
echo "publisher logs in $LOG_DIR"

for i in $(seq 1 "$COUNT"); do
  idx=$(( (i - 1) % ${#FILES[@]} ))
  src="${FILES[$idx]}"
  cam=$(printf "%s%02d" "$PREFIX" "$i")
  url="rtsp://$HOST:$PORT/$cam"

  args=(-hide_banner -loglevel warning -re -stream_loop -1 -i "$src")
  if [[ -n "$ENCODE" ]]; then
    args+=(-an -c:v "$ENCODE" -preset veryfast -tune zerolatency -pix_fmt yuv420p)
    [[ -n "$OUT_FPS" ]] && args+=(-r "$OUT_FPS")
  elif is_h264 "$src"; then
    args+=(-an -c:v copy)
  else
    args+=(-an -c:v libx264 -preset veryfast -tune zerolatency -pix_fmt yuv420p)
    [[ -n "$OUT_FPS" ]] && args+=(-r "$OUT_FPS")
  fi
  args+=(-rtsp_transport tcp -f rtsp "$url")

  ffmpeg "${args[@]}" >"$LOG_DIR/$cam.log" 2>&1 &
  PIDS+=($!)
  echo "  $url  <-  $(basename "$src")"
done

echo
echo "$COUNT camera(s) live. Add these RTSP URLs in TentaVision."
echo "Press Ctrl-C to stop."
wait
