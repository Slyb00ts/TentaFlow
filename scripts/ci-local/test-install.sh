#!/usr/bin/env bash
# =============================================================================
# File: scripts/ci-local/test-install.sh
# Purpose: Run install.sh against a locally built archive inside a container
#          that actually has systemd, so "starts with the system" is verified
#          rather than assumed. Without systemd the installer's most important
#          step — registering and enabling the unit — cannot be tested at all.
#
#          The image is a parameter because the installer's distro handling
#          (package manager, GStreamer plugin names, SELinux, firewalld, the
#          glibc floor) differs per family and none of it is exercised by a
#          single Ubuntu run.
#
# Usage: scripts/ci-local/test-install.sh <archive.tar.gz> [image|all]
#        image: ubuntu:22.04 (default) | debian:12 | fedora:41 | archlinux
# =============================================================================
set -euo pipefail

ARCHIVE="${1:?podaj sciezke do archiwum .tar.gz}"
IMAGE="${2:-ubuntu:22.04}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

MATRIX="ubuntu:22.04 debian:12 fedora:41 archlinux"
if [ "$IMAGE" = "all" ]; then
  rc=0
  for img in $MATRIX; do
    echo ""
    echo "############ $img ############"
    "$0" "$ARCHIVE" "$img" || { rc=1; echo "[test-install] NIEPOWODZENIE: $img"; }
  done
  exit "$rc"
fi

NAME="tentaflow-install-test-$(echo "$IMAGE" | tr ':/' '--')"

# Each family bootstraps systemd differently, and a container image ships none
# of the tooling install.sh assumes a real machine has.
case "$IMAGE" in
  ubuntu*|debian*)
    BOOTSTRAP='export DEBIAN_FRONTEND=noninteractive; apt-get update -qq && apt-get install -y -qq systemd systemd-sysv curl ca-certificates tar >/dev/null' ;;
  fedora*|rockylinux*|almalinux*)
    BOOTSTRAP='dnf install -y -q systemd procps-ng curl tar >/dev/null' ;;
  archlinux*)
    BOOTSTRAP='pacman -Sy --noconfirm --needed --quiet systemd curl tar >/dev/null' ;;
  *)
    echo "nieznany obraz $IMAGE — dodaj bootstrap w test-install.sh" >&2; exit 1 ;;
esac

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
  bash -c "$BOOTSTRAP && exec /usr/lib/systemd/systemd" \
  >/dev/null

echo "[test-install] $IMAGE: czekam az systemd wstanie"
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
