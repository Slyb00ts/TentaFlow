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

# Flagi GPU
INSTALL_CUDA=false
INSTALL_VULKAN=false
INSTALL_ROCM=false

# Wykryta dystrybucja
DISTRO=""

# Lista zainstalowanych komponentow (do podsumowania)
INSTALLED=()

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

Opcje:
  --cuda        Zainstaluj NVIDIA CUDA toolkit
  --vulkan      Zainstaluj pelny Vulkan SDK (headers, validation layers, shaderc)
  --rocm        Zainstaluj AMD ROCm (HIP runtime)
  --all-gpu     Zainstaluj wszystkie GPU backends (CUDA + Vulkan + ROCm)
  -h, --help    Pokaz te pomoc

Przyklady:
  $0                  # Tylko bazowe zaleznosci
  $0 --cuda           # Baza + CUDA
  $0 --all-gpu        # Baza + wszystkie GPU backends

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
                pkg-config
                glib2
                gstreamer
                gst-plugins-base-libs
                openssl
                vulkan-icd-loader
                sqlite
                # Profiling: perf zbiera CPU samples + PMU counters + uncore IMC.
                # which jest potrzebne dla collectors/permissions auto-discovery.
                perf
                which
                # iostat dla disk IO collector (/usr/bin/iostat).
                sysstat
            )
            log_info "Instalacja: ${pkgs[*]}"
            run_privileged pacman -S --needed --noconfirm "${pkgs[@]}"
            INSTALLED+=("base-devel" "cmake" "clang" "lld" "glib2" "gstreamer" "gst-plugins-base-libs" "vulkan-loader" "sqlite" "perf" "sysstat")
            ;;
        debian)
            log_info "Aktualizacja listy pakietow apt..."
            run_privileged apt-get update -qq

            local pkgs=(
                build-essential
                cmake
                clang
                lld
                pkg-config
                libglib2.0-dev
                libgstreamer1.0-dev
                libgstreamer-plugins-base1.0-dev
                libssl-dev
                libvulkan1
                libsqlite3-dev
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
            INSTALLED+=("build-essential" "cmake" "clang" "lld" "libglib2.0-dev" "libgstreamer1.0-dev" "libgstreamer-plugins-base1.0-dev" "libvulkan1" "sqlite3-dev" "perf" "sysstat" "libclang-dev" "patchelf")
            ;;
        fedora)
            local pkgs=(
                gcc
                gcc-c++
                make
                cmake
                clang
                lld
                pkg-config
                glib2-devel
                gstreamer1-devel
                gstreamer1-plugins-base-devel
                openssl-devel
                vulkan-loader
                sqlite-devel
                # Profiling: perf jest w pakiecie 'perf' na Fedora 38+.
                # sysstat dostarcza iostat dla linux.iostat.disk collector.
                perf
                sysstat
            )
            log_info "Instalacja: ${pkgs[*]}"
            run_privileged dnf install -y "${pkgs[@]}"
            INSTALLED+=("gcc/g++" "cmake" "clang" "lld" "glib2-devel" "gstreamer1-devel" "gstreamer1-plugins-base-devel" "vulkan-loader" "sqlite-devel" "perf" "sysstat")
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
                llvm
                pkg-config
                glib
                gstreamer
                gst-plugins-base
                openssl@3
                sqlite
            )
            log_info "Instalacja: ${pkgs[*]}"
            brew install "${pkgs[@]}"
            configure_macos_gstreamer_pkg_config
            ensure_macos_metal_toolchain
            INSTALLED+=("cmake" "llvm (clang+lld)" "pkg-config" "glib" "gstreamer" "gst-plugins-base" "openssl@3" "sqlite")
            ;;
    esac

    log_ok "Bazowe zaleznosci zainstalowane"
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

# --- CUDA ---

install_cuda() {
    log_section "NVIDIA CUDA toolkit"

    if command -v nvcc &>/dev/null; then
        log_ok "CUDA juz zainstalowane: $(nvcc --version 2>/dev/null | tail -1)"
        return
    fi

    case "$DISTRO" in
        arch)
            log_info "Instalacja pakietu cuda z pacman..."
            run_privileged pacman -S --needed --noconfirm cuda
            INSTALLED+=("cuda")
            ;;
        debian)
            log_info "Instalacja nvidia-cuda-toolkit..."
            run_privileged apt-get install -y nvidia-cuda-toolkit
            INSTALLED+=("nvidia-cuda-toolkit")
            ;;
        fedora)
            log_warn "CUDA na Fedorze wymaga recznie dodanego repo NVIDIA."
            log_warn "Instrukcja: https://developer.nvidia.com/cuda-downloads"
            log_info "Probuje zainstalowac z istniejacych repo..."
            if run_privileged dnf install -y cuda-toolkit 2>/dev/null; then
                INSTALLED+=("cuda-toolkit")
            else
                log_warn "Nie udalo sie zainstalowac CUDA. Dodaj repo NVIDIA i uruchom ponownie."
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
            local pkgs=(
                libvulkan-dev
                vulkan-validationlayers-dev
                glslang-dev
                spirv-tools
            )
            log_info "Instalacja: ${pkgs[*]}"
            run_privileged apt-get install -y "${pkgs[@]}"
            INSTALLED+=("vulkan-sdk")
            ;;
        fedora)
            local pkgs=(
                vulkan-devel
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
            log_info "Sprawdzanie dostepnosci ROCm w repo..."
            local rocm_pkgs=(rocm-dev hipblas-devel rocblas-devel)
            if run_privileged dnf install -y "${rocm_pkgs[@]}" 2>/dev/null; then
                INSTALLED+=("rocm-dev hipblas-devel rocblas-devel")
            else
                log_warn "ROCm nie jest dostepny w obecnych repo. Dodaj repo AMD:"
                echo ""
                log_info "  sudo tee /etc/yum.repos.d/rocm.repo <<'REPO'"
                log_info "  [ROCm]"
                log_info "  name=ROCm"
                log_info "  baseurl=https://repo.radeon.com/rocm/rhel9/latest/main"
                log_info "  enabled=1"
                log_info "  gpgcheck=1"
                log_info "  gpgkey=https://repo.radeon.com/rocm/rocm.gpg.key"
                log_info "  REPO"
                log_info "  sudo dnf install -y ${rocm_pkgs[*]}"
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
    log_info "Mozesz teraz zbudowac TentaFlow:"
    echo -e "  ${BOLD}cd tentaflow && cargo build --release${NC}"
    echo ""
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
    install_rust
    install_wasm_target
    install_wasm_bindgen_cli
    install_ios_targets
    require_full_xcode
    install_metal_toolchain
    install_ios_platform
    install_ios_gstreamer_xcframework

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
