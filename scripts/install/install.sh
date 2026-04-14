#!/usr/bin/env sh
# =============================================================================
# Plik: install.sh
# Opis: Jednolinijkowy installer TentaFlow dla Linux i macOS.
# Uzycie:
#   curl -fsSL https://github.com/Slyb00ts/TentaFlow/releases/latest/download/install.sh | sh
# Opcjonalne zmienne srodowiskowe:
#   TENTAFLOW_VERSION=v0.1.0   # konkretna wersja zamiast latest
#   TENTAFLOW_PREFIX=/opt/tentaflow      # katalog instalacji (default)
#   TENTAFLOW_USER_INSTALL=1   # bez sudo, instalacja w ~/.local/share/tentaflow
#   TENTAFLOW_NO_AUTOSTART=1   # nie rejestruje systemd/launchd unit
# =============================================================================

set -eu

REPO="Slyb00ts/TentaFlow"
VERSION="${TENTAFLOW_VERSION:-latest}"
USER_INSTALL="${TENTAFLOW_USER_INSTALL:-0}"
NO_AUTOSTART="${TENTAFLOW_NO_AUTOSTART:-0}"

# ---- detekcja platformy ------------------------------------------------------
detect_target() {
  os=$(uname -s | tr '[:upper:]' '[:lower:]')
  arch=$(uname -m)
  case "$os" in
    linux)
      case "$arch" in
        x86_64|amd64)  echo "x86_64-unknown-linux-gnu" ;;
        aarch64|arm64) echo "aarch64-unknown-linux-gnu" ;;
        *) echo "unsupported_arch:$arch" ;;
      esac ;;
    darwin)
      case "$arch" in
        arm64|aarch64) echo "aarch64-apple-darwin" ;;
        x86_64)        echo "x86_64-apple-darwin" ;;
        *) echo "unsupported_arch:$arch" ;;
      esac ;;
    *) echo "unsupported_os:$os" ;;
  esac
}

TARGET=$(detect_target)
case "$TARGET" in unsupported_*) echo "TentaFlow nie wspiera $TARGET — sprobuj manualnie ze strony Releases" >&2; exit 1 ;; esac

# ---- katalogi ----------------------------------------------------------------
if [ "$USER_INSTALL" = "1" ]; then
  PREFIX="${TENTAFLOW_PREFIX:-$HOME/.local/share/tentaflow}"
  BIN_DIR="$HOME/.local/bin"
  SUDO=""
else
  PREFIX="${TENTAFLOW_PREFIX:-/opt/tentaflow}"
  BIN_DIR="/usr/local/bin"
  SUDO=$(command -v sudo >/dev/null 2>&1 && [ "$(id -u)" != "0" ] && echo sudo || echo "")
fi

mkdir -p "$PREFIX" 2>/dev/null || $SUDO mkdir -p "$PREFIX"
$SUDO mkdir -p "$BIN_DIR"

echo "==> TentaFlow installer"
echo "    target:   $TARGET"
echo "    version:  $VERSION"
echo "    prefix:   $PREFIX"
echo "    bin link: $BIN_DIR/tentaflow"

# ---- pobranie ----------------------------------------------------------------
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

if [ "$VERSION" = "latest" ]; then
  ASSET_URL="https://github.com/$REPO/releases/latest/download/tentaflow-${VERSION}-${TARGET}.tar.gz"
  # Latest URL nie ma w sobie tag'a — bierzemy go z API.
  ACTUAL_TAG=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | grep -m1 '"tag_name"' | sed 's/.*"\(v[^"]*\)".*/\1/')
  VERSION="$ACTUAL_TAG"
  ASSET_URL="https://github.com/$REPO/releases/download/$VERSION/tentaflow-${VERSION}-${TARGET}.tar.gz"
else
  ASSET_URL="https://github.com/$REPO/releases/download/$VERSION/tentaflow-${VERSION}-${TARGET}.tar.gz"
fi

echo "==> Pobieram $ASSET_URL"
curl -fL "$ASSET_URL" -o "$TMP/tentaflow.tar.gz"
curl -fL "$ASSET_URL.sha256" -o "$TMP/tentaflow.tar.gz.sha256" 2>/dev/null || true

# Weryfikacja sha256 jesli mamy plik z sumą
if [ -s "$TMP/tentaflow.tar.gz.sha256" ]; then
  echo "==> Weryfikacja SHA256"
  (cd "$TMP" && shasum -a 256 -c tentaflow.tar.gz.sha256) || \
    { echo "Suma kontrolna nie pasuje" >&2; exit 1; }
fi

echo "==> Rozpakowuje do $PREFIX"
tar -xzf "$TMP/tentaflow.tar.gz" -C "$TMP"
INNER=$(ls "$TMP" | grep -E "^tentaflow-${VERSION}-" | head -1)
$SUDO cp -r "$TMP/$INNER/." "$PREFIX/"
$SUDO ln -sf "$PREFIX/tentaflow" "$BIN_DIR/tentaflow"

if [ ! -f "$PREFIX/config.toml" ] && [ -f "$PREFIX/config.example.toml" ]; then
  $SUDO cp "$PREFIX/config.example.toml" "$PREFIX/config.toml"
fi

# ---- auto-start --------------------------------------------------------------
register_systemd() {
  UNIT="/etc/systemd/system/tentaflow.service"
  echo "==> Rejestruje systemd unit $UNIT"
  $SUDO tee "$UNIT" >/dev/null <<EOF
[Unit]
Description=TentaFlow API Gateway + mesh node
After=network.target

[Service]
Type=simple
ExecStart=$PREFIX/tentaflow --config $PREFIX/config.toml
Restart=on-failure
RestartSec=5
WorkingDirectory=$PREFIX

[Install]
WantedBy=multi-user.target
EOF
  $SUDO systemctl daemon-reload
  $SUDO systemctl enable --now tentaflow.service
  echo "==> Tentaflow startuje przez systemd. Sprawdz: systemctl status tentaflow"
}

register_launchd() {
  PLIST="$HOME/Library/LaunchAgents/ai.tentaflow.plist"
  mkdir -p "$(dirname "$PLIST")"
  echo "==> Rejestruje launchd agent $PLIST"
  cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>ai.tentaflow</string>
  <key>ProgramArguments</key>
  <array>
    <string>$PREFIX/tentaflow</string>
    <string>--config</string><string>$PREFIX/config.toml</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>$PREFIX/tentaflow.log</string>
  <key>StandardErrorPath</key><string>$PREFIX/tentaflow.err.log</string>
  <key>WorkingDirectory</key><string>$PREFIX</string>
</dict>
</plist>
EOF
  launchctl unload "$PLIST" 2>/dev/null || true
  launchctl load "$PLIST"
  echo "==> Tentaflow startuje przez launchd. Sprawdz: launchctl list | grep tentaflow"
}

if [ "$NO_AUTOSTART" = "1" ]; then
  echo "==> auto-start pominiety (TENTAFLOW_NO_AUTOSTART=1). Uruchom recznie: $BIN_DIR/tentaflow"
else
  case "$TARGET" in
    *linux*)  command -v systemctl >/dev/null 2>&1 && register_systemd || echo "Brak systemd, pomijam auto-start" ;;
    *darwin*) register_launchd ;;
  esac
fi

echo ""
echo "==> Instalacja zakonczona. Wersja: $VERSION"
echo "    binarka: $BIN_DIR/tentaflow"
echo "    prefix:  $PREFIX"
echo "    update:  tentaflow update"
echo "    deinstalacja: tentaflow uninstall  (lub manualnie skrypt uninstall.sh)"
