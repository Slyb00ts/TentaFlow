#!/usr/bin/env bash
# =============================================================================
# File: scripts/ci-local/test-install.sh
# Purpose: Run install.sh against a locally built archive inside a container
#          that actually has systemd, so "starts with the system" is verified
#          rather than assumed. Without systemd the installer's most important
#          step — registering and enabling the unit — cannot be tested at all.
#
# Usage: scripts/ci-local/test-install.sh <archive.tar.gz> [image]
# =============================================================================
set -euo pipefail

ARCHIVE="${1:?podaj sciezke do archiwum .tar.gz}"
IMAGE="${2:-ubuntu:22.04}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
NAME="tentaflow-install-test"

docker rm -f "$NAME" >/dev/null 2>&1 || true

# systemd as PID 1 needs the cgroup filesystem and a private tmpfs for /run.
docker run -d --name "$NAME" \
  --privileged --cgroupns=host \
  -v /sys/fs/cgroup:/sys/fs/cgroup:rw \
  --tmpfs /run --tmpfs /run/lock \
  -v "$ARCHIVE:/tmp/tentaflow.tar.gz:ro" \
  -v "$ARCHIVE.sha256:/tmp/tentaflow.tar.gz.sha256:ro" \
  -v "$REPO_ROOT/scripts/install:/installer:ro" \
  -e container=docker \
  "$IMAGE" \
  bash -c 'apt-get update -qq && apt-get install -y -qq systemd systemd-sysv curl ca-certificates >/dev/null && exec /lib/systemd/systemd' \
  >/dev/null

echo "[test-install] czekam az systemd wstanie"
for _ in $(seq 1 60); do
  if docker exec "$NAME" systemctl is-system-running 2>/dev/null | grep -qE 'running|degraded'; then break; fi
  sleep 2
done
docker exec "$NAME" systemctl is-system-running || true

echo "[test-install] instalacja"
docker exec -e TENTAFLOW_EDITION=full \
            -e TENTAFLOW_ASSET_FILE=/tmp/tentaflow.tar.gz \
            "$NAME" sh /installer/install.sh

echo "[test-install] weryfikacja"
docker exec "$NAME" bash -c '
  set -e
  echo "--- autostart (to jest dowod na \"wstaje z systemem\")"
  systemctl is-enabled tentaflow.service
  echo "--- stan"
  systemctl is-active tentaflow.service || true
  echo "--- layout"
  ls -l /opt/tentaflow/current
  test -f /etc/tentaflow/config.toml && echo "config ok"
  test -f /etc/tentaflow/install-receipt.json && echo "receipt ok"
  echo "--- tentaflow status"
  tentaflow status || true
'
echo "[test-install] kontener zostaje jako $NAME (docker rm -f $NAME aby usunac)"
