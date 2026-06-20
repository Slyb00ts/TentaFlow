#!/usr/bin/env bash
# =============================================================================
# Plik: entrypoint.sh
# Opis: ComfyUI (direct-http, bez sidecara). Core gada HTTP wprost do
#       host-mapped portu. Bind 0.0.0.0 WEWNATRZ kontenera: ruch z
#       docker-publish trafia na interfejs kontenera, nie na jego loopback.
# =============================================================================

set -uo pipefail

# Core wstrzykuje PORT = wewnetrzny port kontenera, na ktory mapuje host-port.
# ComfyUI MUSI bindowac wlasnie ten port (nie domyslne 8188), inaczej host-mapping
# trafia w pustke i readiness/health probe sie nie laczy.
PORT="${PORT:-${COMFY_PORT:-8188}}"
cd /opt/ComfyUI

echo "[entrypoint] start comfy na 0.0.0.0:$PORT"
exec python3 main.py --listen 0.0.0.0 --port "$PORT"
