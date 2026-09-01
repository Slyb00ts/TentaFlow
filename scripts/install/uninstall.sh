#!/bin/sh
# =============================================================================
# File:        uninstall.sh
# Description: Removes a TentaFlow installation made by install.sh.
#
# Data is kept by default: /var/lib/tentaflow holds the database, the vector
# store and the per-installation TLS identity, and an uninstall is not a request
# to destroy them. `--purge` removes them, and says what it is about to delete.
#
# Usage: uninstall.sh [--purge]
# =============================================================================
set -eu

PURGE=0
for arg in "$@"; do
  case "$arg" in
    --purge) PURGE=1 ;;
    *) echo "nieznany argument: $arg (uzycie: uninstall.sh [--purge])" >&2; exit 2 ;;
  esac
done

if [ "$(id -u)" -eq 0 ]; then SUDO=""; else SUDO="sudo"; fi

# The receipt says what was installed and where, so an uninstall does not guess.
RECEIPT=""
for candidate in /etc/tentaflow/install-receipt.json \
                 "$HOME/.config/tentaflow/install-receipt.json"; do
  [ -f "$candidate" ] && { RECEIPT="$candidate"; break; }
done

read_field() { sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p" "$RECEIPT" | head -1; }

if [ -n "$RECEIPT" ]; then
  PREFIX=$(read_field prefix)
  CONFIG_DIR=$(dirname "$(read_field config)")
  DATA_DIR=$(read_field home)
  SCOPE=$(read_field service_scope)
elif [ "$(uname -s)" = "Darwin" ]; then
  echo "Brak receiptu — zakladam domyslny layout macOS." >&2
  PREFIX=/usr/local/tentaflow
  CONFIG_DIR=/usr/local/etc/tentaflow
  DATA_DIR=/usr/local/var/tentaflow
  SCOPE=system
else
  echo "Brak receiptu — zakladam domyslny layout." >&2
  PREFIX=/opt/tentaflow
  CONFIG_DIR=/etc/tentaflow
  DATA_DIR=/var/lib/tentaflow
  SCOPE=system
fi

echo "Usuwam TentaFlow:"
echo "  prefix: $PREFIX"
echo "  config: $CONFIG_DIR"
echo "  dane:   $DATA_DIR $([ "$PURGE" = "1" ] && echo '(ZOSTANA USUNIETE)' || echo '(zostaja)')"

if [ "$(uname -s)" = "Darwin" ]; then
  # Booting the service out before deleting its plist; the other order leaves a
  # loaded job pointing at files that no longer exist.
  if [ "$SCOPE" = "user" ]; then
    launchctl bootout "gui/$(id -u)/ai.tentaflow" 2>/dev/null || true
    rm -f "$HOME/Library/LaunchAgents/ai.tentaflow.plist"
  else
    $SUDO launchctl bootout system/ai.tentaflow 2>/dev/null || true
    $SUDO rm -f /Library/LaunchDaemons/ai.tentaflow.plist
  fi
elif command -v systemctl >/dev/null 2>&1; then
  if [ "$SCOPE" = "user" ]; then
    systemctl --user disable --now tentaflow.service 2>/dev/null || true
    rm -f "$HOME/.config/systemd/user/tentaflow.service"
    systemctl --user daemon-reload 2>/dev/null || true
  else
    $SUDO systemctl disable --now tentaflow.service 2>/dev/null || true
    $SUDO rm -f /etc/systemd/system/tentaflow.service
    $SUDO systemctl daemon-reload 2>/dev/null || true
  fi
fi

for bindir in /usr/local/bin "$HOME/.local/bin"; do
  [ -L "$bindir/tentaflow" ] && $SUDO rm -f "$bindir/tentaflow"
done

$SUDO rm -rf "$PREFIX"

if [ "$PURGE" = "1" ]; then
  $SUDO rm -rf "$DATA_DIR" "$CONFIG_DIR"
  echo "Usunieto rowniez dane i konfiguracje."
else
  echo "Dane i konfiguracja zostaly: $DATA_DIR, $CONFIG_DIR"
  echo "Aby usunac wszystko: uninstall.sh --purge"
fi
