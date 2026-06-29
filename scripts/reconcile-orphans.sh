#!/usr/bin/env bash
# =============================================================================
# Plik: reconcile-orphans.sh
# Opis: Pre-start sweep osieroconych silnikow uslug. TentaFlow odpina natywne
#       silniki (setsid) i kontenery docker (brak restart_policy/auto_remove),
#       a sprzatanie biegnie WYLACZNIE na gracefulnym SIGINT/SIGTERM. Po `kill -9`,
#       crashu, OOM-killerze czy braku pradu silniki PRZEZYWAJA: trzymaja porty i
#       VRAM, przez co kolejny start daje "port zajety -> Failed" i odpala to samo
#       po kilka razy (duplikaty). Ten skrypt, uruchomiony GDY TENTAFLOW NIE DZIALA,
#       usuwa wszystkie osierocone procesy/kontenery — wtedy nic nie jest legalnie
#       sledzone, wiec jest bezpieczny. Pomyslany jako ExecStartPre albo recznie
#       przed startem binarki.
# Przyklad: ./scripts/reconcile-orphans.sh            # sprzata
#           ./scripts/reconcile-orphans.sh --dry-run  # tylko pokazuje
# =============================================================================

set -uo pipefail

DRY_RUN=0
[ "${1:-}" = "--dry-run" ] && DRY_RUN=1

# Port HTTPS/QUIC Core (domyslny 8090). Override przez TENTAFLOW_PORT.
TF_PORT="${TENTAFLOW_PORT:-8090}"

log() { printf '[reconcile] %s\n' "$*"; }
run() {
  if [ "$DRY_RUN" = "1" ]; then
    log "DRY: $*"
  else
    eval "$@"
  fi
}

# --- Bezpiecznik: nie sprzataj pod dzialajacym Core ---
# Gdy ktos LISTENuje na porcie Core, ubicie silnikow zabilo by zywe uslugi i
# zostawilo Core z martwymi endpointami. Sprzatamy tylko przy WYLACZONYM Core.
if ss -tln 2>/dev/null | grep -qE "[:.]${TF_PORT}\b"; then
  log "TentaFlow nasluchuje na :${TF_PORT} — Core dziala. ODMAWIAM sprzatania."
  log "Najpierw zatrzymaj Core (SIGINT/SIGTERM), potem uruchom ten skrypt."
  exit 1
fi

# --- 1. Natywne silniki (python-bundle / binary) ---
# Procesy silnikow uruchomione Z instancji bundla. Zakotwiczamy wzorzec na realnej
# sciezce runtime + faktycznym entrypoincie (bin/python|bin/uvicorn|app/server.py),
# zeby NIE zlapac procesu ktory jedynie WSPOMINA ten katalog (np. cwd, arg). Po
# wylaczeniu Core kazdy taki proces jest osierocony (PPID=1). Grupowy kill (lider
# sesji po setsid — lapie tez workery, np. resource_tracker vLLM) + per-PID.
mapfile -t NATIVE_PIDS < <(
  pgrep -f 'cache/bundle-instances/[^ ]+/(bin/python|bin/uvicorn|app/server\.py)' 2>/dev/null || true
)
if [ "${#NATIVE_PIDS[@]}" -gt 0 ]; then
  log "osierocone natywne silniki: ${NATIVE_PIDS[*]}"
  for pid in "${NATIVE_PIDS[@]}"; do
    run "kill -TERM -- -${pid} 2>/dev/null; kill -TERM ${pid} 2>/dev/null || true"
  done
  [ "$DRY_RUN" = "0" ] && sleep 3
  for pid in "${NATIVE_PIDS[@]}"; do
    if [ "$DRY_RUN" = "0" ] && kill -0 "$pid" 2>/dev/null; then
      log "  ${pid} nie zareagowal na TERM -> KILL"
      kill -KILL -- -"${pid}" 2>/dev/null || true
      kill -KILL "${pid}" 2>/dev/null || true
    fi
  done
else
  log "brak osieroconych natywnych silnikow"
fi

# --- 2. Kontenery docker silnikow ---
# Deterministyczna nazwa deployu to `tentaflow-<engine>-<host_port>` (patrz
# DockerDeploy::run). Czyscimy WYLACZNIE ten wzorzec (konczy sie -<port>), zeby
# NIE ruszyc infra typu `tentaflow-shim` (nie ma sufiksu portu). Kontenery nie
# maja restart_policy ani auto_remove, wiec przezywaja kazde wyjscie Core.
if command -v docker >/dev/null 2>&1; then
  mapfile -t TF_CONTAINERS < <(
    docker ps -a --format '{{.Names}}' 2>/dev/null | grep -E '^tentaflow-.+-[0-9]+$' || true
  )
  if [ "${#TF_CONTAINERS[@]}" -gt 0 ]; then
    log "osierocone kontenery silnikow: ${TF_CONTAINERS[*]}"
    for name in "${TF_CONTAINERS[@]}"; do
      run "docker rm -f '${name}' >/dev/null 2>&1 || true"
    done
  else
    log "brak osieroconych kontenerow silnikow"
  fi
else
  log "docker niedostepny — pomijam kontenery"
fi

log "gotowe."
