#!/usr/bin/env sh
# =============================================================================
# File:        uninstall.sh
# Description: Removes a TentaFlow installation from Linux or macOS.
# =============================================================================

set -eu

PREFIX="${TENTAFLOW_PREFIX:-/opt/tentaflow}"
[ ! -d "$PREFIX" ] && [ -d "$HOME/.local/share/tentaflow" ] && PREFIX="$HOME/.local/share/tentaflow"
SUDO=$(command -v sudo >/dev/null 2>&1 && [ "$(id -u)" != "0" ] && echo sudo || echo "")

echo "==> Uninstalling TentaFlow from $PREFIX"

case "$(uname -s)" in
  Linux*)
    if [ -f /etc/systemd/system/tentaflow.service ]; then
      $SUDO systemctl disable --now tentaflow.service 2>/dev/null || true
      $SUDO rm -f /etc/systemd/system/tentaflow.service
      $SUDO systemctl daemon-reload
    fi
    ;;
  Darwin*)
    PLIST="$HOME/Library/LaunchAgents/ai.tentaflow.plist"
    [ -f "$PLIST" ] && launchctl unload "$PLIST" 2>/dev/null && rm -f "$PLIST"
    ;;
esac

$SUDO rm -f /usr/local/bin/tentaflow "$HOME/.local/bin/tentaflow"
$SUDO rm -rf "$PREFIX"

echo "==> Per-user cache (downloaded Python, venvs, models) is NOT removed."
echo "    Remove manually if desired: rm -rf ~/.cache/tentaflow"
echo "==> Done."
