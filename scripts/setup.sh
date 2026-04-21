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
                openssl
                vulkan-icd-loader
                sqlite
            )
            log_info "Instalacja: ${pkgs[*]}"
            run_privileged pacman -S --needed --noconfirm "${pkgs[@]}"
            INSTALLED+=("base-devel" "cmake" "clang" "lld" "vulkan-loader" "sqlite")
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
                libssl-dev
                libvulkan1
                libsqlite3-dev
            )
            log_info "Instalacja: ${pkgs[*]}"
            run_privileged apt-get install -y "${pkgs[@]}"
            INSTALLED+=("build-essential" "cmake" "clang" "lld" "libvulkan1" "sqlite3-dev")
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
                openssl-devel
                vulkan-loader
                sqlite-devel
            )
            log_info "Instalacja: ${pkgs[*]}"
            run_privileged dnf install -y "${pkgs[@]}"
            INSTALLED+=("gcc/g++" "cmake" "clang" "lld" "vulkan-loader" "sqlite-devel")
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
                openssl@3
                sqlite
            )
            log_info "Instalacja: ${pkgs[*]}"
            brew install "${pkgs[@]}"
            INSTALLED+=("cmake" "llvm (clang+lld)" "pkg-config" "openssl@3" "sqlite")
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
WASM_BINDGEN_VERSION="0.2.108"

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
    fi

    # pkg-config
    if command -v pkg-config &>/dev/null; then
        log_ok "pkg-config: $(pkg-config --version)"
    else
        log_error "pkg-config: NIE ZNALEZIONO"
        ok=false
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

    check_sudo
    detect_distro
    install_base
    install_rust
    install_wasm_target
    install_wasm_bindgen_cli
    install_ios_targets

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

main
