#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: ComfyUI (direct-http, bez sidecara). Core gada HTTP wprost do
#       host-mapped portu. Bind 0.0.0.0 WEWNATRZ kontenera: ruch z
#       docker-publish trafia na interfejs kontenera, nie na jego loopback.
# =============================================================================

set -uo pipefail

PORT="${COMFY_PORT:-8188}"
cd /opt/ComfyUI

echo "[entrypoint] start comfy na 0.0.0.0:$PORT"
exec python3 main.py --listen 0.0.0.0 --port "$PORT"
