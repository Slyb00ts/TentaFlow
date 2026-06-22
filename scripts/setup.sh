#!/usr/bin/env bash
# =============================================================================
# Plik: scripts/setup.sh
# Opis: Instalator zaleznosci do kompilacji TentaFlow.
#       Wykrywa dystrybucje, instaluje wymagane pakiety i opcjonalne GPU SDK.
# =============================================================================
set -euo pipefail

# Kolory
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
BOLD='\033[1m'
NC='\033[0m'

# Flagi GPU. Domyslnie WSZYSTKIE wlaczone — srodowisko deweloperskie buduje
# wariant 'multi' llama.cpp, ktory linkuje CUDA + ROCm + Vulkan naraz, wiec
# runtime'y wszystkich trzech musza byc obecne, inaczej link binarki pada na
# "unable to find library -lvulkan/-lamdhip64/...". Na maszynach bez danego
# GPU instalacja jest nieszkodliwa (na Debian/Ubuntu ROCm bez repo AMD po prostu
# sie pomija z ostrzezeniem). Wylacz pojedynczo: --no-cuda/--no-vulkan/--no-rocm,
# albo wszystkie: --minimal (build CPU-only / per-backend variant).
INSTALL_CUDA=true
INSTALL_VULKAN=true
INSTALL_ROCM=true

# Wykryta dystrybucja
DISTRO=""

# Lista zainstalowanych komponentow (do podsumowania)
INSTALLED=()

# Stan buildu zvec (feature 'vector' jest obowiazkowy) — twarda weryfikacja na koncu
ZVEC_OK=true

# --- Funkcje pomocnicze ---

log_info()    { echo -e "${BLUE}[INFO]${NC} $1"; }
log_ok()      { echo -e "${GREEN}[OK]${NC} $1"; }
log_warn()    { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error()   { echo -e "${RED}[ERROR]${NC} $1"; }
log_section() { echo -e "\n${BOLD}${BLUE}=== $1 ===${NC}\n"; }

usage() {
    cat <<EOF
${BOLD}TentaFlow - instalator zaleznosci${NC}

Uzycie: $0 [OPCJE]

GPU backends sa DOMYSLNIE instalowane (CUDA + Vulkan + ROCm) — srodowisko
deweloperskie buduje wariant 'multi' llama.cpp, ktory linkuje wszystkie trzy.

Opcje:
  --no-cuda     Pomin NVIDIA CUDA toolkit
  --no-vulkan   Pomin Vulkan SDK
  --no-rocm     Pomin AMD ROCm (HIP runtime)
  --minimal     Pomin WSZYSTKIE GPU backends (build CPU-only / per-backend variant)
  --cuda/--vulkan/--rocm/--all-gpu  (zachowane dla zgodnosci — wlaczaja dany backend)
  -h, --help    Pokaz te pomoc

Przyklady:
  $0                  # Baza + wszystkie GPU backends (domyslnie)
  $0 --minimal        # Tylko bazowe zaleznosci (bez GPU)
  $0 --no-rocm        # Baza + CUDA + Vulkan, bez ROCm

Obslugiwane systemy:
  - Arch Linux / CachyOS / Manjaro
  - Ubuntu / Debian / Linux Mint / Pop!_OS
  - Fedora / RHEL / CentOS Stream
  - macOS (Homebrew)
EOF
}

# --- Parsowanie argumentow ---

for arg in "$@"; do
    case $arg in
        --cuda)    INSTALL_CUDA=true ;;
        --vulkan)  INSTALL_VULKAN=true ;;
        --rocm)    INSTALL_ROCM=true ;;
        --all-gpu) INSTALL_CUDA=true; INSTALL_VULKAN=true; INSTALL_ROCM=true ;;
        --no-cuda)   INSTALL_CUDA=false ;;
        --no-vulkan) INSTALL_VULKAN=false ;;
        --no-rocm)   INSTALL_ROCM=false ;;
        --minimal|--cpu) INSTALL_CUDA=false; INSTALL_VULKAN=false; INSTALL_ROCM=false ;;
        --help|-h) usage; exit 0 ;;
        *)
            log_error "Nieznana opcja: $arg"
            usage
            exit 1
            ;;
    esac
done

# --- Sprawdzenie uprawnien ---

check_sudo() {
    # macOS uzywa Homebrew, ktory nie wymaga sudo
    if [[ "$DISTRO" == "macos" ]]; then
        return
    fi

    if [[ $EUID -eq 0 ]]; then
        log_warn "Uruchomiono jako root. Rustup bedzie instalowany dla roota."
    else
        if ! command -v sudo &>/dev/null; then
            log_error "Wymagany jest sudo. Zainstaluj sudo lub uruchom jako root."
            exit 1
        fi
        # Sprawdz czy uzytkownik moze uzyc sudo
        if ! sudo -v 2>/dev/null; then
            log_error "Brak uprawnien sudo."
            exit 1
        fi
    fi
}

# Wrapper: uzyj sudo jesli nie jestesmy rootem
run_privileged() {
    if [[ $EUID -eq 0 ]]; then
        "$@"
    else
        sudo "$@"
    fi
}

ensure_macos_metal_toolchain() {
    # macOS 26 / Xcode 26 wydzielil kompilator Metala jako osobny komponent.
    # Bez niego mlx-swift buduje zepsuty metallib i KAZDY model MLX zwraca
    # belkot (zle logity), bez bledu builda. Instalujemy go raz na maszyne.
    if [[ "$DISTRO" != "macos" ]]; then
        return
    fi
    if xcrun --sdk macosx metal --version &>/dev/null; then
        log_ok "Metal Toolchain juz dostepny"
        return
    fi
    log_warn "Brak Metal Toolchain (macOS 26+) — bez niego modele MLX beda belkotac"
    log_info "Pobieranie Metal Toolchain (~688 MB)..."
    if xcodebuild -downloadComponent MetalToolchain; then
        log_ok "Metal Toolchain zainstalowany"
        INSTALLED+=("Metal Toolchain")
        # Wyczysc stary, zepsuty metallib zeby xcodebuild zbudowal poprawny.
        rm -rf "$(dirname "$0")/../tentaflow-desktop/macos/swift/MLXBridge/build-xcode" 2>/dev/null || true
    else
        log_error "Nie udalo sie pobrac Metal Toolchain — uruchom recznie:"
        log_error "  xcodebuild -downloadComponent MetalToolchain"
    fi
}

configure_macos_gstreamer_pkg_config() {
    if [[ "$DISTRO" != "macos" ]]; then
        return
    fi

    local paths=()
    local runtime_lib_paths=()
    local typelib_paths=()
    local plugin_paths=()
    local scanner_path=""
    local brew_prefix
    brew_prefix=$(brew --prefix 2>/dev/null || true)
    if [[ -n "$brew_prefix" ]]; then
        paths+=("$brew_prefix/lib/pkgconfig" "$brew_prefix/share/pkgconfig")
        runtime_lib_paths+=("$brew_prefix/lib")
        typelib_paths+=("$brew_prefix/lib/girepository-1.0")
        plugin_paths+=("$brew_prefix/lib/gstreamer-1.0")
        if [[ -x "$brew_prefix/libexec/gstreamer-1.0/gst-plugin-scanner" ]]; then
            scanner_path="$brew_prefix/libexec/gstreamer-1.0/gst-plugin-scanner"
        fi
    fi

    local formula_prefix
    for formula in glib gstreamer gst-plugins-base; do
        formula_prefix=$(brew --prefix "$formula" 2>/dev/null || true)
        if [[ -n "$formula_prefix" ]]; then
            paths+=("$formula_prefix/lib/pkgconfig" "$formula_prefix/share/pkgconfig")
        fi
    done

    if [[ -d "/Library/Frameworks/GStreamer.framework/Versions/1.0/lib/pkgconfig" ]]; then
        paths+=("/Library/Frameworks/GStreamer.framework/Versions/1.0/lib/pkgconfig")
        runtime_lib_paths+=("/Library/Frameworks/GStreamer.framework/Versions/1.0/lib")
        typelib_paths+=("/Library/Frameworks/GStreamer.framework/Versions/1.0/lib/girepository-1.0")
        plugin_paths+=("/Library/Frameworks/GStreamer.framework/Versions/1.0/lib/gstreamer-1.0")
        if [[ -x "/Library/Frameworks/GStreamer.framework/Versions/1.0/libexec/gstreamer-1.0/gst-plugin-scanner" ]]; then
            scanner_path="/Library/Frameworks/GStreamer.framework/Versions/1.0/libexec/gstreamer-1.0/gst-plugin-scanner"
        fi
    fi

    local joined=""
    local runtime_lib_joined=""
    local typelib_joined=""
    local plugin_joined=""
    local p
    for p in "${paths[@]}"; do
        [[ -d "$p" ]] || continue
        case ":$joined:" in
            *":$p:"*) ;;
            *) joined="${joined:+$joined:}$p" ;;
        esac
    done
    for p in "${runtime_lib_paths[@]}"; do
        [[ -d "$p" ]] || continue
        case ":$runtime_lib_joined:" in
            *":$p:"*) ;;
            *) runtime_lib_joined="${runtime_lib_joined:+$runtime_lib_joined:}$p" ;;
        esac
    done
    for p in "${typelib_paths[@]}"; do
        [[ -d "$p" ]] || continue
        case ":$typelib_joined:" in
            *":$p:"*) ;;
            *) typelib_joined="${typelib_joined:+$typelib_joined:}$p" ;;
        esac
    done
    for p in "${plugin_paths[@]}"; do
        [[ -d "$p" ]] || continue
        case ":$plugin_joined:" in
            *":$p:"*) ;;
            *) plugin_joined="${plugin_joined:+$plugin_joined:}$p" ;;
        esac
    done

    if [[ -n "$joined" ]]; then
        export PKG_CONFIG_PATH="${joined}${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
        log_ok "PKG_CONFIG_PATH dla GStreamer/macOS: $joined"
        if [[ -n "$runtime_lib_joined" ]]; then
            export DYLD_FALLBACK_LIBRARY_PATH="${runtime_lib_joined}${DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}"
        fi
        if [[ -n "$typelib_joined" ]]; then
            export GI_TYPELIB_PATH="${typelib_joined}${GI_TYPELIB_PATH:+:$GI_TYPELIB_PATH}"
        fi
        if [[ -n "$scanner_path" ]]; then
            export GST_PLUGIN_SCANNER="$scanner_path"
        fi

        local profile_file="$HOME/.zprofile"
        if [[ -n "${SHELL:-}" && "${SHELL##*/}" == "bash" ]]; then
            profile_file="$HOME/.bash_profile"
        fi
        local marker_begin="# >>> tentaflow gstreamer pkg-config >>>"
        local marker_end="# <<< tentaflow gstreamer pkg-config <<<"
        local tmp_file
        tmp_file="$(mktemp)"
        if [[ -f "$profile_file" ]]; then
            awk -v begin="$marker_begin" -v end="$marker_end" '
                $0 == begin { skip = 1; next }
                $0 == end { skip = 0; next }
                !skip { print }
            ' "$profile_file" > "$tmp_file"
        fi
        {
            cat "$tmp_file"
            printf '\n%s\n' "$marker_begin"
            printf 'export PKG_CONFIG_PATH="%s${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"\n' "$joined"
            if [[ -n "$runtime_lib_joined" ]]; then
                printf 'export DYLD_FALLBACK_LIBRARY_PATH="%s${DYLD_FALLBACK_LIBRARY_PATH:+:$DYLD_FALLBACK_LIBRARY_PATH}"\n' "$runtime_lib_joined"
            fi
            if [[ -n "$typelib_joined" ]]; then
                printf 'export GI_TYPELIB_PATH="%s${GI_TYPELIB_PATH:+:$GI_TYPELIB_PATH}"\n' "$typelib_joined"
            fi
            if [[ -n "$scanner_path" ]]; then
                printf 'export GST_PLUGIN_SCANNER="%s"\n' "$scanner_path"
            fi
            printf '%s\n' "$marker_end"
        } > "$profile_file"
        rm -f "$tmp_file"
        log_ok "Zapisano GStreamer PKG_CONFIG_PATH w $profile_file"
    fi
}

# --- Detekcja dystrybucji ---

detect_distro() {
    # macOS (Darwin) — uzywa Homebrew
    if [[ "$(uname -s)" == "Darwin" ]]; then
        DISTRO="macos"
        local mac_version
        mac_version=$(sw_vers -productVersion 2>/dev/null || echo "unknown")
        log_info "Wykryto system: ${BOLD}macOS $mac_version${NC}"
        return
    fi

    if [[ -f /etc/os-release ]]; then
        # shellcheck disable=SC1091
        source /etc/os-release
        case "$ID" in
            arch|cachyos|manjaro|endeavouros|garuda)
                DISTRO="arch"
                ;;
            ubuntu|debian|linuxmint|pop|elementary|zorin)
                DISTRO="debian"
                ;;
            fedora|rhel|centos|rocky|alma)
                DISTRO="fedora"
                ;;
            *)
                # Sprawdz ID_LIKE jako fallback
                case "${ID_LIKE:-}" in
                    *arch*)  DISTRO="arch" ;;
                    *debian*|*ubuntu*) DISTRO="debian" ;;
                    *fedora*|*rhel*)   DISTRO="fedora" ;;
                    *)
                        log_error "Nieobslugiwana dystrybucja: $ID ($PRETTY_NAME)"
                        log_error "Obslugiwane: Arch/CachyOS, Ubuntu/Debian, Fedora"
                        exit 1
                        ;;
                esac
                ;;
        esac
        log_info "Wykryto dystrybucje: ${BOLD}$PRETTY_NAME${NC} (rodzina: $DISTRO)"
    else
        log_error "Nie mozna wykryc dystrybucji (/etc/os-release nie istnieje)"
        exit 1
    fi
}

# --- Instalacja bazowych zaleznosci ---

install_base() {
    log_section "Instalacja bazowych zaleznosci"

    case "$DISTRO" in
        arch)
            log_info "Aktualizacja bazy pakietow pacman..."
            run_privileged pacman -Sy --noconfirm

            local pkgs=(
                base-devel
                cmake
                clang
                lld
                git
                git-lfs
                jdk17-openjdk
                unzip
                pkg-config
                glib2
                gstreamer
                gst-plugins-base-libs
                openssl
                vulkan-icd-loader
                sqlite
                # ALSA — sherpa-rs linkuje -lasound (audio/portaudio).
                alsa-lib
                # Profiling: perf zbiera CPU samples + PMU counters + uncore IMC.
                # which jest potrzebne dla collectors/permissions auto-discovery.
                perf
                which
                # iostat dla disk IO collector (/usr/bin/iostat).
                sysstat
            )
            log_info "Instalacja: ${pkgs[*]}"
            run_privileged pacman -S --needed --noconfirm "${pkgs[@]}"

            # protoc (prost-build, feature vector-milvus) to narzedzie build-time.
            # Na rolling-release repo'wy protobuf moze wyprzedzac wersje wymagana
            # przez AUR python-protobuf (protobuf=34.1) — wymuszenie upgrade'u przez
            # --needed wywala transakcje. Instalujemy tylko gdy protoc faktycznie
            # brakuje, zamiast bumpic juz dzialajaca wersje.
            if ! command -v protoc &>/dev/null; then
                run_privileged pacman -S --needed --noconfirm protobuf
                INSTALLED+=("protobuf")
            fi
            INSTALLED+=("base-devel" "cmake" "clang" "lld" "git" "git-lfs" "jdk17-openjdk" "unzip" "glib2" "gstreamer" "gst-plugins-base-libs" "vulkan-loader" "sqlite" "perf" "sysstat")
            ;;
        debian)
            log_info "Aktualizacja listy pakietow apt..."
            run_privileged apt-get update -qq

            local pkgs=(
                build-essential
                cmake
                clang
                lld
                git
                git-lfs
                openjdk-17-jdk
                unzip
                pkg-config
                libglib2.0-dev
                libgstreamer1.0-dev
                libgstreamer-plugins-base1.0-dev
                libssl-dev
                libvulkan1
                libsqlite3-dev
                # ALSA — sherpa-rs (audio/portaudio) linkuje -lasound; bez tego
                # link sherpa-rs pada na "cannot find -lasound".
                libasound2-dev
                protobuf-compiler
                # Profiling: linux-tools dostarcza perf, sysstat dostarcza iostat.
                # linux-tools-generic to meta-package ktory dociaga linux-tools-<kernel>
                # pasujace do biezacego kernela (Ubuntu 24.04+).
                linux-tools-common
                linux-tools-generic
                sysstat
                libclang-dev
                patchelf
            )
            log_info "Instalacja: ${pkgs[*]}"
            run_privileged apt-get install -y "${pkgs[@]}"
            INSTALLED+=("build-essential" "cmake" "clang" "lld" "git" "git-lfs" "openjdk-17-jdk" "unzip" "libglib2.0-dev" "libgstreamer1.0-dev" "libgstreamer-plugins-base1.0-dev" "libvulkan1" "sqlite3-dev" "perf" "sysstat" "libclang-dev" "patchelf")
            ;;
        fedora)
            local pkgs=(
                gcc
                gcc-c++
                make
                cmake
                clang
                lld
                git
                git-lfs
                # System JDK, only needed to build the Android APK (Gradle). The
                # repo-local gradlew bootstrap downloads its own Temurin 17 when
                # the system Java is too new, so the exact version here doesn't
                # matter — `java-latest-openjdk-devel` always exists across Fedora
                # releases (F44 already dropped java-17/21), and --skip-unavailable
                # below keeps setup resilient anyway.
                java-latest-openjdk-devel
                unzip
                pkg-config
                # sherpa-onnx (Eigen/openfst) buduje z -static-libstdc++ -static-libgcc;
                # na Fedorze static libstdc++ to osobny pakiet. Bez niego KAZDY test
                # kompilatora CMake pada (link bez -lstdc++), a Eigen konczy na
                # "Can't link to the standard math library". glibc-static dla -static.
                libstdc++-static
                glibc-static
                glib2-devel
                gstreamer1-devel
                gstreamer1-plugins-base-devel
                openssl-devel
                vulkan-loader
                sqlite-devel
                # ALSA — sherpa-rs linkuje -lasound (audio/portaudio).
                alsa-lib-devel
                protobuf-compiler
                # Profiling: perf jest w pakiecie 'perf' na Fedora 38+.
                # sysstat dostarcza iostat dla linux.iostat.disk collector.
                perf
                sysstat
            )
            log_info "Instalacja: ${pkgs[*]}"
            # --skip-unavailable: nie przerywaj calej transakcji gdy jeden pakiet
            # zniknal w danej wersji Fedory (np. java-17 na F44) — reszta wchodzi.
            run_privileged dnf install -y --skip-unavailable "${pkgs[@]}"
            INSTALLED+=("gcc/g++" "libstdc++-static" "glibc-static" "cmake" "clang" "lld" "git" "git-lfs" "java-latest-openjdk-devel" "unzip" "glib2-devel" "gstreamer1-devel" "gstreamer1-plugins-base-devel" "vulkan-loader" "sqlite-devel" "perf" "sysstat")
            ;;
        macos)
            if ! command -v brew &>/dev/null; then
                log_error "Homebrew nie jest zainstalowany. Zainstaluj go najpierw:"
                log_error '  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"'
                exit 1
            fi

            log_info "Aktualizacja Homebrew..."
            brew update

            local pkgs=(
                cmake
                ninja
                llvm
                git
                git-lfs
                openjdk@17
                unzip
                pkg-config
                glib
                gstreamer
                gst-plugins-base
                openssl@3
                sqlite
                protobuf
            )
            log_info "Instalacja: ${pkgs[*]}"
            brew install "${pkgs[@]}"
            configure_macos_gstreamer_pkg_config
            ensure_macos_metal_toolchain
            INSTALLED+=("cmake" "ninja" "llvm (clang+lld)" "git" "git-lfs" "openjdk@17" "unzip" "pkg-config" "glib" "gstreamer" "gst-plugins-base" "openssl@3" "sqlite")
            ;;
    esac

    log_ok "Bazowe zaleznosci zainstalowane"
}

install_git_lfs() {
    log_section "Git LFS"

    if ! command -v git-lfs &>/dev/null && ! git lfs version &>/dev/null; then
        log_error "Git LFS nie jest dostepny mimo instalacji pakietow bazowych."
        log_error "Zainstaluj recznie pakiet git-lfs dla swojego systemu i uruchom setup ponownie."
        exit 1
    fi

    git lfs install
    log_ok "Git LFS: $(git lfs version)"
    INSTALLED+=("git lfs install")

    # Materializuj artefakty LFS (prebuilt native-libs/*.a|*.so). Jesli repo
    # sklonowano ZANIM git-lfs byl w systemie, te pliki sa tekstowymi pointerami
    # — `git lfs install` rejestruje tylko filtry i NIE sciaga juz
    # wyewidencjonowanych pointerow. Bez `git lfs pull` build.rs pada na
    # "brak native-libs" (linkuje pointer zamiast biblioteki).
    local repo_root
    repo_root=$(git rev-parse --show-toplevel 2>/dev/null || true)
    if [[ -n "$repo_root" ]]; then
        log_info "Pobieranie artefaktow Git LFS (prebuilt native-libs)..."
        if git -C "$repo_root" lfs pull; then
            log_ok "Git LFS: artefakty pobrane"
            INSTALLED+=("git lfs pull")
        else
            log_warn "git lfs pull nieudane — uruchom recznie w repo: git lfs pull"
        fi
    fi
}

# --- Docker (wymagany przez TentaFlow runtime + build zvec) ---
#
# TentaFlow w runtime uruchamia kontenery z silnikami AI przez bollard
# (services/deploy/docker.rs), wiec biezacy user MUSI miec dostep do socketu
# dockera bez sudo — inaczej deploy silnikow pada na "permission denied
# /var/run/docker.sock". Build zvec (kontener gcc-11) tez tego potrzebuje.
# Dlatego instalujemy Docker, startujemy daemon i dodajemy usera do grupy
# 'docker' ZAWSZE (nie tylko gdy zvec idzie przez kontener).
ensure_docker() {
    log_section "Docker"

    if [[ "$DISTRO" == "macos" ]]; then
        if command -v docker &>/dev/null; then
            log_ok "Docker: $(docker --version 2>/dev/null)"
        else
            log_warn "Docker nie znaleziony — zainstaluj Docker Desktop recznie: https://www.docker.com/products/docker-desktop"
        fi
        return
    fi

    if ! command -v docker &>/dev/null; then
        log_info "Instaluje Docker (TentaFlow uruchamia kontenery z silnikami AI)..."
        case "$DISTRO" in
            arch)   run_privileged pacman -S --needed --noconfirm docker ;;
            debian) run_privileged apt-get install -y docker.io ;;
            fedora) run_privileged dnf install -y docker || run_privileged dnf install -y moby-engine ;;
        esac
    fi
    if ! command -v docker &>/dev/null; then
        log_error "Nie udalo sie zainstalowac Dockera — zainstaluj recznie i uruchom setup ponownie."
        return
    fi
    log_ok "Docker: $(docker --version 2>/dev/null)"

    # Daemon (systemd)
    if command -v systemctl &>/dev/null; then
        run_privileged systemctl enable --now docker 2>/dev/null || true
    fi

    # Grupa 'docker' dla usera — bez tego ani build, ani TentaFlow w runtime nie
    # siegna socketu bez sudo. Przy `sudo ./setup.sh` realnym userem jest
    # $SUDO_USER, nie root.
    local docker_user="${SUDO_USER:-$(id -un)}"
    if [[ -n "$docker_user" && "$docker_user" != "root" ]]; then
        if id -nG "$docker_user" 2>/dev/null | tr ' ' '\n' | grep -qx docker; then
            log_ok "Uzytkownik '$docker_user' jest juz w grupie 'docker'."
        else
            run_privileged groupadd -f docker 2>/dev/null || true
            if run_privileged usermod -aG docker "$docker_user" 2>/dev/null; then
                log_warn "Dodano '$docker_user' do grupy 'docker'."
                log_warn "  WAZNE: wyloguj sie i zaloguj ponownie (lub 'newgrp docker') — inaczej ani"
                log_warn "  build, ani TentaFlow runtime nie siegna socketu dockera bez sudo."
                NEED_DOCKER_RELOGIN=true
                INSTALLED+=("usermod -aG docker $docker_user")
            fi
        fi
    fi

    # Weryfikacja dostepu w biezacej sesji
    if docker info &>/dev/null; then
        log_ok "Docker dostepny dla biezacej sesji (bez sudo)."
    elif run_privileged docker info &>/dev/null; then
        log_warn "Docker dziala, ale ta sesja nie ma jeszcze dostepu — wymagany re-login/'newgrp docker'."
    else
        log_warn "Docker zainstalowany, ale daemon nie odpowiada — sprawdz: sudo systemctl status docker"
    fi
}

# --- zvec (wbudowana baza wektorowa — statyczny artefakt per platforma) ---

install_zvec() {
    log_section "zvec (wbudowana baza wektorowa)"

    # Mapowanie hosta na zvendorowany artefakt tentaflow-zvec-sys.
    local plat
    if [[ "$DISTRO" == "macos" ]]; then
        plat="macos-arm64"
    elif [[ "$(uname -m)" == "aarch64" || "$(uname -m)" == "arm64" ]]; then
        plat="linux-aarch64"
    else
        plat="linux-x86_64"
    fi

    # Desktop links the shared lib; mobile would use a static archive.
    local artifact="libzvec_c_api.so"
    [[ "$DISTRO" == "macos" ]] && artifact="libzvec_c_api.dylib"
    local lib="$(dirname "$0")/../tentaflow-zvec-sys/vendor/lib/$plat/$artifact"
    if [[ -f "$lib" ]]; then
        log_ok "zvec juz zbudowany ($plat) — pomijam"
        return
    fi

    # Linux: RocksDB 8.1 (zaleznosc zvec) nie kompiluje sie pod gcc>=13, wiec build
    # leci w kontenerze Ubuntu 22.04/gcc-11 — wymaga Dockera. Docker musi byc
    # zainstalowany ORAZ dostepny dla biezacego usera (socket). Wczesniej setup
    # sprawdzal tylko "czy docker jest w PATH" — gdy byl zainstalowany, ale user
    # nie nalezal do grupy docker, leciał build bez sudo i po cichu padał.
    #
    # Preferujemy NATYWNY build (gcc-11 + ninja + cmake<4) — bez Dockera, bez
    # root-owned artefaktow. RocksDB 8.1 nie kompiluje sie pod gcc>=13, a host
    # ma zwykle nowszy gcc, wiec doinstalowujemy gcc-11 obok (apt: Debian/Ubuntu).
    # Docker zostaje fallbackiem gdy natywny toolchain nie wchodzi (Arch/Fedora,
    # cmake>=4 itd.). need_sudo=true tylko dla fallbacku, gdy user nie siega
    # socketu dockera wprost (build leci przez sudo, artefakty root-owned).
    local need_sudo=false
    if [[ "$DISTRO" != "macos" ]]; then
        if [[ "$DISTRO" == "debian" ]]; then
            log_info "Instaluje natywny toolchain zvec (gcc-11, g++-11, ninja)..."
            run_privileged apt-get install -y gcc-11 g++-11 ninja-build >/dev/null 2>&1 || true
        fi

        local cmake_major
        cmake_major="$(cmake --version 2>/dev/null | sed -n '1s/.*version \([0-9][0-9]*\).*/\1/p')"
        if command -v gcc-11 &>/dev/null && command -v g++-11 &>/dev/null \
           && command -v ninja &>/dev/null \
           && [[ -n "$cmake_major" && "$cmake_major" -lt 4 ]]; then
            log_ok "Natywny gcc-11 gotowy — zvec zbuduje sie bez Dockera."
        else
            log_info "Natywny gcc-11<13/ninja/cmake<4 niedostepny — uzyje Dockera (Ubuntu 22.04/gcc-11)."
            if ! command -v docker &>/dev/null; then
                log_info "Instaluje Docker..."
                case "$DISTRO" in
                    arch)   run_privileged pacman -S --needed --noconfirm docker ;;
                    debian) run_privileged apt-get install -y docker.io ;;
                    fedora) run_privileged dnf install -y docker || run_privileged dnf install -y moby-engine ;;
                esac
            fi
            run_privileged systemctl enable --now docker 2>/dev/null || true
            if docker info &>/dev/null; then
                : # biezacy user ma dostep do socketu
            elif run_privileged docker info &>/dev/null; then
                # Daemon dziala, ale biezacy user nie ma dostepu do socketu
                # (nie nalezy do grupy 'docker') — stad "permission denied ...
                # /var/run/docker.sock". Dodajemy go do grupy, zeby docker
                # dzialal BEZ sudo przy kolejnych uruchomieniach (np. bezposrednie
                # ./scripts/native-libs/build-all.sh). Czlonkostwo grupy wchodzi
                # w zycie dopiero po re-loginie, wiec TEN build leci jeszcze przez
                # sudo (need_sudo). Przy `sudo ./setup.sh` realnym userem jest
                # $SUDO_USER, nie root.
                local docker_user="${SUDO_USER:-$(id -un)}"
                if [[ -n "$docker_user" && "$docker_user" != "root" ]]; then
                    run_privileged groupadd -f docker 2>/dev/null || true
                    if run_privileged usermod -aG docker "$docker_user" 2>/dev/null; then
                        log_warn "Dodano '$docker_user' do grupy 'docker' — zadziala po WYLOGOWANIU i ponownym zalogowaniu (albo od razu w tej sesji: 'newgrp docker')."
                        INSTALLED+=("usermod -aG docker $docker_user")
                    fi
                fi
                need_sudo=true
            else
                log_error "Ani natywny gcc-11, ani Docker (nawet przez sudo) nie sa dostepne."
                log_error "  Doinstaluj gcc-11/ninja albo uruchom daemon dockera, potem: ./scripts/build-zvec.sh $plat"
                ZVEC_OK=false
                return
            fi
        fi
    fi

    log_info "Buduje zvec ($plat) — dlugi build (RocksDB+Arrow), jednorazowo na maszyne..."
    local build_sh="$(dirname "$0")/build-zvec.sh"
    local built=false
    # ZAWSZE jako biezacy user — NIE przez sudo na cały skrypt. Inaczej
    # mkdir/cp do tentaflow-zvec-sys/vendor/lib/ tworzylo pliki root-owned i
    # pozniejszy user-owy build (np. ./scripts/native-libs/build-all.sh) padal na
    # "cp: Permission denied". build-zvec.sh sam siega po 'sudo docker' tylko dla
    # kontenera i chown-uje jego output — reszta (mkdir/cp/git) leci jako user.
    # (need_sudo zostaje wyliczone wyzej tylko do diagnostyki/komunikatu.)
    bash "$build_sh" "$plat" && built=true
    if [[ "$built" == true ]]; then
        INSTALLED+=("zvec ($plat)")
    else
        log_error "Build zvec nieudany — sprobuj recznie: ./scripts/build-zvec.sh $plat"
        ZVEC_OK=false
    fi
}

# --- zvec dla iOS (statyczne archiwa do buildu tentaflow-mobile/ios) ---

# Binarka tentaflow-mobile linkuje dwa statyczne archiwa zvec (libzvec_c_api.a +
# libzvec_deps.a), bo appka iOS nie moze wozic luznego .dylib. Vector jest
# OBOWIAZKOWY (nie feature), wiec build.rs wymaga archiwum dla KAZDEGO targetu iOS,
# ktory budujemy: device (ios-arm64) ORAZ symulator (ios-sim-arm64) — bez sim build
# tentaflow-mobile na symulator panikuje w build.rs. Cross-build leci na hoscie
# macOS przez odpowiednie SDK (iphoneos / iphonesimulator) — patrz build-zvec.sh.
install_zvec_ios() {
    if [[ "$DISTRO" != "macos" ]]; then
        return
    fi
    log_section "zvec dla iOS (device + symulator)"

    local build_sh="$(dirname "$0")/build-zvec.sh"
    local plat
    for plat in ios-arm64 ios-sim-arm64; do
        local lib_dir="$(dirname "$0")/../tentaflow-zvec-sys/vendor/lib/$plat"
        if [[ -f "$lib_dir/libzvec_c_api.a" && -f "$lib_dir/libzvec_deps.a" ]]; then
            log_ok "zvec juz zbudowany ($plat) — pomijam"
            continue
        fi
        log_info "Buduje zvec ($plat) — cross-build na iOS SDK (RocksDB+Arrow+protoc), jednorazowo..."
        if bash "$build_sh" "$plat"; then
            INSTALLED+=("zvec ($plat)")
        else
            log_error "Build zvec ($plat) nieudany — sprobuj recznie: ./scripts/build-zvec.sh $plat"
            ZVEC_OK=false
        fi
    done
}

# --- Rust toolchain ---

install_rust() {
    log_section "Rust toolchain"

    if command -v rustup &>/dev/null; then
        log_ok "rustup juz zainstalowany: $(rustup --version 2>/dev/null)"
        log_info "Aktualizacja toolchaina..."
        rustup update stable --no-self-update
    else
        log_info "Instalacja rustup..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
        # Zaladuj srodowisko cargo
        # shellcheck disable=SC1091
        source "$HOME/.cargo/env"
        INSTALLED+=("rustup + stable toolchain")
    fi

    # Upewnij sie ze mamy stable
    rustup default stable

    # rustup shim MUSI byc na PATH przed ewentualnym systemowym cargo (apt/distro
    # potrafi miec stare 1.75) — inaczej build uzyje starego cargo mimo rustupa.
    export PATH="$HOME/.cargo/bin:$PATH"

    # Wymus minimalna wersje: zaleznosci wymagaja feature `edition2024`, ktora
    # ustabilizowano dopiero w Rust 1.85. Stary toolchain (np. systemowy 1.75)
    # wywala build z "feature `edition2024` is required". Fail-loud z instrukcja.
    local min_rust="1.85.0"
    local cur_rust
    cur_rust="$(rustc --version 2>/dev/null | awk '{print $2}')"
    if [[ -z "$cur_rust" ]] || \
       [[ "$(printf '%s\n%s\n' "$min_rust" "$cur_rust" | sort -V | head -1)" != "$min_rust" ]]; then
        log_warn "Aktywny rustc=${cur_rust:-brak} < ${min_rust}; probuje rustup update stable..."
        rustup update stable --no-self-update
        rustup default stable
        cur_rust="$(rustc --version 2>/dev/null | awk '{print $2}')"
    fi
    if [[ -z "$cur_rust" ]] || \
       [[ "$(printf '%s\n%s\n' "$min_rust" "$cur_rust" | sort -V | head -1)" != "$min_rust" ]]; then
        log_error "Rust ${cur_rust:-brak} jest za stary (wymagane >= ${min_rust} dla edition2024)."
        log_error "Aktywny cargo: $(command -v cargo 2>/dev/null || echo brak)"
        log_error "Jesli to systemowy Rust (apt/distro): usun go (np. 'apt remove rustc cargo') albo upewnij sie ze ~/.cargo/bin jest PIERWSZE w PATH, i uruchom ponownie."
        exit 1
    fi

    log_ok "Rust: $(rustc --version)"
    INSTALLED+=("rust-stable")
}

# --- WASM targets ---

install_wasm_target() {
    log_section "WASM targets (wasm32-wasip1 + wasm32-unknown-unknown)"

    # wasm32-wasip1 — dla addonow (Wasmtime/wasmi sandbox)
    if rustup target list --installed | grep -q "wasm32-wasip1"; then
        log_ok "wasm32-wasip1 juz zainstalowany"
    else
        log_info "Dodawanie targetu wasm32-wasip1..."
        rustup target add wasm32-wasip1
        INSTALLED+=("wasm32-wasip1")
    fi

    # wasm32-unknown-unknown — dla tentaflow-protocol-wasm (browser glue)
    if rustup target list --installed | grep -q "wasm32-unknown-unknown"; then
        log_ok "wasm32-unknown-unknown juz zainstalowany"
    else
        log_info "Dodawanie targetu wasm32-unknown-unknown..."
        rustup target add wasm32-unknown-unknown
        INSTALLED+=("wasm32-unknown-unknown")
    fi

    log_ok "WASM targets gotowe"
}

# --- wasm-bindgen CLI ---

# Wersja MUSI byc zgodna z dependency w tentaflow-protocol-wasm/Cargo.toml
# oraz z hardkodowana wartoscia w tentaflow-core/build.rs (funkcja
# build_protocol_wasm_bindings). Bez tego narzedzia GUI nie dostanie
# plikow www/js/protocol/wasm_glue.{js,wasm} i codec.js rzuci ImportError.
WASM_BINDGEN_VERSION="0.2.120"

install_wasm_bindgen_cli() {
    log_section "wasm-bindgen CLI (v${WASM_BINDGEN_VERSION})"

    if command -v wasm-bindgen &>/dev/null; then
        local current
        current=$(wasm-bindgen --version 2>/dev/null | awk '{print $2}')
        if [[ "$current" == "$WASM_BINDGEN_VERSION" ]]; then
            log_ok "wasm-bindgen $current juz zainstalowany"
            return
        else
            log_warn "wasm-bindgen $current != wymagana $WASM_BINDGEN_VERSION — reinstaluje"
        fi
    fi

    log_info "Kompilacja wasm-bindgen-cli (moze potrwac kilka minut)..."
    cargo install wasm-bindgen-cli --version "$WASM_BINDGEN_VERSION" --locked
    INSTALLED+=("wasm-bindgen-cli ${WASM_BINDGEN_VERSION}")

    log_ok "wasm-bindgen CLI gotowy"
}

# --- Android Rust/mobile toolchain ---

install_android_rust_tools() {
    log_section "Android Rust toolchain"

    local target
    for target in aarch64-linux-android armv7-linux-androideabi x86_64-linux-android; do
        if rustup target list --installed | grep -q "^$target$"; then
            log_ok "$target juz zainstalowany"
        else
            log_info "Dodawanie targetu $target..."
            rustup target add "$target"
            INSTALLED+=("$target")
        fi
    done

    if command -v cargo-ndk &>/dev/null; then
        log_ok "cargo-ndk juz zainstalowany"
    else
        log_info "Instalacja cargo-ndk..."
        cargo install cargo-ndk
        INSTALLED+=("cargo-ndk")
    fi
}

# --- iOS targets (macOS only) ---

install_ios_targets() {
    # Targety iOS maja sens tylko na macOS (wymagaja Xcode CLT + SDK).
    if [[ "$DISTRO" != "macos" ]]; then
        return
    fi

    log_section "iOS targety (aarch64-apple-ios + aarch64-apple-ios-sim)"

    if ! xcode-select -p &>/dev/null; then
        log_warn "Xcode Command Line Tools niezainstalowane — pomijam iOS targety."
        log_warn "Zainstaluj recznie: xcode-select --install"
        return
    fi

    for t in aarch64-apple-ios aarch64-apple-ios-sim; do
        if rustup target list --installed | grep -q "^$t$"; then
            log_ok "$t juz zainstalowany"
        else
            log_info "Dodawanie targetu $t..."
            rustup target add "$t"
            INSTALLED+=("$t")
        fi
    done

    log_ok "iOS targety gotowe"
}

# --- Pelne Xcode (macOS only) ---
#
# tentaflow/build.rs wywoluje `xcodebuild build -scheme MLXBridge` na
# tentaflow-desktop/macos/swift/MLXBridge zeby zbudowac libMLXBridge.dylib +
# default.metallib (Metal shadery dla mlx-swift). To wymaga PELNEGO Xcode
# (Xcode.app), nie wystarczy `xcode-select --install` (CLT).
# Bez tego build dziala, ale mlx-swift bridge sie nie buduje i MLX modele
# (Bielik, Qwen) startuja z bledem "Failed to load default metallib".
require_full_xcode() {
    if [[ "$DISTRO" != "macos" ]]; then
        return
    fi

    log_section "Pelne Xcode (wymagane do budowy libMLXBridge.dylib)"

    if ! command -v xcodebuild &>/dev/null; then
        log_warn "Brak xcodebuild — masz tylko Command Line Tools."
        log_warn "Pobierz Xcode z App Store (free): https://apps.apple.com/pl/app/xcode/id497799835"
        log_warn "Po instalacji przelacz toolchain:"
        log_warn "  sudo xcode-select -s /Applications/Xcode.app/Contents/Developer"
        log_warn "Bez Xcode build skonczy sie OK, ale MLX modele nie zadzialaja."
        return
    fi

    local xcode_dev
    xcode_dev=$(xcode-select -p 2>/dev/null || echo "")
    if [[ "$xcode_dev" != *"Xcode.app"* ]]; then
        log_warn "Aktywne Developer Dir: $xcode_dev (to nie pelne Xcode)"
        log_warn "Przelacz: sudo xcode-select -s /Applications/Xcode.app/Contents/Developer"
        log_warn "Pomijam dalsze sprawdzenia Xcode."
        return
    fi

    # `|| true` chroni przed pipefail — gdy xcodebuild -version padnie
    # (np. nieakceptowana licencja albo brak iOS SDK), `set -e` cicho wybijal
    # caly setup.sh w tym miejscu bez zadnego komunikatu dla uzytkownika.
    local xcv
    xcv=$(xcodebuild -version 2>/dev/null | head -1 || true)
    if [[ -n "$xcv" ]]; then
        log_ok "Xcode: $xcv"
    else
        log_warn "xcodebuild dostepny, ale -version padlo. Sprawdz licencje: sudo xcodebuild -license"
    fi
}

# --- Metal Toolchain (macOS only) ---
#
# Xcode 16 wydzielil Metal Toolchain jako osobny komponent. Bez niego
# `xcodebuild` na Swift Package z .metal sourcami nie zbuduje default.metallib
# (mlx-swift kompiluje shadery shadery przy build framework'a). Bez metallib
# MLX startuje z bledem "Failed to load default metallib" i modele zwracaja
# bełkot lub w ogole nie startuja.
install_metal_toolchain() {
    if [[ "$DISTRO" != "macos" ]]; then
        return
    fi

    log_section "Xcode Metal Toolchain"

    if ! xcode-select -p &>/dev/null; then
        log_warn "Xcode Command Line Tools niezainstalowane — pomijam Metal Toolchain."
        log_warn "Zainstaluj recznie: xcode-select --install  (lub Xcode z App Store)"
        return
    fi

    # Narzedzie 'metal' dzialajace = toolchain jest pobrany.
    if xcrun metal --version &>/dev/null; then
        log_ok "Metal Toolchain juz zainstalowany"
        return
    fi

    # xcodebuild -downloadComponent wymaga pelnego Xcode (nie samego CLT).
    local xcode_dev
    xcode_dev=$(xcode-select -p)
    if [[ "$xcode_dev" != *"Xcode.app"* ]]; then
        log_warn "Aktywne Developer Dir: $xcode_dev"
        log_warn "Metal Toolchain wymaga pelnego Xcode. Zainstaluj Xcode i przelacz:"
        log_warn "  sudo xcode-select -s /Applications/Xcode.app/Contents/Developer"
        log_warn "Potem uruchom: xcodebuild -downloadComponent MetalToolchain"
        return
    fi

    log_info "Metal Toolchain brakuje. Pobieram komponent (moze potrwac, kilkaset MB)..."
    if xcodebuild -downloadComponent MetalToolchain; then
        if xcrun metal --version &>/dev/null; then
            log_ok "Metal Toolchain zainstalowany"
            INSTALLED+=("metal-toolchain")
        else
            log_warn "Pobranie zakonczone, ale 'xcrun metal' wciaz nie dziala."
            log_warn "Sprobuj recznie: xcodebuild -downloadComponent MetalToolchain"
        fi
    else
        log_warn "Nie udalo sie pobrac Metal Toolchain."
        log_warn "Uruchom recznie: xcodebuild -downloadComponent MetalToolchain"
    fi
}

# --- iOS Platform (macOS only) ---

# Xcode 16 traktuje platformy (iOS, watchOS, tvOS) jako osobne komponenty.
# Bez pobranej platformy iOS brakuje m.in. libclang_rt.ios.a, co wywala
# linker przy buildzie tentaflow-mobile/ios. Pobranie jest spore (~5-8 GB),
# ale wymagane do kompilacji i symulatora iOS.
install_ios_platform() {
    if [[ "$DISTRO" != "macos" ]]; then
        return
    fi

    log_section "Xcode iOS Platform (libclang_rt.ios.a + SDK)"

    if ! xcode-select -p &>/dev/null; then
        log_warn "Xcode Command Line Tools niezainstalowane — pomijam iOS platform."
        return
    fi

    local xcode_dev
    xcode_dev=$(xcode-select -p)
    if [[ "$xcode_dev" != *"Xcode.app"* ]]; then
        log_warn "Aktywne Developer Dir: $xcode_dev"
        log_warn "iOS Platform wymaga pelnego Xcode. Przelacz:"
        log_warn "  sudo xcode-select -s /Applications/Xcode.app/Contents/Developer"
        log_warn "Potem uruchom: xcodebuild -downloadPlatform iOS"
        return
    fi

    # Sprawdzamy faktyczna obecnosc iOS runtime w aktywnym toolchain.
    # libclang_rt.ios.a siedzi pod ../usr/lib/clang/<ver>/lib/darwin/ —
    # numer wersji clang sie zmienia, wiec find/glob.
    local rt_lib
    rt_lib=$(find "$xcode_dev/Toolchains/XcodeDefault.xctoolchain/usr/lib/clang" \
        -name "libclang_rt.ios.a" 2>/dev/null | head -n1 || true)

    if [[ -n "$rt_lib" ]] && xcrun --show-sdk-path --sdk iphoneos &>/dev/null; then
        log_ok "iOS Platform juz zainstalowana"
        return
    fi

    log_info "iOS Platform brakuje. Pobieram (moze potrwac, ~5-8 GB)..."
    if xcodebuild -downloadPlatform iOS; then
        rt_lib=$(find "$xcode_dev/Toolchains/XcodeDefault.xctoolchain/usr/lib/clang" \
            -name "libclang_rt.ios.a" 2>/dev/null | head -n1)
        if [[ -n "$rt_lib" ]]; then
            log_ok "iOS Platform zainstalowana"
            INSTALLED+=("ios-platform")
        else
            log_warn "Pobranie zakonczone, ale libclang_rt.ios.a wciaz nie widoczne."
            log_warn "Sprobuj: sudo xcodebuild -runFirstLaunch"
        fi
    else
        log_warn "Nie udalo sie pobrac iOS Platform."
        log_warn "Uruchom recznie: xcodebuild -downloadPlatform iOS"
    fi
}

# --- GStreamer iOS xcframework (macOS only) ---

# Apka iOS linkuje sie z combined libGStreamer.a (camera feature w
# tentaflow-core). Upstream dystrybuuje gotowy xcframework jako tar.xz —
# pobieramy do repo-local Frameworks/ (gitignored) i generujemy syntetyczny
# pkg-config zeby gstreamer-rs/cargo dla aarch64-apple-ios mogl linkowac.
install_ios_gstreamer_xcframework() {
    if [[ "$DISTRO" != "macos" ]]; then
        return
    fi

    log_section "GStreamer iOS xcframework (camera feature)"

    local gst_version="1.28.3"
    local archive_url="https://gstreamer.freedesktop.org/data/pkg/ios/${gst_version}/gstreamer-${gst_version}-xcframework.tar.xz"
    local repo_root
    repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
    local target_dir="$repo_root/tentaflow-mobile/ios/Frameworks"
    local xcframework_dir="$target_dir/GStreamer.xcframework"
    local pkgconfig_dir="$target_dir/pkgconfig"
    local marker="$xcframework_dir/ios-arm64/libGStreamer.a"

    if [[ -f "$marker" ]]; then
        log_ok "GStreamer iOS xcframework juz pobrany: $xcframework_dir"
    else
        log_info "Pobieranie GStreamer iOS xcframework ${gst_version} (~600 MB)..."
        mkdir -p "$target_dir"
        local tmp_archive
        tmp_archive="$(mktemp -t gstios.XXXXXX).tar.xz"
        if ! curl -fL --progress-bar -o "$tmp_archive" "$archive_url"; then
            log_error "Nie udalo sie pobrac $archive_url"
            rm -f "$tmp_archive"
            return 1
        fi
        log_info "Wypakowuje xcframework (moze potrwac, archiwum waży kilkaset MB)..."
        rm -rf "$xcframework_dir"
        if ! tar -xJf "$tmp_archive" -C "$target_dir"; then
            log_error "Nie udalo sie wypakowac $tmp_archive"
            rm -f "$tmp_archive"
            return 1
        fi
        rm -f "$tmp_archive"
        if [[ ! -f "$marker" ]]; then
            log_error "Po wypakowaniu brakuje $marker — sprawdz strukture archiwum"
            return 1
        fi
        log_ok "GStreamer iOS xcframework zainstalowany"
        INSTALLED+=("GStreamer iOS xcframework $gst_version")
    fi

    # Syntetyczne pkg-config wskazujace na repo-local xcframework. Wszystkie
    # moduly glib/gstreamer maja ten sam combined .a — kazdy .pc rozni sie
    # tylko nazwa i wersja. gstreamer-rs build script wola pkg-config dla
    # konkretnych modulow podczas linkowania aarch64-apple-ios.
    mkdir -p "$pkgconfig_dir"
    local headers="$xcframework_dir/ios-arm64/Headers"
    local libdir="$xcframework_dir/ios-arm64"
    local glib_version="2.84.0"

    local pc_entries=(
        "glib-2.0:$glib_version"
        "gobject-2.0:$glib_version"
        "gmodule-2.0:$glib_version"
        "gmodule-no-export-2.0:$glib_version"
        "gio-2.0:$glib_version"
        "gstreamer-1.0:$gst_version"
        "gstreamer-app-1.0:$gst_version"
        "gstreamer-audio-1.0:$gst_version"
        "gstreamer-base-1.0:$gst_version"
        "gstreamer-pbutils-1.0:$gst_version"
    )

    local entry name ver
    for entry in "${pc_entries[@]}"; do
        name="${entry%:*}"
        ver="${entry##*:}"
        cat > "$pkgconfig_dir/${name}.pc" <<EOF
prefix=$libdir
Name: $name
Description: GStreamer iOS xcframework (synthetic pc -> combined libGStreamer.a)
Version: $ver
Cflags: -I$headers
Libs: -L$libdir -lGStreamer
EOF
    done
    log_ok "pkg-config dla iOS xcframework: $pkgconfig_dir"
}

# --- GStreamer Android SDK ---

find_gstreamer_android_pkg_config_dir() {
    local root="$1"
    local target_hint="$2"
    local match
    [ -d "$root" ] || return 1
    match=$(find "$root" -path "*/lib/pkgconfig/gstreamer-1.0.pc" 2>/dev/null | grep -E "$target_hint" | head -1 || true)
    if [[ -z "$match" ]]; then
        match=$(find "$root" -path "*/pkgconfig/gstreamer-1.0.pc" 2>/dev/null | grep -E "$target_hint" | head -1 || true)
    fi
    if [[ -n "$match" ]]; then
        dirname "$match"
        return 0
    fi
    return 1
}

install_android_gstreamer_sdk() {
    log_section "GStreamer Android SDK"

    local gst_version="1.28.3"
    local cache_dir="${TENTAFLOW_NATIVE_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/tentaflow-native-libs}"
    local target_dir="$cache_dir/gstreamer/android/$gst_version"
    local existing

    existing="$(find_gstreamer_android_pkg_config_dir "$target_dir" 'arm64|aarch64' 2>/dev/null || true)"
    if [[ -z "$existing" && -n "${GSTREAMER_ANDROID_ROOT:-}" ]]; then
        existing="$(find_gstreamer_android_pkg_config_dir "$GSTREAMER_ANDROID_ROOT" 'arm64|aarch64' 2>/dev/null || true)"
    fi
    if [[ -z "$existing" ]]; then
        existing="$(find_gstreamer_android_pkg_config_dir "$HOME/Library/GStreamer" 'arm64|aarch64' 2>/dev/null || true)"
    fi
    if [[ -n "$existing" ]]; then
        log_ok "GStreamer Android SDK juz dostepny: $existing"
        return
    fi

    local archive="gstreamer-1.0-android-universal-${gst_version}.tar.xz"
    local archive_url="https://gstreamer.freedesktop.org/data/pkg/android/${gst_version}/${archive}"
    local sha_url="${archive_url}.sha256sum"
    local download_dir="$cache_dir/downloads"
    local archive_path="$download_dir/$archive"
    local sha_path="$archive_path.sha256sum"

    mkdir -p "$download_dir" "$target_dir"
    # Small checksum first; use it to decide whether the (possibly partial)
    # archive is already complete and valid — skip the 939 MB download if so.
    curl -fL -o "$sha_path" "$sha_url"
    if [[ -f "$archive_path" ]] && ( cd "$download_dir" && sha256sum -c "$(basename "$sha_path")" >/dev/null 2>&1 ); then
        log_ok "Archiwum GStreamer Android ${gst_version} juz pobrane (checksum OK) — pomijam download."
    else
        log_info "Pobieranie GStreamer Android SDK ${gst_version} (~939 MB, wznawialne)..."
        # -C - wznawia przerwane pobieranie zamiast startowac od zera.
        curl -fL -C - --progress-bar -o "$archive_path" "$archive_url"
        ( cd "$download_dir" && sha256sum -c "$(basename "$sha_path")" )
    fi

    log_info "Wypakowuje GStreamer Android SDK do $target_dir..."
    rm -rf "$target_dir"
    mkdir -p "$target_dir"
    tar -xJf "$archive_path" -C "$target_dir"

    existing="$(find_gstreamer_android_pkg_config_dir "$target_dir" 'arm64|aarch64' 2>/dev/null || true)"
    if [[ -z "$existing" ]]; then
        log_error "Po wypakowaniu nie znaleziono gstreamer-1.0.pc dla arm64 w $target_dir"
        return 1
    fi

    log_ok "GStreamer Android SDK zainstalowany: $target_dir"
    INSTALLED+=("GStreamer Android SDK $gst_version")
}

install_android_gradle_runner() {
    log_section "Android Gradle"

    local repo_root
    repo_root=$(git rev-parse --show-toplevel 2>/dev/null || true)
    if [[ -z "$repo_root" ]]; then
        log_warn "Nie znaleziono repo git — pomijam bootstrap Gradle."
        return
    fi

    local gradlew="$repo_root/tentaflow-mobile/android/gradlew"
    if [[ ! -x "$gradlew" ]]; then
        chmod +x "$gradlew"
    fi
    "$gradlew" --version >/dev/null
    log_ok "Android Gradle gotowy"
    INSTALLED+=("Android Gradle")
}

# --- CUDA ---

# Minimalna wersja CUDA dla najnowszych architektur Blackwell sm_100/sm_103
# (B200/B300/GB300). 12.9 wprowadza sm_103; celujemy w 13.x. Dystrybucyjny
# `nvidia-cuda-toolkit` (Ubuntu pakietuje 12.0/11.x) NIE zna sm_103 i wywala
# build llama.cpp na `Unsupported gpu architecture 'compute_103'`.
CUDA_MIN_MAJOR=12
CUDA_MIN_MINOR=9
CUDA_TARGET_PKG="cuda-toolkit-13-0"

# Wypisuje "MAJOR MINOR" zainstalowanego nvcc (PATH oraz /usr/local/cuda), nic gdy brak.
detect_nvcc_version() {
    local nvcc_bin=""
    if command -v nvcc &>/dev/null; then
        nvcc_bin="$(command -v nvcc)"
    elif [[ -x /usr/local/cuda/bin/nvcc ]]; then
        nvcc_bin="/usr/local/cuda/bin/nvcc"
    else
        return 1
    fi
    local v
    v=$("$nvcc_bin" --version 2>/dev/null | grep -oE 'release [0-9]+\.[0-9]+' | awk '{print $2}')
    [[ -n "$v" ]] || return 1
    echo "${v%%.*} ${v##*.}"
}

# Zwraca 0 gdy zainstalowany nvcc >= CUDA_MIN (czyli zna Blackwell sm_103).
cuda_new_enough() {
    local mm maj min
    mm=$(detect_nvcc_version) || return 1
    maj=${mm%% *}; min=${mm##* }
    if (( maj > CUDA_MIN_MAJOR )); then return 0; fi
    if (( maj == CUDA_MIN_MAJOR && min >= CUDA_MIN_MINOR )); then return 0; fi
    return 1
}

install_cuda() {
    log_section "NVIDIA CUDA toolkit"

    # /usr/local/cuda/bin (instalacja z repo NVIDIA) ma pierwszenstwo nad starym
    # /usr/bin/nvcc z dystrybucyjnego pakietu.
    [[ -x /usr/local/cuda/bin/nvcc ]] && export PATH="/usr/local/cuda/bin:$PATH"

    if cuda_new_enough; then
        log_ok "CUDA wystarczajaca dla Blackwell: $(nvcc --version 2>/dev/null | grep release)"
        return
    fi

    if local mm; mm=$(detect_nvcc_version); then
        log_warn "Wykryto za stary CUDA toolkit (${mm// /.}) — nie zna sm_103 (B300). Aktualizuje do ${CUDA_TARGET_PKG}."
    fi

    case "$DISTRO" in
        arch)
            # Arch (rolling) ma aktualny pakiet `cuda`.
            log_info "Instalacja pakietu cuda z pacman..."
            run_privileged pacman -S --needed --noconfirm cuda
            INSTALLED+=("cuda")
            ;;
        debian)
            # Dystrybucyjny nvidia-cuda-toolkit jest za stary na Blackwell —
            # bierzemy toolkit z repo NVIDIA (network repo) w wersji 13.x.
            if dpkg -l nvidia-cuda-toolkit 2>/dev/null | grep -q '^ii'; then
                log_warn "Usuwam dystrybucyjny nvidia-cuda-toolkit (za stary, przyslania nowy nvcc w /usr/bin)"
                run_privileged apt-get remove -y nvidia-cuda-toolkit || true
            fi
            local ubuntu_ver="${VERSION_ID//./}"   # 24.04 -> 2404
            local cuda_arch
            case "$(uname -m)" in
                x86_64)        cuda_arch="x86_64" ;;
                aarch64|arm64) cuda_arch="sbsa" ;;   # Grace/ARM server (GB200/GB300)
                *)             cuda_arch="x86_64" ;;
            esac
            local repo_base="https://developer.download.nvidia.com/compute/cuda/repos/ubuntu${ubuntu_ver}/${cuda_arch}"
            local keyring="/tmp/cuda-keyring_1.1-1_all.deb"
            # curl jest potrzebny do pobrania keyringa — na czystej maszynie moze go brakowac.
            if ! command -v curl &>/dev/null; then
                run_privileged apt-get install -y curl ca-certificates
            fi
            log_info "Dodaje repo NVIDIA CUDA (ubuntu${ubuntu_ver}/${cuda_arch}) i instaluje ${CUDA_TARGET_PKG}..."
            if curl -fsSL "${repo_base}/cuda-keyring_1.1-1_all.deb" -o "$keyring"; then
                run_privileged dpkg -i "$keyring"
                run_privileged apt-get update
                # Najpierw celowana 13.0; jak brak dla tej wersji Ubuntu — najnowszy meta-pakiet.
                if run_privileged apt-get install -y "${CUDA_TARGET_PKG}"; then
                    INSTALLED+=("${CUDA_TARGET_PKG}")
                elif run_privileged apt-get install -y cuda-toolkit; then
                    INSTALLED+=("cuda-toolkit")
                else
                    log_warn "Nie udalo sie zainstalowac CUDA z repo NVIDIA. Zainstaluj recznie: https://developer.nvidia.com/cuda-downloads"
                fi
                export PATH="/usr/local/cuda/bin:$PATH"
                # Trwały PATH dla przyszłych powłok — inaczej build nie znajdzie nvcc
                # po usunięciu starego /usr/bin/nvcc (build-all.sh w nowej powłoce).
                if [[ -x /usr/local/cuda/bin/nvcc ]]; then
                    echo 'export PATH=/usr/local/cuda/bin:$PATH' | run_privileged tee /etc/profile.d/zz-cuda.sh >/dev/null
                    log_ok "CUDA w PATH: /etc/profile.d/zz-cuda.sh (nowe powloki). W BIEZACEJ sesji: export PATH=/usr/local/cuda/bin:\$PATH"
                fi
            else
                log_warn "Nie pobrano cuda-keyring (ubuntu${ubuntu_ver}/${cuda_arch}). Sprawdz wersje Ubuntu/arch i zainstaluj CUDA 13 recznie: https://developer.nvidia.com/cuda-downloads"
            fi
            ;;
        fedora)
            log_warn "CUDA na Fedorze wymaga recznie dodanego repo NVIDIA (https://developer.nvidia.com/cuda-downloads)."
            log_info "Probuje zainstalowac cuda-toolkit z istniejacych repo..."
            if run_privileged dnf install -y cuda-toolkit 2>/dev/null; then
                INSTALLED+=("cuda-toolkit")
                export PATH="/usr/local/cuda/bin:$PATH"
            else
                log_warn "Nie udalo sie zainstalowac CUDA. Dodaj repo NVIDIA (>= 12.9 dla sm_103) i uruchom ponownie."
            fi
            ;;
    esac
}

# --- Vulkan SDK ---

install_vulkan() {
    log_section "Vulkan SDK (pelny, z validation layers i shaderc)"

    case "$DISTRO" in
        arch)
            local pkgs=(
                vulkan-devel
                vulkan-headers
                vulkan-validation-layers
                shaderc
                spirv-tools
            )
            log_info "Instalacja: ${pkgs[*]}"
            run_privileged pacman -S --needed --noconfirm "${pkgs[@]}"
            INSTALLED+=("vulkan-sdk")
            ;;
        debian)
            # Core, required for the Vulkan llama.cpp backend: loader+headers,
            # shader compiler, SPIR-V tools.
            local pkgs=(
                libvulkan-dev
                glslang-dev
                spirv-tools
            )
            log_info "Instalacja: ${pkgs[*]}"
            run_privileged apt-get install -y "${pkgs[@]}"
            # Validation layers are debug-only and the package name churns across
            # Ubuntu releases — 24.04 dropped `vulkan-validationlayers-dev` in
            # favour of `vulkan-validationlayers` + `vulkan-utility-libraries-dev`.
            # apt has no --skip-unavailable, so install each best-effort (a missing
            # debug layer must never abort setup).
            for opt in vulkan-validationlayers vulkan-validationlayers-dev vulkan-utility-libraries-dev; do
                if run_privileged apt-get install -y "$opt" >/dev/null 2>&1; then
                    log_ok "  + $opt"
                fi
            done
            INSTALLED+=("vulkan-sdk")
            ;;
        fedora)
            local pkgs=(
                vulkan-loader-devel
                vulkan-headers
                vulkan-validation-layers-devel
                glslang-devel
                spirv-tools
            )
            log_info "Instalacja: ${pkgs[*]}"
            run_privileged dnf install -y "${pkgs[@]}"
            INSTALLED+=("vulkan-sdk")
            ;;
    esac

    log_ok "Vulkan SDK zainstalowany"
}

# --- ROCm ---

install_rocm() {
    log_section "AMD ROCm (HIP runtime + hipBLAS)"

    case "$DISTRO" in
        arch)
            local rocm_pkgs=(
                hip-runtime-amd
                hipblas
                rocblas
                rocsolver
                rocm-cmake
            )
            log_info "Instalacja: ${rocm_pkgs[*]}"
            run_privileged pacman -S --needed --noconfirm "${rocm_pkgs[@]}"
            INSTALLED+=("rocm (hip-runtime-amd hipblas rocblas rocsolver)")

            # ROCm instaluje sie do /opt/rocm/bin — dodaj do PATH
            if [[ -d /opt/rocm/bin ]]; then
                export PATH="/opt/rocm/bin:$PATH"

                # bash/zsh: /etc/profile.d/
                if ! grep -q '/opt/rocm/bin' /etc/profile.d/rocm.sh 2>/dev/null; then
                    echo 'export PATH="/opt/rocm/bin:$PATH"' | run_privileged tee /etc/profile.d/rocm.sh >/dev/null
                    log_info "Utworzono /etc/profile.d/rocm.sh (bash/zsh)"
                    INSTALLED+=("rocm-path-profile")
                fi

                # fish
                local fish_config="$HOME/.config/fish/config.fish"
                if [[ -d "$HOME/.config/fish" ]] && ! grep -q '/opt/rocm/bin' "$fish_config" 2>/dev/null; then
                    echo 'fish_add_path /opt/rocm/bin' >> "$fish_config"
                    log_info "Dodano /opt/rocm/bin do fish config"
                    INSTALLED+=("rocm-path-fish")
                fi
            fi
            ;;
        debian)
            log_info "Sprawdzanie dostepnosci ROCm w repo..."
            local rocm_pkgs=(rocm-dev hipblas-dev rocblas-dev)
            if run_privileged apt-get install -y "${rocm_pkgs[@]}" 2>/dev/null; then
                INSTALLED+=("rocm-dev hipblas-dev rocblas-dev")
            else
                log_warn "ROCm nie jest dostepny w obecnych repo. Dodaj repo AMD:"
                echo ""
                log_info "  curl -fsSL https://repo.radeon.com/rocm/rocm.gpg.key | sudo gpg --dearmor -o /etc/apt/keyrings/rocm.gpg"
                log_info "  echo 'deb [arch=amd64 signed-by=/etc/apt/keyrings/rocm.gpg] https://repo.radeon.com/rocm/apt/latest \$(lsb_release -cs) main' | sudo tee /etc/apt/sources.list.d/rocm.list"
                log_info "  sudo apt-get update && sudo apt-get install -y ${rocm_pkgs[*]}"
                echo ""
                log_warn "Po dodaniu repo uruchom skrypt ponownie z --rocm"
            fi

            # PATH
            if [[ -d /opt/rocm/bin ]] && ! echo "$PATH" | grep -q "/opt/rocm/bin"; then
                export PATH="/opt/rocm/bin:$PATH"
                if ! grep -q '/opt/rocm/bin' /etc/profile.d/rocm.sh 2>/dev/null; then
                    echo 'export PATH="/opt/rocm/bin:$PATH"' | run_privileged tee /etc/profile.d/rocm.sh >/dev/null
                    INSTALLED+=("rocm-path-profile")
                fi
            fi
            ;;
        fedora)
            # Nazwy pakietow ROCm na Fedorze sie zmieniaja miedzy wydaniami, wiec
            # NIE hardcodujemy ich. Instalujemy to, co dostarcza konkretne
            # biblioteki wymagane przy linkowaniu wariantu 'multi' llama.cpp
            # (libamdhip64/librocblas/libhipblas) — przez `dnf whatprovides`.
            # Dziala niezaleznie od dokladnej nazwy pakietu (Fedora pakuje ROCm
            # w swoich repo od F36+).
            log_info "Szukam pakietow ROCm dostarczajacych libamdhip64/librocblas/libhipblas..."
            local rocm_libs=(libamdhip64.so librocblas.so libhipblas.so)
            local got=()
            local lib pkg
            for lib in "${rocm_libs[@]}"; do
                pkg="$(dnf -y -q repoquery --whatprovides "*/$lib" </dev/null 2>/dev/null | head -1)"
                if [ -n "$pkg" ]; then
                    if run_privileged dnf install -y "$pkg"; then
                        got+=("$pkg")
                    fi
                fi
            done
            if [ ${#got[@]} -gt 0 ]; then
                INSTALLED+=("rocm: ${got[*]}")
            else
                log_warn "ROCm nie znaleziony w repo Fedory. Dodaj repo AMD (repo.radeon.com)"
                log_warn "lub odpal z --no-rocm jesli ta maszyna nie ma karty AMD."
            fi

            # PATH
            if [[ -d /opt/rocm/bin ]] && ! echo "$PATH" | grep -q "/opt/rocm/bin"; then
                export PATH="/opt/rocm/bin:$PATH"
                if ! grep -q '/opt/rocm/bin' /etc/profile.d/rocm.sh 2>/dev/null; then
                    echo 'export PATH="/opt/rocm/bin:$PATH"' | run_privileged tee /etc/profile.d/rocm.sh >/dev/null
                    INSTALLED+=("rocm-path-profile")
                fi
            fi
            ;;
    esac
}

# --- Weryfikacja ---

download_meeting_bot_assets() {
    log_section "Pobieranie assetow teams-bot (Silero VAD)"

    local script_dir
    script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
    local model_dir="${script_dir}/tentaflow-containers/agents/native/teams-bot/models"
    local model_file="${model_dir}/silero_vad.onnx"
    local silero_url="https://github.com/snakers4/silero-vad/raw/v5.1/src/silero_vad/data/silero_vad.onnx"

    if [[ -f "$model_file" ]]; then
        log_ok "Silero VAD juz istnieje ($(du -h "$model_file" | cut -f1))"
    else
        mkdir -p "$model_dir"
        log_info "Pobieram Silero VAD: $silero_url"
        if command -v curl &>/dev/null; then
            if curl -fL "$silero_url" -o "$model_file"; then
                log_ok "Silero VAD pobrany ($(du -h "$model_file" | cut -f1))"
                INSTALLED+=("silero_vad.onnx (teams-bot)")
            else
                log_warn "Nie udalo sie pobrac Silero VAD — bot uzyje fallback RMS (gorsza jakosc VAD)"
                rm -f "$model_file"
            fi
        elif command -v wget &>/dev/null; then
            if wget -q -O "$model_file" "$silero_url"; then
                log_ok "Silero VAD pobrany ($(du -h "$model_file" | cut -f1))"
                INSTALLED+=("silero_vad.onnx (teams-bot)")
            else
                log_warn "Nie udalo sie pobrac Silero VAD — bot uzyje fallback RMS"
                rm -f "$model_file"
            fi
        else
            log_warn "Brak curl ani wget — pomijam pobieranie Silero VAD"
        fi
    fi
}

verify_installation() {
    log_section "Weryfikacja instalacji"

    local ok=true

    # cmake
    if command -v cmake &>/dev/null; then
        log_ok "cmake: $(cmake --version | head -1)"
    else
        log_error "cmake: NIE ZNALEZIONO"
        ok=false
    fi

    # clang
    if command -v clang &>/dev/null; then
        log_ok "clang: $(clang --version | head -1)"
    else
        log_error "clang: NIE ZNALEZIONO"
        ok=false
    fi

    # rustc
    if command -v rustc &>/dev/null; then
        log_ok "rustc: $(rustc --version)"
    else
        log_error "rustc: NIE ZNALEZIONO"
        ok=false
    fi

    # cargo
    if command -v cargo &>/dev/null; then
        log_ok "cargo: $(cargo --version)"
    else
        log_error "cargo: NIE ZNALEZIONO"
        ok=false
    fi

    # git lfs
    if git lfs version &>/dev/null; then
        log_ok "git-lfs: $(git lfs version)"
    else
        log_error "git-lfs: NIE ZNALEZIONO"
        ok=false
    fi

    # wasm targets
    if rustup target list --installed 2>/dev/null | grep -q "wasm32-wasip1"; then
        log_ok "wasm32-wasip1: zainstalowany"
    else
        log_error "wasm32-wasip1: BRAK"
        ok=false
    fi
    if rustup target list --installed 2>/dev/null | grep -q "wasm32-unknown-unknown"; then
        log_ok "wasm32-unknown-unknown: zainstalowany"
    else
        log_error "wasm32-unknown-unknown: BRAK"
        ok=false
    fi

    # wasm-bindgen CLI
    if command -v wasm-bindgen &>/dev/null; then
        log_ok "wasm-bindgen: $(wasm-bindgen --version 2>/dev/null)"
    else
        log_error "wasm-bindgen: NIE ZNALEZIONO (GUI nie dostanie wasm_glue.js)"
        ok=false
    fi

    # iOS targets (tylko macOS)
    if [[ "$DISTRO" == "macos" ]]; then
        for t in aarch64-apple-ios aarch64-apple-ios-sim; do
            if rustup target list --installed 2>/dev/null | grep -q "^$t$"; then
                log_ok "$t: zainstalowany"
            else
                log_warn "$t: BRAK (wymagany do buildu mobile/ios)"
            fi
        done

        # xcodebuild — wymagany przez tentaflow/build.rs do zbudowania
        # libMLXBridge.dylib (Swift bridge mlx-swift). Bez tego MLX modele
        # uruchomione w runtime zwracaja "Failed to load default metallib".
        if command -v xcodebuild &>/dev/null; then
            log_ok "xcodebuild: $(xcodebuild -version 2>/dev/null | head -1)"
            local xcode_dev
            xcode_dev=$(xcode-select -p 2>/dev/null || echo "")
            if [[ "$xcode_dev" == *"Xcode.app"* ]]; then
                log_ok "Aktywne Developer Dir to pelne Xcode"
            else
                log_warn "Aktywne Developer Dir: $xcode_dev (NIE pelne Xcode)"
                log_warn "  Przelacz: sudo xcode-select -s /Applications/Xcode.app/Contents/Developer"
            fi
        else
            log_warn "xcodebuild: BRAK (wymagany do mlx-swift bridge — Bielik / Qwen / inne MLX)"
            log_warn "  Pobierz Xcode z App Store"
        fi

        # Metal Toolchain — wymagany przez xcodebuild do kompilacji shaderow
        # mlx-swift przy budowie libMLXBridge.dylib.
        if xcrun metal --version &>/dev/null; then
            log_ok "Metal Toolchain: dostepny"
        else
            log_warn "Metal Toolchain: BRAK (wymagany do MLX)"
            log_warn "  Pobierz: xcodebuild -downloadComponent MetalToolchain"
        fi

        # iOS Platform — wymagana do buildu tentaflow-mobile/ios.
        local xcode_dev
        xcode_dev=$(xcode-select -p 2>/dev/null || true)
        if [[ -n "$xcode_dev" ]] && \
           find "$xcode_dev/Toolchains/XcodeDefault.xctoolchain/usr/lib/clang" \
                -name "libclang_rt.ios.a" 2>/dev/null | grep -q .; then
            log_ok "iOS Platform: dostepna"
        else
            log_warn "iOS Platform: BRAK (wymagana do buildu mobile/ios)"
            log_warn "  Pobierz: xcodebuild -downloadPlatform iOS"
        fi
    fi

    # pkg-config
    if command -v pkg-config &>/dev/null; then
        log_ok "pkg-config: $(pkg-config --version)"
    else
        log_error "pkg-config: NIE ZNALEZIONO"
        ok=false
    fi

    if command -v pkg-config &>/dev/null && pkg-config --exists 'glib-2.0 >= 2.56'; then
        log_ok "glib-2.0: $(pkg-config --modversion glib-2.0)"
    else
        log_error "glib-2.0: NIE ZNALEZIONO przez pkg-config"
        case "$DISTRO" in
            arch)   log_error "  Zainstaluj: sudo pacman -S --needed glib2 pkg-config" ;;
            debian) log_error "  Zainstaluj: sudo apt-get install -y libglib2.0-dev pkg-config" ;;
            fedora) log_error "  Zainstaluj: sudo dnf install -y glib2-devel pkg-config" ;;
            macos)
                log_error "  Zainstaluj: brew install glib pkg-config"
                log_error "  Albo zainstaluj oficjalny GStreamer framework i ustaw:"
                log_error "  export PKG_CONFIG_PATH=/Library/Frameworks/GStreamer.framework/Versions/1.0/lib/pkgconfig:\$PKG_CONFIG_PATH"
                ;;
        esac
        log_error "  Sprawdz: pkg-config --modversion glib-2.0"
        ok=false
    fi

    if command -v pkg-config &>/dev/null && \
       pkg-config --exists 'gstreamer-1.0 >= 1.14' && \
       pkg-config --exists 'gstreamer-app-1.0 >= 1.14'; then
        log_ok "gstreamer-1.0: $(pkg-config --modversion gstreamer-1.0)"
        log_ok "gstreamer-app-1.0: $(pkg-config --modversion gstreamer-app-1.0)"
    else
        log_error "gstreamer-1.0 / gstreamer-app-1.0: NIE ZNALEZIONO przez pkg-config"
        case "$DISTRO" in
            arch)   log_error "  Zainstaluj: sudo pacman -S --needed gstreamer gst-plugins-base-libs pkg-config" ;;
            debian) log_error "  Zainstaluj: sudo apt-get install -y libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev pkg-config" ;;
            fedora) log_error "  Zainstaluj: sudo dnf install -y gstreamer1-devel gstreamer1-plugins-base-devel pkg-config" ;;
            macos)
                log_error "  Zainstaluj: brew install gstreamer gst-plugins-base pkg-config"
                log_error "  Albo zainstaluj oficjalny GStreamer framework i ustaw:"
                log_error "  export PKG_CONFIG_PATH=/Library/Frameworks/GStreamer.framework/Versions/1.0/lib/pkgconfig:\$PKG_CONFIG_PATH"
                ;;
        esac
        log_error "  Sprawdz: pkg-config --modversion gstreamer-1.0"
        log_error "  Sprawdz: pkg-config --modversion gstreamer-app-1.0"
        ok=false
    fi

    # Chrome / Chromium — opcjonalne, wymagane tylko jesli user wdrozy
    # teams-bota w trybie native (deploy.native). Docker tryb ma chromium
    # wbudowany w obraz. Brak nie blokuje setup, tylko warning.
    local found_browser=""
    if [[ "$DISTRO" == "macos" ]]; then
        for app in \
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
            "/Applications/Chromium.app/Contents/MacOS/Chromium" \
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser" \
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"; do
            if [[ -x "$app" ]]; then
                found_browser="$app"
                break
            fi
        done
    else
        for bin in chromium chromium-browser google-chrome google-chrome-stable brave-browser microsoft-edge; do
            if command -v "$bin" &>/dev/null; then
                found_browser=$(command -v "$bin")
                break
            fi
        done
    fi
    if [[ -n "$found_browser" ]]; then
        log_ok "Chrome/Chromium: $found_browser (wymagane dla teams-bot native)"
    else
        log_warn "Chrome/Chromium: BRAK — teams-bot w trybie native nie zadziala"
        log_warn "  Docker tryb dziala bez tego (chromium w obrazie)"
    fi

    # Opcjonalne: CUDA
    if [[ "$INSTALL_CUDA" == true ]]; then
        if command -v nvcc &>/dev/null; then
            log_ok "nvcc (CUDA): $(nvcc --version 2>/dev/null | grep release)"
        else
            log_warn "nvcc (CUDA): NIE ZNALEZIONO"
        fi
    fi

    # Opcjonalne: Vulkan
    if [[ "$INSTALL_VULKAN" == true ]]; then
        if command -v vulkaninfo &>/dev/null; then
            log_ok "vulkaninfo: dostepny"
        else
            log_warn "vulkaninfo: NIE ZNALEZIONO (moze nie byc w PATH lub brak GPU)"
        fi
    fi

    # Opcjonalne: ROCm
    if [[ "$INSTALL_ROCM" == true ]]; then
        if command -v hipcc &>/dev/null; then
            log_ok "hipcc (ROCm): $(hipcc --version 2>/dev/null | head -1)"
        else
            log_warn "hipcc (ROCm): NIE ZNALEZIONO"
        fi
    fi

    # zvec — feature 'vector' jest OBOWIAZKOWY dla binarki tentaflow, wiec
    # natywna biblioteka musi byc obecna w vendorze, inaczej projekt sie nie zbuduje.
    local zvec_plat
    if [[ "$DISTRO" == "macos" ]]; then
        zvec_plat="macos-arm64"
    elif [[ "$(uname -m)" == "aarch64" || "$(uname -m)" == "arm64" ]]; then
        zvec_plat="linux-aarch64"
    else
        zvec_plat="linux-x86_64"
    fi
    local zvec_artifact="libzvec_c_api.so"
    [[ "$DISTRO" == "macos" ]] && zvec_artifact="libzvec_c_api.dylib"
    local zvec_lib="$(dirname "$0")/../tentaflow-zvec-sys/vendor/lib/$zvec_plat/$zvec_artifact"
    if [[ -f "$zvec_lib" ]]; then
        log_ok "zvec: $zvec_lib"
    else
        log_error "zvec: BRAK natywnej biblioteki — feature 'vector' jest obowiazkowy, projekt sie nie zbuduje"
        log_error "  Zbuduj: ./scripts/build-zvec.sh $zvec_plat"
        [[ "$DISTRO" != "macos" ]] && log_error "  (na Linux wymagany dzialajacy Docker — RocksDB buduje sie w kontenerze gcc-11)"
        ok=false
    fi
    # zvec dla iOS — tentaflow-mobile linkuje statyczne archiwa; vector jest
    # obowiazkowy, wiec build.rs wymaga ich dla device ORAZ symulatora.
    if [[ "$DISTRO" == "macos" ]]; then
        local iplat
        for iplat in ios-arm64 ios-sim-arm64; do
            local ios_dir="$(dirname "$0")/../tentaflow-zvec-sys/vendor/lib/$iplat"
            if [[ -f "$ios_dir/libzvec_c_api.a" && -f "$ios_dir/libzvec_deps.a" ]]; then
                log_ok "zvec ($iplat): libzvec_c_api.a + libzvec_deps.a"
            else
                log_error "zvec ($iplat): BRAK statycznych archiwow — build tentaflow-mobile ($iplat) nie zlinkuje"
                log_error "  Zbuduj: ./scripts/build-zvec.sh $iplat"
                ok=false
            fi
        done
    fi

    if [[ "$ZVEC_OK" != true ]]; then
        ok=false
    fi

    echo ""
    if [[ "$ok" == true ]]; then
        log_ok "Wszystkie wymagane zaleznosci sa dostepne."
    else
        log_error "Brakuje niektorych wymaganych zaleznosci."
        return 1
    fi
}

# --- Podsumowanie ---

print_summary() {
    log_section "Podsumowanie"

    if [[ ${#INSTALLED[@]} -eq 0 ]]; then
        log_info "Wszystko bylo juz zainstalowane, nic nie zmieniono."
    else
        log_info "Zainstalowane/zaktualizowane komponenty:"
        for item in "${INSTALLED[@]}"; do
            echo -e "  ${GREEN}+${NC} $item"
        done
    fi

    echo ""
    if [[ "${NEED_DOCKER_RELOGIN:-}" == true ]]; then
        log_warn "UWAGA: dodano Cie do grupy 'docker'. WYLOGUJ SIE I ZALOGUJ PONOWNIE"
        log_warn "(albo uruchom 'newgrp docker') ZANIM zbudujesz/uruchomisz TentaFlow —"
        log_warn "inaczej build zvec i deploy silnikow AI pada na 'permission denied docker.sock'."
        echo ""
    fi
    log_warn "REQUIRED STEP: build native libraries (no longer in the repo — each dev builds locally):"
    echo -e "  ${BOLD}./scripts/native-libs/build-all.sh${NC}"
    log_info "  Detects the platform and builds zvec, llama.cpp, whisper.cpp, sherpa-onnx, onnxruntime"
    log_info "  into native-libs/<platform>/ (include + lib-static + lib-dynamic + manifest.toml)."
    log_info "  Update sources: ${BOLD}build-all.sh --update${NC}. Single library: ${BOLD}--only llama-cpp${NC}."
    echo ""
    log_info "Only then can you build TentaFlow:"
    echo -e "  ${BOLD}cd tentaflow && cargo build --release${NC}"
    echo ""
}

# --- vLLM deployment recipes (refresh vendored snapshot) ---

# A snapshot is committed in tentaflow-core/vllm-recipes/recipes.json.gz and
# embedded into the binary, so this is a best-effort freshness refresh — never
# fatal. Offline / no network just keeps the committed snapshot.
update_vllm_recipes() {
    log_section "vLLM recipes"
    local script="$(dirname "$0")/update-vllm-recipes.sh"
    if [[ ! -f "$script" ]]; then
        log_warn "update-vllm-recipes.sh nie znaleziony — pomijam (zostaje snapshot z repo)."
        return
    fi
    if bash "$script" >/dev/null 2>&1; then
        log_ok "Snapshot recipe vLLM odswiezony."
    else
        log_warn "Nie udalo sie odswiezyc recipe (offline?) — zostaje wbudowany snapshot."
    fi
}

# --- Main ---

main() {
    echo -e "${BOLD}${BLUE}"
    echo "  _____          _        _____ _               "
    echo " |_   _|__ _ __ | |_ __ _|  ___| | _____      __"
    echo "   | |/ _ \\ '_ \\| __/ _\` | |_  | |/ _ \\ \\ /\\ / /"
    echo "   | |  __/ | | | || (_| |  _| | | (_) \\ V  V / "
    echo "   |_|\\___|_| |_|\\__\\__,_|_|   |_|\\___/ \\_/\\_/  "
    echo -e "${NC}"
    echo -e "${BOLD}Instalator zaleznosci${NC}"
    echo ""

    # detect_distro MUSI byc przed check_sudo — inaczej $DISTRO jest pusty
    # i check_sudo wchodzi w branch sudo nawet na macOS (gdzie uzywamy brew).
    detect_distro
    check_sudo

    # Silero VAD dla teams-bot (Docker COPY w Dockerfile + native build.sh
    # fallback download). Vision i audio (silero/wespeaker) modele aplikacji
    # pobierane sa runtime — vision per deploy, audio przez bootstrap async
    # przy starcie.
    download_meeting_bot_assets

    install_base
    install_git_lfs
    ensure_docker
    # Rust + WASM toolchain FIRST: it is foundational (native builds may use cargo)
    # and must never be skipped by a `set -e` abort in a later fragile native step.
    # A zvec failure on a fresh rig used to leave the box with an old/system Rust
    # and no wasm-bindgen -> Rust 1.75 build errors + dashboard wasm_glue 404.
    install_rust
    install_wasm_target
    install_wasm_bindgen_cli
    install_zvec
    update_vllm_recipes
    install_android_rust_tools
    install_android_gstreamer_sdk
    install_android_gradle_runner
    install_ios_targets
    require_full_xcode
    install_metal_toolchain
    install_ios_platform
    install_ios_gstreamer_xcframework
    install_zvec_ios

    if [[ "$INSTALL_CUDA" == true ]]; then
        install_cuda
    fi

    if [[ "$INSTALL_VULKAN" == true ]]; then
        install_vulkan
    fi

    if [[ "$INSTALL_ROCM" == true ]]; then
        install_rocm
    fi

    verify_installation
    print_summary
}

# Auto-run main tylko gdy skrypt jest wywolany bezposrednio (./setup.sh).
# Gdy source'owany z innego skryptu/sesji — udostepnia funkcje bez efektu ubocznego.
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
    main "$@"
fi
