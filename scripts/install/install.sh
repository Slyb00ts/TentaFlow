#!/bin/sh
# =============================================================================
# File:        install.sh
# Description: Installs TentaFlow from GitHub Releases on Linux.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Slyb00ts/TentaFlow/main/scripts/install/install.sh | sh
#
# The bootstrap URL points at the repository, not at a release asset: a tag with
# an -alpha/-beta/-rc suffix is published as a pre-release, and GitHub's
# `releases/latest` skips those — the installer would fetch itself from a stale
# release, or fail outright on a repository that has only pre-releases.
#
# Layout:
#   /opt/tentaflow/versions/<ver>   binaries + our shared libraries
#   /opt/tentaflow/current          symlink to the live version (atomic swap)
#   /usr/local/bin/tentaflow        symlink into PATH
#   /etc/tentaflow/config.toml      configuration, never overwritten on update
#   /etc/tentaflow/install-receipt.json  what was installed and how
#   /var/lib/tentaflow              TENTAFLOW_HOME: data, TLS identity, SQLite
#
# Environment overrides:
#   TENTAFLOW_EDITION=full|slim      skip the interactive question
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
TARGET="x86_64-unknown-linux-gnu"
VERSION="${TENTAFLOW_VERSION:-latest}"
EDITION="${TENTAFLOW_EDITION:-}"
BIND="${TENTAFLOW_BIND:-127.0.0.1:8090}"
USER_INSTALL="${TENTAFLOW_USER_INSTALL:-0}"
NO_AUTOSTART="${TENTAFLOW_NO_AUTOSTART:-0}"
WITH_DOCKER="${TENTAFLOW_WITH_DOCKER:-0}"
SKIP_DEPS="${TENTAFLOW_SKIP_DEPS:-0}"
ASSET_FILE="${TENTAFLOW_ASSET_FILE:-}"
SERVICE_USER="tentaflow"

if [ "$USER_INSTALL" = "1" ]; then
  PREFIX="${TENTAFLOW_PREFIX:-$HOME/.local/share/tentaflow}"
  CONFIG_DIR="$HOME/.config/tentaflow"
  DATA_DIR="$HOME/.local/share/tentaflow/data"
  BIN_DIR="$HOME/.local/bin"
  SUDO=""
  SERVICE_SCOPE="user"
  SERVICE_USER="$(id -un)"
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

[ "$(uname -s)" = "Linux" ] || die "Ten instalator obsluguje na razie tylko Linux x86_64."
[ "$(uname -m)" = "x86_64" ] || die "Ten instalator obsluguje na razie tylko x86_64 (wykryto: $(uname -m))."

# =============================================================================
# Package manager
# =============================================================================
detect_pm() {
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
  [ "$SKIP_DEPS" = "1" ] && { warn "TENTAFLOW_SKIP_DEPS=1 — pomijam: $*"; return 0; }
  case "$PM" in
    apt)
      $SUDO env DEBIAN_FRONTEND=noninteractive apt-get update -qq
      $SUDO env DEBIAN_FRONTEND=noninteractive apt-get install -y -qq "$@" ;;
    dnf)    $SUDO dnf install -y --quiet "$@" ;;
    pacman) $SUDO pacman -Sy --noconfirm --needed "$@" ;;
    zypper) $SUDO zypper --non-interactive install "$@" ;;
    *)      warn "Nieznany menedzer pakietow — zainstaluj recznie: $*"; return 1 ;;
  esac
}

# =============================================================================
# Which edition
# =============================================================================
# Hardware detection PROPOSES; the user decides. Reading the GPU is not enough:
# an integrated GB10 on a DGX Spark is a real CUDA target and a Strix Halo is a
# real Vulkan one, while the integrated chip in a thin laptop is neither — and
# unified memory means VRAM size does not separate them either.
detect_edition() {
  if command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi -L >/dev/null 2>&1; then
    GPU_DESC="NVIDIA: $(nvidia-smi -L 2>/dev/null | head -1)"
    PROPOSED=full
  elif [ -d /sys/class/drm ] && ls /sys/class/drm 2>/dev/null | grep -q '^card[0-9]'; then
    GPU_DESC="GPU obecne (AMD/Intel — sciezka Vulkan)"
    PROPOSED=full
  else
    GPU_DESC="brak GPU"
    PROPOSED=slim
  fi
}

choose_edition() {
  detect_edition
  if [ -n "$EDITION" ]; then
    ok "Edycja z TENTAFLOW_EDITION: $EDITION"
    return
  fi
  echo ""
  echo "  ${C_BOLD}Wykryto:${C_RESET} $GPU_DESC"
  echo ""
  echo "    ${C_BOLD}full${C_RESET}  llama.cpp (Vulkan), whisper, wizja, TTS  ~161 MB"
  echo "          lokalna inferencja na GPU; potrzebuje GStreamera i Vulkana"
  echo "    ${C_BOLD}slim${C_RESET}  sam gateway: mesh, flow, dashboard        ~60 MB"
  echo "          bez lokalnych silnikow; katalog pokazuje uslugi chmurowe"
  echo "          (OpenAI, Anthropic, ...) i kontenery uzytkowe"
  echo ""
  if [ ! -t 0 ]; then
    EDITION="$PROPOSED"
    warn "Brak terminala (curl | sh) — wybieram '$EDITION'. Wymus przez TENTAFLOW_EDITION=full|slim."
    return
  fi
  printf "  Ktora edycje zainstalowac? [%s]: " "$PROPOSED"
  read -r answer </dev/tty || answer=""
  EDITION="${answer:-$PROPOSED}"
  case "$EDITION" in
    full|slim) ok "Edycja: $EDITION" ;;
    *) die "Nieznana edycja '$EDITION' (dozwolone: full, slim)" ;;
  esac
}

# =============================================================================
# Runtime dependencies
# =============================================================================
install_runtime_deps() {
  [ "$SKIP_DEPS" = "1" ] && { warn "Pomijam zaleznosci systemowe."; return; }

  command -v curl >/dev/null 2>&1 || pm_install curl || die "Brak curl."
  command -v tar  >/dev/null 2>&1 || pm_install tar  || die "Brak tar."

  if [ "$EDITION" = "full" ]; then
    # GStreamer needs its PLUGINS, not just the core library: the camera
    # pipeline builds elements from base/good/bad. libvulkan1 is the loader the
    # binary links against (libvulkan.so.1 in NEEDED).
    log "Instaluje biblioteki runtime (GStreamer + pluginy, Vulkan loader)"
    case "$PM" in
      apt) pm_install libgstreamer1.0-0 gstreamer1.0-plugins-base \
             gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
             gstreamer1.0-libav libvulkan1 libgomp1 || warn "Czesc pakietow sie nie zainstalowala." ;;
      dnf) pm_install gstreamer1 gstreamer1-plugins-base gstreamer1-plugins-good \
             gstreamer1-plugins-bad-free vulkan-loader libgomp || warn "Czesc pakietow sie nie zainstalowala." ;;
      pacman) pm_install gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad \
             vulkan-icd-loader || warn "Czesc pakietow sie nie zainstalowala." ;;
      zypper) pm_install gstreamer gstreamer-plugins-base gstreamer-plugins-good \
             libvulkan1 || warn "Czesc pakietow sie nie zainstalowala." ;;
      *) warn "Nieznany menedzer pakietow — zainstaluj GStreamer + pluginy i loader Vulkana recznie." ;;
    esac
  fi

  if ! command -v docker >/dev/null 2>&1; then
    if [ "$WITH_DOCKER" = "1" ]; then
      log "Instaluje Docker Engine"
      curl -fsSL https://get.docker.com | $SUDO sh || warn "Instalacja Dockera nieudana."
      $SUDO systemctl enable --now docker 2>/dev/null || true
    else
      warn "Docker nie jest zainstalowany — silniki kontenerowe nie beda dostepne."
      warn "Instalator go NIE dokłada bez zgody: uruchom ponownie z TENTAFLOW_WITH_DOCKER=1."
    fi
  fi
}

# =============================================================================
# Download
# =============================================================================
resolve_version() {
  [ "$VERSION" != "latest" ] && return
  log "Ustalam najnowsza wersje"
  # /releases, not /releases/latest: the latter hides pre-releases, and every
  # tag so far carries an -alpha suffix.
  VERSION=$(curl -fsSL "https://api.github.com/repos/$REPO/releases?per_page=10" \
    | grep -m1 '"tag_name"' | sed 's/.*"\(v[^"]*\)".*/\1/')
  [ -n "$VERSION" ] || die "Nie udalo sie ustalic wersji z GitHub API (limit 60 zapytan/h na IP?)."
  ok "Wersja: $VERSION"
}

verify_sha256() {
  archive="$1"; sumfile="$2"
  if command -v sha256sum >/dev/null 2>&1; then
    ( cd "$(dirname "$archive")" && sha256sum -c "$(basename "$sumfile")" >/dev/null ) \
      || die "Suma kontrolna sie nie zgadza — przerywam."
  elif command -v shasum >/dev/null 2>&1; then
    ( cd "$(dirname "$archive")" && shasum -a 256 -c "$(basename "$sumfile")" >/dev/null ) \
      || die "Suma kontrolna sie nie zgadza — przerywam."
  else
    # Silently skipping verification in a `curl | sh` installer is how a
    # tampered archive gets installed, so this is fatal rather than a warning.
    die "Brak sha256sum i shasum — nie moge zweryfikowac archiwum."
  fi
  ok "Suma kontrolna OK"
}

download_archive() {
  TMP=$(mktemp -d)
  trap 'rm -rf "$TMP"' EXIT
  ARCHIVE="$TMP/tentaflow.tar.gz"

  if [ -n "$ASSET_FILE" ]; then
    log "Uzywam lokalnego archiwum: $ASSET_FILE"
    cp "$ASSET_FILE" "$ARCHIVE"
    [ -f "$ASSET_FILE.sha256" ] && cp "$ASSET_FILE.sha256" "$ARCHIVE.sha256"
  else
    resolve_version
    name="tentaflow-${VERSION}-${TARGET}-${EDITION}.tar.gz"
    url="https://github.com/$REPO/releases/download/$VERSION/$name"
    log "Pobieram $name"
    curl -fL --progress-bar "$url" -o "$ARCHIVE" || die "Pobieranie nieudane: $url"
    curl -fsSL "$url.sha256" -o "$ARCHIVE.sha256" || die "Brak pliku .sha256 dla $name."
  fi

  # A checksum file names the archive it was made from, which is never the
  # temp name we just wrote — rewrite the name, keep the digest.
  if [ -f "$ARCHIVE.sha256" ]; then
    digest=$(awk '{print $1; exit}' "$ARCHIVE.sha256")
    printf '%s  %s\n' "$digest" "$(basename "$ARCHIVE")" > "$ARCHIVE.sha256"
  fi
  [ -f "$ARCHIVE.sha256" ] || die "Brak sumy kontrolnej archiwum."
  verify_sha256 "$ARCHIVE" "$ARCHIVE.sha256"
}

# =============================================================================
# Install
# =============================================================================
install_files() {
  log "Rozpakowuje"
  tar -xzf "$ARCHIVE" -C "$TMP"
  inner=$(find "$TMP" -maxdepth 1 -type d -name 'tentaflow-*' | head -1)
  [ -n "$inner" ] || die "Archiwum ma nieoczekiwana strukture."

  # The version comes from the binary itself, so a local archive (CI, offline)
  # lands in a correctly named directory without trusting the file name.
  ver=$("$inner/tentaflow" --version 2>/dev/null | awk '{print $2}')
  [ -n "$ver" ] || die "Nie moge odczytac wersji z binarki."
  VERSION_DIR="$PREFIX/versions/$ver"

  $SUDO mkdir -p "$PREFIX/versions" "$CONFIG_DIR" "$DATA_DIR" "$BIN_DIR"
  $SUDO rm -rf "$VERSION_DIR"
  $SUDO cp -r "$inner" "$VERSION_DIR"

  # Atomic swap: rename over the symlink, never unlink-then-link, so a reader
  # never sees a missing `current`.
  $SUDO ln -sfn "$VERSION_DIR" "$PREFIX/current.new"
  $SUDO mv -T "$PREFIX/current.new" "$PREFIX/current"
  $SUDO ln -sfn "$PREFIX/current/tentaflow" "$BIN_DIR/tentaflow"
  ok "Zainstalowano $ver w $VERSION_DIR"
  INSTALLED_VERSION="$ver"
}

write_config() {
  if [ -f "$CONFIG" ]; then
    ok "Konfiguracja istnieje — nie ruszam: $CONFIG"
    return
  fi
  log "Tworze konfiguracje ($BIND, mesh wylaczony)"
  # The binary owns the config schema; composing TOML here would duplicate it
  # and drift on the first change.
  $SUDO "$PREFIX/current/tentaflow" init-config --output "$CONFIG" --bind "$BIND" --no-mesh
}

write_receipt() {
  variant=slim
  [ "$EDITION" = "full" ] && variant=vulkan
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
  if ! id "$SERVICE_USER" >/dev/null 2>&1; then
    log "Tworze uzytkownika systemowego $SERVICE_USER"
    $SUDO useradd --system --home-dir "$DATA_DIR" --shell /usr/sbin/nologin "$SERVICE_USER" \
      || $SUDO useradd --system --home-dir "$DATA_DIR" --shell /sbin/nologin "$SERVICE_USER" \
      || warn "Nie udalo sie utworzyc uzytkownika — usluga pojdzie jako root."
  fi
  for grp in docker video render; do
    getent group "$grp" >/dev/null 2>&1 && $SUDO usermod -aG "$grp" "$SERVICE_USER" 2>/dev/null || true
  done
  $SUDO chown -R "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR" "$CONFIG_DIR" 2>/dev/null || true
}

register_service() {
  [ "$NO_AUTOSTART" = "1" ] && { warn "Pomijam rejestracje uslugi (TENTAFLOW_NO_AUTOSTART=1)."; return; }
  command -v systemctl >/dev/null 2>&1 || { warn "Brak systemd — uruchom recznie: tentaflow"; return; }

  template="$PREFIX/current/tentaflow.service.in"
  [ -f "$template" ] || die "Brak szablonu unitu w archiwum: $template"

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
    loginctl enable-linger "$(id -un)" 2>/dev/null || warn "loginctl enable-linger nieudane — usluga zatrzyma sie po wylogowaniu."
    ok "Usluga zarejestrowana (systemd --user)"
  else
    printf '%s\n' "$unit_body" | $SUDO tee /etc/systemd/system/tentaflow.service >/dev/null
    $SUDO systemctl daemon-reload
    $SUDO systemctl enable --now tentaflow.service
    ok "Usluga zarejestrowana i wlaczona (systemd)"
  fi
}

harden_platform() {
  [ "$USER_INSTALL" = "1" ] && return
  # Fedora/RHEL: files unpacked into /opt carry no service context, and SELinux
  # can block the JIT mappings wgpu/ggml need.
  if command -v restorecon >/dev/null 2>&1; then
    $SUDO restorecon -R "$PREFIX" 2>/dev/null || true
  fi
  if command -v firewall-cmd >/dev/null 2>&1 && [ "${BIND%%:*}" = "0.0.0.0" ]; then
    port="${BIND##*:}"
    log "Otwieram port $port w firewalld (bind na 0.0.0.0)"
    $SUDO firewall-cmd --permanent --add-port="$port/tcp" >/dev/null 2>&1 || true
    $SUDO firewall-cmd --reload >/dev/null 2>&1 || true
  fi
}

stop_if_running() {
  command -v systemctl >/dev/null 2>&1 || return 0
  if [ "$SERVICE_SCOPE" = "user" ]; then
    systemctl --user is-active --quiet tentaflow.service 2>/dev/null && {
      log "Zatrzymuje dzialajaca usluge przed podmiana"
      systemctl --user stop tentaflow.service
    }
  else
    $SUDO systemctl is-active --quiet tentaflow.service 2>/dev/null && {
      log "Zatrzymuje dzialajaca usluge przed podmiana"
      $SUDO systemctl stop tentaflow.service
    }
  fi
  return 0
}

# =============================================================================
# Run
# =============================================================================
echo ""
echo "${C_BOLD}Instalator TentaFlow${C_RESET}"
detect_pm
echo "${C_DIM}  system:  $(. /etc/os-release 2>/dev/null && echo "$PRETTY_NAME" || uname -s) / $PM${C_RESET}"
echo "${C_DIM}  prefix:  $PREFIX${C_RESET}"
echo "${C_DIM}  dane:    $DATA_DIR${C_RESET}"

choose_edition
install_runtime_deps
download_archive
stop_if_running
install_files
write_config
create_service_user
write_receipt
harden_platform
register_service

echo ""
printf "%s%sGotowe.%s\n" "$C_GREEN" "$C_BOLD" "$C_RESET"
printf "  %sbinarka:%s   $BIN_DIR/tentaflow\n" "$C_DIM" "$C_RESET"
printf "  %swersja:%s    $INSTALLED_VERSION ($EDITION)\n" "$C_DIM" "$C_RESET"
printf "  %sdashboard:%s https://%s\n" "$C_DIM" "$C_RESET" "$BIND"
echo ""
echo "  ${C_BOLD}Pierwsze logowanie: admin / admin${C_RESET} — zmien haslo zaraz po zalogowaniu."
if [ "${BIND%%:*}" = "0.0.0.0" ]; then
  warn "Serwer nasluchuje na wszystkich interfejsach z domyslnym haslem — zmien je TERAZ."
  warn "Certyfikat jest self-signed; dla nazwy domenowej ustaw [server.tls].extra_sans w $CONFIG."
fi
echo ""
echo "  tentaflow status     stan uslugi, autostart, health"
echo "  tentaflow stop|start zatrzymanie / uruchomienie"
echo "  tentaflow update     aktualizacja z GitHub Releases"
echo ""
