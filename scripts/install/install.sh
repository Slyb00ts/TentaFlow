#!/bin/sh
# =============================================================================
# File:        install.sh
# Description: Installs TentaFlow from GitHub Releases on Linux and macOS.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Slyb00ts/TentaFlow/main/scripts/install/install.sh | sh
#
# The bootstrap URL points at the repository, not at a release asset: a tag with
# an -alpha/-beta/-rc suffix is published as a pre-release, and GitHub's
# `releases/latest` skips those — the installer would fetch itself from a stale
# release, or fail outright on a repository that has only pre-releases.
#
# Layout (Linux; macOS uses /usr/local/{tentaflow,etc,var} and launchd):
#   /opt/tentaflow/versions/<ver>   binaries + our shared libraries
#   /opt/tentaflow/current          symlink to the live version (atomic swap)
#   /usr/local/bin/tentaflow        symlink into PATH
#   /etc/tentaflow/config.toml      configuration, never overwritten on update
#   /etc/tentaflow/install-receipt.json  what was installed and how
#   /var/lib/tentaflow              TENTAFLOW_HOME: data, TLS identity, SQLite
#
# Environment overrides:
#   TENTAFLOW_EDITION=full|slim      skip the interactive question
#   TENTAFLOW_VARIANT=vulkan|cuda12|cuda13   GPU backend for the full edition
#   TENTAFLOW_VERSION=v0.1.0         install a specific version
#   TENTAFLOW_BIND=0.0.0.0:8090      listen address (default 127.0.0.1:8090)
#   TENTAFLOW_PREFIX=/opt/tentaflow  install prefix
#   TENTAFLOW_ASSET_FILE=/path.tgz   install a local archive (CI / offline)
#   TENTAFLOW_USER_INSTALL=1         no sudo, everything under $HOME
#   TENTAFLOW_NO_AUTOSTART=1         do not register or start the service
#   TENTAFLOW_WITH_DOCKER=1          install Docker Engine if missing
#   TENTAFLOW_SKIP_DEPS=1            install no system packages
# =============================================================================
set -eu

REPO="Slyb00ts/TentaFlow"
VERSION="${TENTAFLOW_VERSION:-latest}"
EDITION="${TENTAFLOW_EDITION:-}"
# vulkan | cuda12 | cuda13 | metal | none — decides WHICH archive is fetched.
VARIANT="${TENTAFLOW_VARIANT:-}"
BIND="${TENTAFLOW_BIND:-127.0.0.1:8090}"
USER_INSTALL="${TENTAFLOW_USER_INSTALL:-0}"
NO_AUTOSTART="${TENTAFLOW_NO_AUTOSTART:-0}"
WITH_DOCKER="${TENTAFLOW_WITH_DOCKER:-0}"
SKIP_DEPS="${TENTAFLOW_SKIP_DEPS:-0}"
ASSET_FILE="${TENTAFLOW_ASSET_FILE:-}"
SERVICE_USER="tentaflow"

if [ -t 1 ] && [ "${NO_COLOR:-0}" = "0" ]; then
  C_BOLD="$(printf '\033[1m')"; C_DIM="$(printf '\033[2m')"
  C_RED="$(printf '\033[0;31m')"; C_GREEN="$(printf '\033[0;32m')"
  C_YELLOW="$(printf '\033[0;33m')"; C_BLUE="$(printf '\033[0;34m')"
  C_RESET="$(printf '\033[0m')"
else
  C_BOLD=""; C_DIM=""; C_RED=""; C_GREEN=""; C_YELLOW=""; C_BLUE=""; C_RESET=""
fi
log()  { printf "%s==>%s %s\n" "$C_BLUE" "$C_RESET" "$*"; }
ok()   { printf "%s  ok%s %s\n" "$C_GREEN" "$C_RESET" "$*"; }
warn() { printf "%s  !!%s %s\n" "$C_YELLOW" "$C_RESET" "$*" >&2; }
die()  { printf "%s  xx%s %s\n" "$C_RED" "$C_RESET" "$*" >&2; exit 1; }

# The target triple decides which release asset is downloaded, so an
# unsupported combination has to fail HERE rather than after a 160 MB download
# of an archive whose binary cannot run.
case "$(uname -s)/$(uname -m)" in
  Linux/x86_64)   OS=linux; TARGET="x86_64-unknown-linux-gnu" ;;
  Linux/aarch64)  OS=linux; TARGET="aarch64-unknown-linux-gnu" ;;
  Darwin/arm64)   OS=macos; TARGET="aarch64-apple-darwin" ;;
  Darwin/x86_64)  die "No build for Intel Macs yet — only Apple Silicon is built." ;;
  *) die "Unsupported platform: $(uname -s) $(uname -m)." ;;
esac
# macOS has no /etc or /var/lib for third-party software and no system-wide
# service account convention worth inventing here: the daemon runs as the
# installing user, which is what a single-user machine actually wants.
if [ "$USER_INSTALL" = "1" ]; then
  PREFIX="${TENTAFLOW_PREFIX:-$HOME/.local/share/tentaflow}"
  CONFIG_DIR="$HOME/.config/tentaflow"
  DATA_DIR="$HOME/.local/share/tentaflow/data"
  BIN_DIR="$HOME/.local/bin"
  SUDO=""
  SERVICE_SCOPE="user"
  SERVICE_USER="$(id -un)"
elif [ "$OS" = "macos" ]; then
  PREFIX="${TENTAFLOW_PREFIX:-/usr/local/tentaflow}"
  CONFIG_DIR="/usr/local/etc/tentaflow"
  DATA_DIR="/usr/local/var/tentaflow"
  BIN_DIR="/usr/local/bin"
  SERVICE_SCOPE="system"
  SERVICE_USER="${SUDO_USER:-$(id -un)}"
  if [ "$(id -u)" -eq 0 ]; then SUDO=""; else SUDO="sudo"; fi
else
  PREFIX="${TENTAFLOW_PREFIX:-/opt/tentaflow}"
  CONFIG_DIR="/etc/tentaflow"
  DATA_DIR="/var/lib/tentaflow"
  BIN_DIR="/usr/local/bin"
  SERVICE_SCOPE="system"
  if [ "$(id -u)" -eq 0 ]; then SUDO=""; else SUDO="sudo"; fi
fi
CONFIG="$CONFIG_DIR/config.toml"
RECEIPT="$CONFIG_DIR/install-receipt.json"


# =============================================================================
# Package manager
# =============================================================================
detect_pm() {
  if [ "$OS" = "macos" ]; then
    command -v brew >/dev/null 2>&1 && PM=brew || PM=none
    return
  fi
  for pm in apt-get dnf pacman zypper; do
    if command -v "$pm" >/dev/null 2>&1; then
      case "$pm" in
        apt-get) PM=apt ;; *) PM="$pm" ;;
      esac
      return
    fi
  done
  PM=unknown
}

pm_install() {
  [ "$SKIP_DEPS" = "1" ] && { warn "TENTAFLOW_SKIP_DEPS=1 — skipping: $*"; return 0; }
  case "$PM" in
    apt)
      $SUDO env DEBIAN_FRONTEND=noninteractive apt-get update -qq
      $SUDO env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$@" ;;
    dnf)    $SUDO dnf install -y --quiet "$@" ;;
    pacman) $SUDO pacman -Sy --noconfirm --needed "$@" ;;
    zypper) $SUDO zypper --non-interactive install "$@" ;;
    # brew refuses to run as root, so a sudo'd install still installs formulae
    # as the invoking user.
    brew)   if [ "$(id -u)" -eq 0 ] && [ -n "${SUDO_USER:-}" ]; then
              sudo -u "$SUDO_USER" brew install "$@"
            else
              brew install "$@"
            fi ;;
    none)   warn "No Homebrew — install it (https://brew.sh) or add these by hand: $*"; return 1 ;;
    *)      warn "Unknown package manager — install these by hand: $*"; return 1 ;;
  esac
}

# =============================================================================
# Portability floor
# =============================================================================
# The release is linked on Ubuntu 22.04, so the glibc and libstdc++ it was built
# against are the floor for every machine that installs it. Checking here turns
# an unrunnable binary ("version `GLIBCXX_3.4.30' not found" on the first start,
# long after the service was enabled) into a refusal that names the reason.
MIN_GLIBC=2.35
MIN_GLIBCXX=3.4.30
MIN_MACOS=15.0

version_ge() { [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -1)" = "$2" ]; }

check_libc_floor() {
  if [ "$OS" = "macos" ]; then
    # 15.0, not 14: KokoroBridge and the vendored MisakiSwift declare
    # .macOS(.v15), so the dylibs we ship refuse to load below it and the MLX
    # engines — the whole point of the macOS edition — would be missing.
    macver=$(sw_vers -productVersion 2>/dev/null || echo "")
    case "$macver" in
      [0-9]*) version_ge "$macver" "$MIN_MACOS" \
                || die "This Mac runs macOS $macver; the package needs $MIN_MACOS or newer." ;;
      *) warn "Could not read the macOS version — skipping the compatibility check." ;;
    esac
    ok "Compatible: macOS $macver"
    return 0
  fi
  glibc=$(ldd --version 2>/dev/null | head -1 | awk '{print $NF}')
  case "$glibc" in
    [0-9]*) ;;
    *) warn "Could not read the glibc version — skipping the compatibility check."; return 0 ;;
  esac
  version_ge "$glibc" "$MIN_GLIBC" || die \
"This system has glibc $glibc; the package is built against glibc >= $MIN_GLIBC.
   Supported: Ubuntu 22.04+, Debian 12+, Fedora 38+, RHEL 10+, Arch/CachyOS.
   Older ones (RHEL 9, Debian 11, Ubuntu 20.04) have to build from source."

  # libstdc++ ships its ABI versions as symbols in the .so; the newest GLIBCXX_*
  # it defines is what the binary can demand.
  libcxx=$(ldconfig -p 2>/dev/null | awk '/libstdc\+\+\.so\.6/ {print $NF; exit}')
  [ -n "$libcxx" ] && [ -r "$libcxx" ] || {
    warn "libstdc++.so.6 not found — skipping the C++ ABI check."
    return 0
  }
  # grep -ao, not strings: binutils is not installed everywhere, grep is.
  have=$(grep -ao 'GLIBCXX_[0-9][0-9.]*' "$libcxx" 2>/dev/null \
    | sed 's/^GLIBCXX_//; s/\.$//' | sort -V | tail -1)
  [ -n "$have" ] || { warn "Could not read GLIBCXX from $libcxx — skipping."; return 0; }
  version_ge "$have" "$MIN_GLIBCXX" || die \
"This system has libstdc++ with GLIBCXX_$have; the package needs GLIBCXX_$MIN_GLIBCXX.
   Install a newer libstdc++ (gcc >= 12) or build TentaFlow from source."
  ok "ABI compatible: glibc $glibc, GLIBCXX_$have"
}

# =============================================================================
# Which edition
# =============================================================================
# Hardware detection PROPOSES; the user decides. Reading the GPU is not enough:
# an integrated GB10 on a DGX Spark is a real CUDA target and a Strix Halo is a
# real Vulkan one, while the integrated chip in a thin laptop is neither — and
# unified memory means VRAM size does not separate them either.
# Which CUDA line a card needs. CUDA 13 dropped everything below sm_75 and is
# the only one that knows sm_103 (B300) and sm_121 (GB10 / DGX Spark); 12.8
# still serves Turing through consumer Blackwell. The driver decides too: a
# 13.x runtime refuses to load under a driver older than 580.
cuda_variant_for_gpu() {
  cc=$(nvidia-smi --query-gpu=compute_cap --format=csv,noheader 2>/dev/null | head -1 | tr -d ' .')
  drv=$(nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null | head -1 | cut -d. -f1)
  [ -n "$cc" ] || { echo ""; return; }
  # Blackwell Ultra (103) and GB10 (121) exist only in the 13.x line.
  if [ "$cc" -ge 103 ] 2>/dev/null && [ "$cc" -ne 120 ] 2>/dev/null; then
    echo "cuda13"; return
  fi
  if [ -n "$drv" ] && [ "$drv" -ge 580 ] 2>/dev/null && [ "$cc" -ge 75 ] 2>/dev/null; then
    echo "cuda13"; return
  fi
  if [ "$cc" -ge 50 ] 2>/dev/null; then echo "cuda12"; return; fi
  echo ""
}

detect_edition() {
  if [ "$OS" = "macos" ]; then
    # Every Apple Silicon Mac has a usable GPU and the Metal/MLX engines are
    # part of the full edition, so the proposal is full regardless of model.
    GPU_DESC="Apple Silicon: $(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo 'GPU Metal')"
    PROPOSED=full
    PROPOSED_VARIANT=metal
    return
  fi
  if command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi -L >/dev/null 2>&1; then
    GPU_DESC="NVIDIA: $(nvidia-smi -L 2>/dev/null | head -1)"
    PROPOSED=full
    PROPOSED_VARIANT="$(cuda_variant_for_gpu)"
    [ -n "$PROPOSED_VARIANT" ] || PROPOSED_VARIANT=vulkan
  elif [ -e /sys/class/drm/card0 ]; then
    GPU_DESC="GPU present (AMD/Intel — Vulkan path)"
    PROPOSED=full
    PROPOSED_VARIANT=vulkan
  else
    GPU_DESC="no GPU"
    PROPOSED=slim
    PROPOSED_VARIANT=none
  fi
}

choose_edition() {
  detect_edition
  # macOS has one edition. The MLX engines come in through a per-target
  # dependency block that --no-default-features does not switch off, so a "slim"
  # macOS build would ship the same engines under a name that promises none —
  # there is no such asset, and offering the choice would be a lie.
  if [ "$OS" = "macos" ]; then
    case "${EDITION:-full}" in
      full|"") EDITION=full ;;
      slim) die "There is no slim edition for macOS (the MLX engines are compiled into this target)." ;;
      *) die "Unknown edition '$EDITION' (macOS has only: full)" ;;
    esac
    ok "Edition: full (Metal/MLX)"
    return
  fi
  if [ -n "$EDITION" ]; then
    ok "Edition from TENTAFLOW_EDITION: $EDITION"
    return
  fi
  echo ""
  echo "  ${C_BOLD}Detected:${C_RESET} $GPU_DESC"
  echo ""
  echo "    ${C_BOLD}full${C_RESET}  llama.cpp, whisper, vision, TTS           ~161 MB"
  echo "          local inference on the GPU; variant: ${C_BOLD}$PROPOSED_VARIANT${C_RESET}"
  echo "    ${C_BOLD}slim${C_RESET}  gateway only: mesh, flows, dashboard       ~91 MB"
  echo "          no local engines; the catalog keeps cloud providers"
  echo "          (OpenAI, Anthropic, ...) and the utility containers"
  echo ""
  case "$PROPOSED_VARIANT" in
    cuda12) echo "  ${C_DIM}CUDA 12.8 — Turing..Blackwell (sm_75-sm_120)${C_RESET}" ;;
    cuda13) echo "  ${C_DIM}CUDA 13.2 — required for B300 (sm_103) and GB10 / DGX Spark (sm_121)${C_RESET}" ;;
    vulkan) echo "  ${C_DIM}Vulkan — the portable backend for AMD, Intel and NVIDIA without CUDA${C_RESET}" ;;
  esac
  echo ""
  if [ ! -t 0 ]; then
    EDITION="$PROPOSED"
    warn "No terminal (curl | sh) — choosing '$EDITION'. Override with TENTAFLOW_EDITION=full|slim."
    return
  fi
  printf "  Which edition should be installed? [%s]: " "$PROPOSED"
  read -r answer </dev/tty || answer=""
  EDITION="${answer:-$PROPOSED}"
  case "$EDITION" in
    full|slim) ok "Edition: $EDITION" ;;
    *) die "Unknown edition '$EDITION' (allowed: full, slim)" ;;
  esac
}

# The archive to fetch. slim has one build per architecture; full has one per
# GPU backend, and picking the wrong one gives a binary that either cannot start
# (CUDA without the driver) or leaves the GPU idle (Vulkan on an NVIDIA rig).
choose_variant() {
  if [ "$EDITION" = "slim" ]; then VARIANT=none; return; fi
  if [ -n "$VARIANT" ]; then ok "Variant from TENTAFLOW_VARIANT: $VARIANT"; return; fi
  VARIANT="$PROPOSED_VARIANT"
  [ "$VARIANT" = "none" ] && VARIANT=vulkan
  case "$VARIANT" in
    vulkan|cuda12|cuda13|metal) ok "Variant: $VARIANT" ;;
    *) die "Unknown variant '$VARIANT' (allowed: vulkan, cuda12, cuda13, metal)" ;;
  esac
}

# A CUDA build links libcudart and libcublas; without them the service starts
# and dies immediately. Checking here turns that into a message with a fix.
check_cuda_runtime() {
  case "$VARIANT" in cuda*) ;; *) return 0 ;; esac
  command -v nvidia-smi >/dev/null 2>&1 || \
    warn "No nvidia-smi — the $VARIANT variant needs the NVIDIA driver."
  missing=""
  for lib in libcudart.so libcublas.so; do
    ldconfig -p 2>/dev/null | grep -q "$lib" || missing="$missing $lib"
  done
  [ -z "$missing" ] && { ok "CUDA runtime present"; return 0; }
  warn "Missing CUDA runtime libraries:$missing"
  case "$VARIANT" in
    cuda12) pkg="cuda-runtime-12-8" ;;
    cuda13) pkg="cuda-runtime-13-2" ;;
  esac
  warn "Install them from NVIDIA's repository (package $pkg), or choose the"
  warn "vulkan variant instead: TENTAFLOW_VARIANT=vulkan."
}

# =============================================================================
# Runtime dependencies
# =============================================================================
install_runtime_deps() {
  [ "$SKIP_DEPS" = "1" ] && { warn "Skipping system dependencies."; return; }

  command -v curl >/dev/null 2>&1 || pm_install curl || die "curl is missing."
  command -v tar  >/dev/null 2>&1 || pm_install tar  || die "tar is missing."

  if [ "$EDITION" = "full" ]; then
    # GStreamer needs its PLUGINS, not just the core library: the camera
    # pipeline builds elements from base/good/bad. libvulkan1 is the loader the
    # binary links against (libvulkan.so.1 in NEEDED).
    log "Installing runtime libraries (GStreamer + plugins, Vulkan loader)"
    case "$PM" in
      apt) pm_install libgstreamer1.0-0 gstreamer1.0-plugins-base \
             gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
             gstreamer1.0-libav libvulkan1 libgomp1 || warn "Some packages failed to install." ;;
      dnf) pm_install gstreamer1 gstreamer1-plugins-base gstreamer1-plugins-good \
             gstreamer1-plugins-bad-free vulkan-loader libgomp || warn "Some packages failed to install." ;;
      pacman) pm_install gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad \
             vulkan-icd-loader || warn "Some packages failed to install." ;;
      zypper) pm_install gstreamer gstreamer-plugins-base gstreamer-plugins-good \
             libvulkan1 || warn "Some packages failed to install." ;;
      # macOS needs no Vulkan loader (inference is Metal/MLX) — only GStreamer,
      # which the camera pipeline links against.
      brew) pm_install gstreamer || warn "Installing GStreamer through brew failed." ;;
      none) warn "No Homebrew — install GStreamer by hand (brew install gstreamer)." ;;
      *) warn "Unknown package manager — install GStreamer with its plugins and the Vulkan loader by hand." ;;
    esac
  fi

  if ! command -v docker >/dev/null 2>&1; then
    if [ "$WITH_DOCKER" = "1" ] && [ "$OS" = "macos" ]; then
      # Docker Desktop is a GUI app with a licence to accept; get.docker.com is
      # Linux-only and installing a cask silently is not our call.
      warn "On macOS install Docker Desktop by hand: https://docker.com/products/docker-desktop"
    elif [ "$WITH_DOCKER" = "1" ]; then
      log "Installing Docker Engine"
      curl -fsSL https://get.docker.com | $SUDO sh || warn "Installing Docker failed."
      $SUDO systemctl enable --now docker 2>/dev/null || true
    else
      warn "Docker is not installed — container engines will be unavailable."
      warn "The installer does NOT add it without consent: re-run with TENTAFLOW_WITH_DOCKER=1."
    fi
  fi
}

# =============================================================================
# Download
# =============================================================================
resolve_version() {
  [ "$VERSION" != "latest" ] && return
  log "Resolving the newest version"
  # /releases, not /releases/latest: the latter hides pre-releases, and every
  # tag so far carries an -alpha suffix.
  VERSION=$(curl -fsSL "https://api.github.com/repos/$REPO/releases?per_page=10" \
    | grep -m1 '"tag_name"' | sed 's/.*"\(v[^"]*\)".*/\1/')
  [ -n "$VERSION" ] || die "Could not resolve a version from the GitHub API (60 requests/h per IP?)."
  ok "Version: $VERSION"
}

verify_sha256() {
  archive="$1"; sumfile="$2"
  if command -v sha256sum >/dev/null 2>&1; then
    ( cd "$(dirname "$archive")" && sha256sum -c "$(basename "$sumfile")" >/dev/null ) \
      || die "Checksum mismatch — aborting."
  elif command -v shasum >/dev/null 2>&1; then
    ( cd "$(dirname "$archive")" && shasum -a 256 -c "$(basename "$sumfile")" >/dev/null ) \
      || die "Checksum mismatch — aborting."
  else
    # Silently skipping verification in a `curl | sh` installer is how a
    # tampered archive gets installed, so this is fatal rather than a warning.
    die "Neither sha256sum nor shasum — cannot verify the archive."
  fi
  ok "Checksum OK"
}

download_archive() {
  TMP=$(mktemp -d)
  trap 'rm -rf "$TMP"' EXIT
  ARCHIVE="$TMP/tentaflow.tar.gz"

  if [ -n "$ASSET_FILE" ]; then
    log "Using a local archive: $ASSET_FILE"
    cp "$ASSET_FILE" "$ARCHIVE"
    [ -f "$ASSET_FILE.sha256" ] && cp "$ASSET_FILE.sha256" "$ARCHIVE.sha256"
  else
    resolve_version
    if [ "$EDITION" = "slim" ]; then
      name="tentaflow-${VERSION}-${TARGET}-slim.tar.gz"
    else
      name="tentaflow-${VERSION}-${TARGET}-full-${VARIANT}.tar.gz"
    fi
    url="https://github.com/$REPO/releases/download/$VERSION/$name"
    log "Downloading $name"
    curl -fL --progress-bar "$url" -o "$ARCHIVE" || die "Download failed: $url"
    curl -fsSL "$url.sha256" -o "$ARCHIVE.sha256" || die "No .sha256 file for $name."
  fi

  # A checksum file names the archive it was made from, which is never the
  # temp name we just wrote — rewrite the name, keep the digest.
  if [ -f "$ARCHIVE.sha256" ]; then
    digest=$(awk '{print $1; exit}' "$ARCHIVE.sha256")
    printf '%s  %s\n' "$digest" "$(basename "$ARCHIVE")" > "$ARCHIVE.sha256"
  fi
  [ -f "$ARCHIVE.sha256" ] || die "The archive has no checksum."
  verify_sha256 "$ARCHIVE" "$ARCHIVE.sha256"
}

# =============================================================================
# Install
# =============================================================================
install_files() {
  log "Unpacking"
  tar -xzf "$ARCHIVE" -C "$TMP"
  inner=$(find "$TMP" -maxdepth 1 -type d -name 'tentaflow-*' | head -1)
  [ -n "$inner" ] || die "The archive has an unexpected structure."

  # The version comes from the binary itself, so a local archive (CI, offline)
  # lands in a correctly named directory without trusting the file name.
  ver=$("$inner/tentaflow" --version 2>/dev/null | awk '{print $2}')
  [ -n "$ver" ] || die "Cannot read the version from the binary."
  VERSION_DIR="$PREFIX/versions/$ver"

  $SUDO mkdir -p "$PREFIX/versions" "$CONFIG_DIR" "$DATA_DIR" "$BIN_DIR"
  $SUDO rm -rf "$VERSION_DIR"
  $SUDO cp -r "$inner" "$VERSION_DIR"

  # Atomic swap: rename over the symlink, never unlink-then-link, so a reader
  # never sees a missing `current`.
  $SUDO ln -sfn "$VERSION_DIR" "$PREFIX/current.new"
  $SUDO mv -T "$PREFIX/current.new" "$PREFIX/current"
  $SUDO ln -sfn "$PREFIX/current/tentaflow" "$BIN_DIR/tentaflow"
  ok "Installed $ver in $VERSION_DIR"
  INSTALLED_VERSION="$ver"
}

write_config() {
  if [ -f "$CONFIG" ]; then
    ok "Configuration exists — leaving it alone: $CONFIG"
    return
  fi
  log "Writing the configuration ($BIND, mesh disabled)"
  # The binary owns the config schema; composing TOML here would duplicate it
  # and drift on the first change.
  $SUDO "$PREFIX/current/tentaflow" init-config --output "$CONFIG" --bind "$BIND" --no-mesh
}

write_receipt() {
  # `tentaflow update` builds the next download from these two fields, so they
  # must say what was installed, not what the hardware looked like.
  variant="$VARIANT"
  $SUDO sh -c "cat > '$RECEIPT'" <<EOF
{
  "version": "$INSTALLED_VERSION",
  "edition": "$EDITION",
  "variant": "$variant",
  "target": "$TARGET",
  "prefix": "$PREFIX",
  "config": "$CONFIG",
  "home": "$DATA_DIR",
  "service_scope": "$SERVICE_SCOPE"
}
EOF
  ok "Receipt: $RECEIPT"
}

create_service_user() {
  [ "$USER_INSTALL" = "1" ] && return
  if [ "$OS" = "macos" ]; then
    # No service account is created: the daemon runs as the installing user
    # (SERVICE_USER), so the data directory simply has to belong to them.
    $SUDO chown -R "$SERVICE_USER" "$DATA_DIR" "$CONFIG_DIR" 2>/dev/null || true
    return
  fi
  if ! id "$SERVICE_USER" >/dev/null 2>&1; then
    log "Creating the system user $SERVICE_USER"
    $SUDO useradd --system --home-dir "$DATA_DIR" --shell /usr/sbin/nologin "$SERVICE_USER" \
      || $SUDO useradd --system --home-dir "$DATA_DIR" --shell /sbin/nologin "$SERVICE_USER" \
      || warn "Could not create the user — the service would run as root."
  fi
  for grp in docker video render; do
    getent group "$grp" >/dev/null 2>&1 && $SUDO usermod -aG "$grp" "$SERVICE_USER" 2>/dev/null || true
  done
  $SUDO chown -R "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR" "$CONFIG_DIR" 2>/dev/null || true
}

# launchd, not systemd: on macOS a LaunchAgent waits for a login, so a server
# that must come up with the machine is a LaunchDaemon. The per-user install
# keeps an agent, which is exactly what its scope means.
register_launchd() {
  template="$PREFIX/current/ai.tentaflow.plist.in"
  [ -f "$template" ] || die "No launchd template in the archive: $template"

  if [ "$SERVICE_SCOPE" = "user" ]; then
    plist="$HOME/Library/LaunchAgents/ai.tentaflow.plist"
    logdir="$HOME/Library/Logs/TentaFlow"
    domain="gui/$(id -u)"
    userblock=""
    mkdir -p "$(dirname "$plist")" "$logdir"
  else
    plist="/Library/LaunchDaemons/ai.tentaflow.plist"
    logdir="/usr/local/var/log/tentaflow"
    domain="system"
    userblock="  <key>UserName</key><string>$SERVICE_USER</string>"
    $SUDO mkdir -p "$(dirname "$plist")" "$logdir"
    $SUDO chown "$SERVICE_USER" "$logdir" 2>/dev/null || true
  fi

  body=$(sed -e "s|@PREFIX@|$PREFIX|g" -e "s|@CONFIG@|$CONFIG|g" \
             -e "s|@HOME@|$DATA_DIR|g" -e "s|@LOGDIR@|$logdir|g" \
             -e "s|@USERNAME@|$userblock|g" "$template")

  # bootout first: bootstrap over a loaded service fails with "service already
  # loaded", which would leave a reinstall running the OLD binary.
  if [ "$SERVICE_SCOPE" = "user" ]; then
    launchctl bootout "$domain/ai.tentaflow" 2>/dev/null || true
    printf '%s\n' "$body" > "$plist"
    launchctl bootstrap "$domain" "$plist" || die "launchctl bootstrap failed."
  else
    $SUDO launchctl bootout "$domain/ai.tentaflow" 2>/dev/null || true
    printf '%s\n' "$body" | $SUDO tee "$plist" >/dev/null
    $SUDO chmod 644 "$plist"
    $SUDO chown root:wheel "$plist"
    $SUDO launchctl bootstrap "$domain" "$plist" || die "launchctl bootstrap failed."
  fi
  ok "Service registered (launchd, $SERVICE_SCOPE)"
}

register_service() {
  [ "$NO_AUTOSTART" = "1" ] && { warn "Skipping service registration (TENTAFLOW_NO_AUTOSTART=1)."; return; }
  if [ "$OS" = "macos" ]; then
    register_launchd
    return
  fi
  command -v systemctl >/dev/null 2>&1 || { warn "No systemd — start it by hand: tentaflow"; return; }

  template="$PREFIX/current/tentaflow.service.in"
  [ -f "$template" ] || die "No unit template in the archive: $template"

  unit_body=$(sed -e "s|@PREFIX@|$PREFIX|g" -e "s|@HOME@|$DATA_DIR|g" \
                  -e "s|@CONFIG@|$CONFIG|g" -e "s|@CONFIG_DIR@|$CONFIG_DIR|g" \
                  -e "s|@USER@|$SERVICE_USER|g" "$template")

  if [ "$SERVICE_SCOPE" = "user" ]; then
    unit_dir="$HOME/.config/systemd/user"
    mkdir -p "$unit_dir"
    # A user unit has no User=/Group=: it already runs as the logged-in user,
    # and systemd refuses the directives in that scope.
    printf '%s\n' "$unit_body" | grep -v '^User=\|^Group=' > "$unit_dir/tentaflow.service"
    systemctl --user daemon-reload
    systemctl --user enable --now tentaflow.service
    # Without lingering the service stops at logout, which is not what
    # "starts with the system" means to anyone.
    loginctl enable-linger "$(id -un)" 2>/dev/null || warn "loginctl enable-linger failed — the service will stop at logout."
    ok "Service registered (systemd --user)"
  else
    printf '%s\n' "$unit_body" | $SUDO tee /etc/systemd/system/tentaflow.service >/dev/null
    $SUDO systemctl daemon-reload
    $SUDO systemctl enable --now tentaflow.service
    ok "Service registered and enabled (systemd)"
  fi
}

harden_platform() {
  if [ "$OS" = "macos" ]; then
    # An archive that ever passed through a browser or an app carries the
    # quarantine attribute, and Gatekeeper then kills the unsigned binary and
    # every dylib next to it with a dialog no daemon can answer.
    $SUDO xattr -dr com.apple.quarantine "$PREFIX/current" 2>/dev/null || true
    return
  fi
  [ "$USER_INSTALL" = "1" ] && return
  # Fedora/RHEL: files unpacked into /opt carry no service context, and SELinux
  # can block the JIT mappings wgpu/ggml need.
  if command -v restorecon >/dev/null 2>&1; then
    $SUDO restorecon -R "$PREFIX" 2>/dev/null || true
  fi
  if command -v firewall-cmd >/dev/null 2>&1 && [ "${BIND%%:*}" = "0.0.0.0" ]; then
    port="${BIND##*:}"
    log "Opening port $port in firewalld (bound to 0.0.0.0)"
    $SUDO firewall-cmd --permanent --add-port="$port/tcp" >/dev/null 2>&1 || true
    $SUDO firewall-cmd --reload >/dev/null 2>&1 || true
  fi
}

stop_if_running() {
  if [ "$OS" = "macos" ]; then
    # A running daemon holds the dylibs it loaded; replacing the version
    # directory underneath it is how you get a half-updated process.
    if [ "$SERVICE_SCOPE" = "user" ]; then
      launchctl bootout "gui/$(id -u)/ai.tentaflow" 2>/dev/null || true
    else
      $SUDO launchctl bootout system/ai.tentaflow 2>/dev/null || true
    fi
    return 0
  fi
  command -v systemctl >/dev/null 2>&1 || return 0
  if [ "$SERVICE_SCOPE" = "user" ]; then
    systemctl --user is-active --quiet tentaflow.service 2>/dev/null && {
      log "Stopping the running service before the swap"
      systemctl --user stop tentaflow.service
    }
  else
    $SUDO systemctl is-active --quiet tentaflow.service 2>/dev/null && {
      log "Stopping the running service before the swap"
      $SUDO systemctl stop tentaflow.service
    }
  fi
  return 0
}

# =============================================================================
# Run
# =============================================================================
echo ""
echo "${C_BOLD}TentaFlow installer${C_RESET}"
detect_pm
if [ -r /etc/os-release ]; then
  SYSTEM_NAME=$(. /etc/os-release; echo "${PRETTY_NAME:-$(uname -s)}")
elif [ "$OS" = "macos" ]; then
  SYSTEM_NAME="macOS $(sw_vers -productVersion 2>/dev/null || echo '')"
else
  SYSTEM_NAME=$(uname -s)
fi
echo "${C_DIM}  system:  $SYSTEM_NAME / $PM${C_RESET}"
echo "${C_DIM}  prefix:  $PREFIX${C_RESET}"
echo "${C_DIM}  data:    $DATA_DIR${C_RESET}"

check_libc_floor
choose_edition
choose_variant
check_cuda_runtime
install_runtime_deps
download_archive
stop_if_running
install_files
# Before write_config, which RUNS the freshly unpacked binary: on macOS the
# quarantine attribute has to be gone by then or Gatekeeper kills that very
# first invocation.
harden_platform
write_config
create_service_user
write_receipt
register_service

echo ""
printf "%s%sDone.%s\n" "$C_GREEN" "$C_BOLD" "$C_RESET"
printf "  %sbinary:%s    $BIN_DIR/tentaflow\n" "$C_DIM" "$C_RESET"
printf "  %sversion:%s   $INSTALLED_VERSION ($EDITION/$VARIANT)\n" "$C_DIM" "$C_RESET"
printf "  %sdashboard:%s https://%s\n" "$C_DIM" "$C_RESET" "$BIND"
echo ""
echo "  ${C_BOLD}First login: admin / admin${C_RESET} — change the password right after signing in."
if [ "${BIND%%:*}" = "0.0.0.0" ]; then
  warn "The server listens on every interface with the default password — change it NOW."
  warn "The certificate is self-signed; for a domain name set [server.tls].extra_sans in $CONFIG."
fi
echo ""
echo "  tentaflow status     service state, autostart, health"
echo "  tentaflow stop|start stop / start the service"
echo "  tentaflow update     update from GitHub Releases"
echo ""
