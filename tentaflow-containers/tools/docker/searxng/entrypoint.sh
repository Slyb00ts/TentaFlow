#!/bin/sh
# =============================================================================
# Plik: entrypoint.sh
# Opis: Startuje SearXNG z runtime sekretem, gdy deploy nie podal SEARXNG_SECRET.
# Przykład: docker run -p 8080:8080 tentaflow-searxng
# =============================================================================

set -eu

if [ -z "${SEARXNG_SECRET:-}" ]; then
  SEARXNG_SECRET="$(python -c 'import secrets; print(secrets.token_hex(32))')"
  export SEARXNG_SECRET
fi

exec /usr/local/searxng/entrypoint.sh "$@"
