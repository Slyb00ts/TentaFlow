#!/usr/bin/env sh
# =============================================================================
# File:        install.sh
# Description: Zero-touch installer for TentaFlow on Linux and macOS.
#              Checks + installs all runtime dependencies:
#                - Docker Engine + Buildx (BuildKit)
#                - Python 3.10+ with venv + pip (for Python-bundle engines)
#                - curl, tar, systemd/launchd
#              Detects package manager (apt / dnf / pacman / zypper / brew)
#              and installs missing pieces non-interactively. User can skip
#              individual checks with env flags if they know what they're doing.
#
# Usage:
#   curl -fsSL https://github.com/Slyb00ts/TentaFlow/releases/latest/download/install.sh | sh
#
# Environment overrides:
#   TENTAFLOW_VERSION=v0.1.0         # install a specific version instead of latest
#   TENTAFLOW_PREFIX=/opt/tentaflow  # install prefix (default shown)
#   TENTAFLOW_USER_INSTALL=1         # no sudo, install under ~/.local/share/tentaflow
#   TENTAFLOW_NO_AUTOSTART=1         # skip systemd/launchd registration
#   TENTAFLOW_SKIP_DEPS=1            # skip all dep installation (you're on your own)
#   TENTAFLOW_SKIP_DOCKER=1          # skip Docker install check
#   TENTAFLOW_SKIP_PYTHON=1          # skip Python install check
#   TENTAFLOW_NO_GROUP=1             # skip adding user to docker group (Linux)
# =============================================================================

set -eu

REPO="Slyb00ts/TentaFlow"
VERSION="${TENTAFLOW_VERSION:-latest}"
USER_INSTALL="${TENTAFLOW_USER_INSTALL:-0}"
NO_AUTOSTART="${TENTAFLOW_NO_AUTOSTART:-0}"
SKIP_DEPS="${TENTAFLOW_SKIP_DEPS:-0}"
SKIP_DOCKER="${TENTAFLOW_SKIP_DOCKER:-0}"
SKIP_PYTHON="${TENTAFLOW_SKIP_PYTHON:-0}"
NO_GROUP="${TENTAFLOW_NO_GROUP:-0}"

# ---- Colors (opt-out if not a TTY) ----
if [ -t 1 ] && [ "${NO_COLOR:-0}" = "0" ]; then
  C_BOLD="$(printf '\033[1m')"
  C_DIM="$(printf '\033[2m')"
  C_RED="$(printf '\033[0;31m')"
  C_GREEN="$(printf '\033[0;32m')"
  C_YELLOW="$(printf '\033[0;33m')"
  C_BLUE="$(printf '\033[0;34m')"
  C_RESET="$(printf '\033[0m')"
else
  C_BOLD=""; C_DIM=""; C_RED=""; C_GREEN=""; C_YELLOW=""; C_BLUE=""; C_RESET=""
fi

log()  { printf "%s==>%s %s\n" "$C_BLUE" "$C_RESET" "$*"; }
ok()   { printf "%s ✓%s %s\n" "$C_GREEN" "$C_RESET" "$*"; }
warn() { printf "%s ⚠%s %s\n" "$C_YELLOW" "$C_RESET" "$*" >&2; }
err()  { printf "%s ✗%s %s\n" "$C_RED" "$C_RESET" "$*" >&2; }

# ---- Platform detection ----
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

detect_pm() {
  case "$(uname -s)" in
    Darwin) echo "brew"; return ;;
    Linux)
      if command -v apt-get >/dev/null 2>&1; then echo "apt";
      elif command -v dnf >/dev/null 2>&1;      then echo "dnf";
      elif command -v pacman >/dev/null 2>&1;   then echo "pacman";
      elif command -v zypper >/dev/null 2>&1;   then echo "zypper";
      else echo "unknown"; fi ;;
    *) echo "unknown" ;;
  esac
}

TARGET=$(detect_target)
case "$TARGET" in unsupported_*)
  err "TentaFlow does not support $TARGET — install manually from the Releases page."
  exit 1 ;;
esac

PM=$(detect_pm)

if [ "$USER_INSTALL" = "1" ]; then
  PREFIX="${TENTAFLOW_PREFIX:-$HOME/.local/share/tentaflow}"
  BIN_DIR="$HOME/.local/bin"
  SUDO=""
else
  PREFIX="${TENTAFLOW_PREFIX:-/opt/tentaflow}"
  BIN_DIR="/usr/local/bin"
  SUDO=$(command -v sudo >/dev/null 2>&1 && [ "$(id -u)" != "0" ] && echo sudo || echo "")
fi

# =============================================================================
# Dependency management
# =============================================================================

pm_install() {
  # Install a list of packages non-interactively with the detected PM.
  pkgs="$*"
  case "$PM" in
    apt)
      $SUDO DEBIAN_FRONTEND=noninteractive apt-get update -qq
      $SUDO DEBIAN_FRONTEND=noninteractive apt-get install -y -qq $pkgs ;;
    dnf)     $SUDO dnf install -y --quiet $pkgs ;;
    pacman)  $SUDO pacman -Sy --noconfirm --needed $pkgs ;;
    zypper)  $SUDO zypper --non-interactive install $pkgs ;;
    brew)    brew install $pkgs ;;
    *)       warn "Unknown package manager — install $pkgs manually"; return 1 ;;
  esac
}

# ---- curl + tar (required to even fetch tentaflow) ----
check_base_tools() {
  missing=""
  command -v curl >/dev/null 2>&1 || missing="$missing curl"
  command -v tar  >/dev/null 2>&1 || missing="$missing tar"
  if [ -n "$missing" ]; then
    log "Installing base tools:$missing"
    pm_install $missing || { err "Cannot install base tools — install manually:$missing"; exit 1; }
  fi
  ok "Base tools (curl, tar) present"
}

# ---- Docker Engine + Buildx (BuildKit) ----
check_docker() {
  if [ "$SKIP_DOCKER" = "1" ]; then
    warn "Docker check skipped (TENTAFLOW_SKIP_DOCKER=1). Deploys will fail until you install Docker manually."
    return 0
  fi

  if command -v docker >/dev/null 2>&1; then
    ok "Docker present: $(docker --version 2>/dev/null || echo '?')"
  else
    log "Docker not found — installing"
    case "$PM" in
      apt)
        # Official Docker APT repo (works on Ubuntu/Debian stable)
        $SUDO apt-get update -qq
        $SUDO DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
            ca-certificates curl gnupg lsb-release
        $SUDO install -m 0755 -d /etc/apt/keyrings
        DISTRO=$(. /etc/os-release; echo "$ID")
        CODENAME=$(. /etc/os-release; echo "${VERSION_CODENAME:-$UBUNTU_CODENAME}")
        curl -fsSL "https://download.docker.com/linux/$DISTRO/gpg" | $SUDO gpg --dearmor -o /etc/apt/keyrings/docker.gpg 2>/dev/null || true
        $SUDO chmod a+r /etc/apt/keyrings/docker.gpg 2>/dev/null || true
        echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/$DISTRO $CODENAME stable" \
          | $SUDO tee /etc/apt/sources.list.d/docker.list >/dev/null
        $SUDO apt-get update -qq
        $SUDO DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
            docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
        ;;
      dnf)
        $SUDO dnf -y install dnf-plugins-core
        $SUDO dnf config-manager --add-repo https://download.docker.com/linux/fedora/docker-ce.repo 2>/dev/null \
          || $SUDO dnf-3 config-manager --add-repo https://download.docker.com/linux/fedora/docker-ce.repo
        $SUDO dnf install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
        ;;
      pacman)
        pm_install docker docker-buildx
        ;;
      zypper)
        pm_install docker docker-buildx
        ;;
      brew)
        warn "Docker on macOS requires Docker Desktop — download from https://docs.docker.com/desktop/install/mac-install/"
        warn "Install it manually, then re-run this installer."
        return 0
        ;;
      *)
        err "Cannot auto-install Docker on this system. See https://docs.docker.com/engine/install/"
        return 1 ;;
    esac

    # Linux — start service + enable
    if command -v systemctl >/dev/null 2>&1; then
      $SUDO systemctl enable --now docker 2>/dev/null || true
    fi
    ok "Docker installed"
  fi

  # Buildx plugin (BuildKit) — required for --mount=type=cache in our Dockerfiles.
  if ! docker buildx version >/dev/null 2>&1; then
    log "Docker buildx plugin missing — installing"
    case "$PM" in
      apt)    pm_install docker-buildx-plugin ;;
      dnf)    pm_install docker-buildx-plugin ;;
      pacman) pm_install docker-buildx ;;
      zypper) pm_install docker-buildx ;;
      brew)   warn "Install Docker Desktop — buildx is bundled" ;;
    esac
  fi
  if docker buildx version >/dev/null 2>&1; then
    ok "Docker buildx (BuildKit) present: $(docker buildx version 2>/dev/null | head -1)"
  else
    warn "buildx still missing — container builds będą failować z 'the --mount option requires BuildKit'"
  fi

  # Docker daemon reachability
  if ! docker info >/dev/null 2>&1; then
    warn "Docker daemon nie odpowiada. Spróbuj: sudo systemctl start docker  (Linux) / otwórz Docker Desktop (macOS)"
  fi

  # Add current user to docker group (Linux only, skippable)
  if [ "$NO_GROUP" = "0" ] && [ "$(uname -s)" = "Linux" ] && [ "$USER_INSTALL" = "0" ]; then
    if ! id -nG "$USER" 2>/dev/null | grep -qw docker; then
      if getent group docker >/dev/null 2>&1; then
        log "Adding $USER to 'docker' group (needed to run docker without sudo)"
        $SUDO usermod -aG docker "$USER" || true
        warn "Log out and back in (or 'newgrp docker') for group change to take effect."
      fi
    fi
  fi
}

# ---- Python 3.10+ with venv + pip ----
check_python() {
  if [ "$SKIP_PYTHON" = "1" ]; then
    warn "Python check skipped. Python-bundle engines (vLLM, xtts, …) will fail until Python 3.10+ z venv+pip is installed."
    return 0
  fi

  python_cmd=""
  for cand in python3.12 python3.11 python3.10 python3; do
    if command -v "$cand" >/dev/null 2>&1; then
      if "$cand" -c 'import sys; sys.exit(0 if sys.version_info >= (3,10) else 1)' 2>/dev/null; then
        python_cmd="$cand"
        break
      fi
    fi
  done

  if [ -z "$python_cmd" ]; then
    log "Python 3.10+ not found — installing"
    case "$PM" in
      apt)    pm_install python3 python3-venv python3-pip ;;
      dnf)    pm_install python3 python3-pip ;;
      pacman) pm_install python python-pip ;;
      zypper) pm_install python3 python3-pip ;;
      brew)   pm_install python@3.12 ;;
      *)      warn "Install Python 3.10+ manually with venv and pip"; return 0 ;;
    esac
    python_cmd=$(command -v python3.12 || command -v python3.11 || command -v python3.10 || command -v python3)
  fi

  ok "Python present: $python_cmd ($($python_cmd --version 2>&1))"

  # venv module
  if ! $python_cmd -m venv --help >/dev/null 2>&1; then
    log "python venv missing — installing"
    case "$PM" in
      apt) pm_install python3-venv ;;
      *) warn "python venv not usable — Python-bundle deploys (vLLM, xtts) will fail" ;;
    esac
  fi

  # pip
  if ! $python_cmd -m pip --version >/dev/null 2>&1; then
    log "pip missing — installing via ensurepip"
    $python_cmd -m ensurepip --upgrade 2>/dev/null || {
      case "$PM" in
        apt) pm_install python3-pip ;;
        *)   warn "pip not installable automatically — deploy z pip install nie zadziała" ;;
      esac
    }
  fi
  if $python_cmd -m pip --version >/dev/null 2>&1; then
    ok "pip present: $($python_cmd -m pip --version 2>/dev/null | head -1)"
  fi
}

# =============================================================================
# Run dependency install
# =============================================================================

echo ""
echo "${C_BOLD}TentaFlow installer${C_RESET}"
echo "${C_DIM}  target:  $TARGET${C_RESET}"
echo "${C_DIM}  pm:      $PM${C_RESET}"
echo "${C_DIM}  version: $VERSION${C_RESET}"
echo "${C_DIM}  prefix:  $PREFIX${C_RESET}"
echo ""

if [ "$SKIP_DEPS" = "0" ]; then
  log "Checking dependencies"
  check_base_tools
  check_docker
  check_python
  echo ""
fi

# =============================================================================
# Download + extract
# =============================================================================

mkdir -p "$PREFIX" 2>/dev/null || $SUDO mkdir -p "$PREFIX"
$SUDO mkdir -p "$BIN_DIR"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

if [ "$VERSION" = "latest" ]; then
  log "Resolving latest release tag"
  ACTUAL_TAG=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | grep -m1 '"tag_name"' | sed 's/.*"\(v[^"]*\)".*/\1/')
  if [ -z "$ACTUAL_TAG" ]; then
    err "Cannot resolve latest version from GitHub API. Try TENTAFLOW_VERSION=v0.x.y ./install.sh"
    exit 1
  fi
  VERSION="$ACTUAL_TAG"
  ok "Latest: $VERSION"
fi
ASSET_URL="https://github.com/$REPO/releases/download/$VERSION/tentaflow-${VERSION}-${TARGET}.tar.gz"

log "Downloading $ASSET_URL"
curl -fL --progress-bar "$ASSET_URL" -o "$TMP/tentaflow.tar.gz"
curl -fL "$ASSET_URL.sha256" -o "$TMP/tentaflow.tar.gz.sha256" 2>/dev/null || true

if [ -s "$TMP/tentaflow.tar.gz.sha256" ]; then
  log "Verifying SHA-256"
  (cd "$TMP" && shasum -a 256 -c tentaflow.tar.gz.sha256 >/dev/null) || {
    err "Checksum mismatch — aborting."
    exit 1
  }
  ok "Checksum OK"
fi

log "Extracting to $PREFIX"
tar -xzf "$TMP/tentaflow.tar.gz" -C "$TMP"
INNER=$(ls "$TMP" | grep -E "^tentaflow-${VERSION}-" | head -1)
$SUDO cp -r "$TMP/$INNER/." "$PREFIX/"
$SUDO ln -sf "$PREFIX/tentaflow" "$BIN_DIR/tentaflow"

if [ ! -f "$PREFIX/config.toml" ] && [ -f "$PREFIX/config.example.toml" ]; then
  $SUDO cp "$PREFIX/config.example.toml" "$PREFIX/config.toml"
fi
ok "Installed to $PREFIX"

# =============================================================================
# systemd / launchd registration
# =============================================================================

register_systemd() {
  UNIT="/etc/systemd/system/tentaflow.service"
  log "Registering systemd unit at $UNIT"
  $SUDO tee "$UNIT" >/dev/null <<EOF
[Unit]
Description=TentaFlow API Gateway + mesh node
After=network.target docker.service

[Service]
Type=simple
ExecStart=$PREFIX/tentaflow --config $PREFIX/config.toml
Restart=on-failure
RestartSec=5
WorkingDirectory=$PREFIX
Environment="DOCKER_BUILDKIT=1"

[Install]
WantedBy=multi-user.target
EOF
  $SUDO systemctl daemon-reload
  $SUDO systemctl enable --now tentaflow.service
  ok "Managed by systemd. Status: systemctl status tentaflow"
}

register_launchd() {
  PLIST="$HOME/Library/LaunchAgents/ai.tentaflow.plist"
  mkdir -p "$(dirname "$PLIST")"
  log "Registering launchd agent at $PLIST"
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
  <key>EnvironmentVariables</key>
  <dict>
    <key>DOCKER_BUILDKIT</key><string>1</string>
  </dict>
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
  ok "Managed by launchd. Status: launchctl list | grep tentaflow"
}

if [ "$NO_AUTOSTART" = "1" ]; then
  log "Skipping auto-start (TENTAFLOW_NO_AUTOSTART=1). Run manually: $BIN_DIR/tentaflow"
else
  case "$TARGET" in
    *linux*)
      if command -v systemctl >/dev/null 2>&1; then
        register_systemd
      else
        warn "systemd not found — run manually: $BIN_DIR/tentaflow"
      fi ;;
    *darwin*) register_launchd ;;
  esac
fi

# =============================================================================
# Final summary + next-step hints
# =============================================================================

echo ""
printf "%s%sInstallation complete%s\n" "$C_GREEN" "$C_BOLD" "$C_RESET"
printf "  %sbinary:%s     $BIN_DIR/tentaflow\n" "$C_DIM" "$C_RESET"
printf "  %sprefix:%s     $PREFIX\n" "$C_DIM" "$C_RESET"
printf "  %sversion:%s    $VERSION\n" "$C_DIM" "$C_RESET"
printf "  %sdashboard:%s  https://localhost:8090\n" "$C_DIM" "$C_RESET"
echo ""

# Post-install hints
if [ "$(uname -s)" = "Linux" ] && [ "$NO_GROUP" = "0" ] && [ "$USER_INSTALL" = "0" ]; then
  if ! id -nG "$USER" 2>/dev/null | grep -qw docker; then
    warn "Dodaj się do grupy docker (jeśli nie jesteś): 'sudo usermod -aG docker \$USER' + relogin"
  fi
fi

if [ "$(uname -s)" = "Darwin" ]; then
  if ! docker info >/dev/null 2>&1; then
    warn "Uruchom Docker Desktop zanim zrobisz pierwszy deploy silnika."
  fi
fi

echo ""
echo "Next steps:"
echo "  1. ${C_BOLD}Open dashboard:${C_RESET}      https://localhost:8090"
echo "  2. ${C_BOLD}Deploy silnik:${C_RESET}       Services → Nowy serwis → wybierz silnik"
echo "  3. ${C_BOLD}Ustaw auto-start:${C_RESET}    sudo systemctl enable tentaflow   (Linux)"
echo "  4. ${C_BOLD}Logi:${C_RESET}                journalctl -u tentaflow -f         (Linux)"
echo ""
