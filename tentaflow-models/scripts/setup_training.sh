#!/bin/bash
# =============================================================================
# Plik: setup_training.sh
# Opis: Instalacja srodowiska do trenowania modeli (Ubuntu, Fedora, CachyOS).
# =============================================================================

set -o pipefail

# --- Kolory ---
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

# --- Sciezki projektu ---
PROJECT_DIR="/home/critix/repos/TentaFlow/tentaflow-models"
VENV_DIR="${PROJECT_DIR}/.venv"
SCRIPTS_DIR="${PROJECT_DIR}/scripts"
LLAMA_CPP_DIR="${HOME}/llama.cpp"

# --- Zmienne stanu ---
TOTAL_STEPS=7
SUCCEEDED=()
FAILED=()
SKIPPED=()

# =============================================================================
# Funkcje pomocnicze
# =============================================================================

info()    { echo -e "${CYAN}${BOLD}$*${NC}"; }
ok()      { echo -e "${GREEN}[OK]${NC} $*"; }
warn()    { echo -e "${YELLOW}[SKIP]${NC} $*"; }
error()   { echo -e "${RED}[BLAD]${NC} $*"; }

step() {
    local num="$1"; shift
    echo ""
    echo -e "${BOLD}========================================${NC}"
    echo -e "${CYAN}[${num}/${TOTAL_STEPS}]${NC} ${BOLD}$*${NC}"
    echo -e "${BOLD}========================================${NC}"
}

# Uruchamia komende i obsluguje bledy; nie przerywa skryptu
run_cmd() {
    local desc="$1"; shift
    if "$@"; then
        ok "${desc}"
        return 0
    else
        error "${desc} (exit code: $?)"
        return 1
    fi
}

mark_ok()      { SUCCEEDED+=("$1"); }
mark_fail()    { FAILED+=("$1"); }
mark_skip()    { SKIPPED+=("$1"); }

# =============================================================================
# Detekcja dystrybucji
# =============================================================================

detect_distro() {
    if [[ ! -f /etc/os-release ]]; then
        error "Brak /etc/os-release — nie mozna wykryc dystrybucji."
        exit 1
    fi

    # shellcheck source=/dev/null
    source /etc/os-release

    case "${ID}" in
        ubuntu|debian)
            DISTRO="ubuntu"
            PKG="apt"
            ;;
        fedora)
            DISTRO="fedora"
            PKG="dnf"
            ;;
        cachyos|arch|endeavouros|manjaro)
            DISTRO="arch"
            PKG="pacman"
            ;;
        *)
            # Sprawdz ID_LIKE jako fallback
            if [[ "${ID_LIKE}" == *"ubuntu"* ]] || [[ "${ID_LIKE}" == *"debian"* ]]; then
                DISTRO="ubuntu"
                PKG="apt"
            elif [[ "${ID_LIKE}" == *"fedora"* ]]; then
                DISTRO="fedora"
                PKG="dnf"
            elif [[ "${ID_LIKE}" == *"arch"* ]]; then
                DISTRO="arch"
                PKG="pacman"
            else
                error "Nieobslugiwana dystrybucja: ${ID} (ID_LIKE=${ID_LIKE})"
                exit 1
            fi
            ;;
    esac

    ok "Wykryto dystrybucje: ${DISTRO} (${PRETTY_NAME})"
}

# =============================================================================
# Sprawdzenie sudo
# =============================================================================

check_sudo() {
    if [[ $EUID -eq 0 ]]; then
        warn "Skrypt uruchomiony jako root — sudo nie jest wymagane."
        SUDO=""
    else
        if ! command -v sudo &>/dev/null; then
            error "Brak sudo. Zainstaluj sudo lub uruchom jako root."
            exit 1
        fi
        # Sprawdz czy uzytkownik ma uprawnienia sudo
        if ! sudo -v 2>/dev/null; then
            error "Brak uprawnien sudo. Uruchom jako root lub dodaj uzytkownika do grupy sudo/wheel."
            exit 1
        fi
        SUDO="sudo"
        ok "Uprawnienia sudo potwierdzone."
    fi
}

# =============================================================================
# Sprawdzenie NVIDIA GPU
# =============================================================================

check_nvidia() {
    if ! command -v nvidia-smi &>/dev/null; then
        error "nvidia-smi nie znaleziono. Upewnij sie, ze driver NVIDIA jest zainstalowany."
        exit 1
    fi
    local gpu_info
    gpu_info=$(nvidia-smi --query-gpu=name,driver_version --format=csv,noheader 2>/dev/null | head -1)
    ok "GPU: ${gpu_info}"

    local gpu_count
    gpu_count=$(nvidia-smi --query-gpu=name --format=csv,noheader 2>/dev/null | wc -l)
    ok "Liczba GPU: ${gpu_count}"
}

# =============================================================================
# Krok 1: Python 3.12
# =============================================================================

install_python() {
    step 1 "Python 3.12 + venv + dev"

    if command -v python3.12 &>/dev/null; then
        local pyver
        pyver=$(python3.12 --version 2>&1)
        warn "Python 3.12 juz zainstalowany: ${pyver}"
        mark_skip "Python 3.12"
        return 0
    fi

    local step_ok=true

    case "${DISTRO}" in
        ubuntu)
            # Sprawdz czy python3.12 jest dostepny w repo
            if ! apt-cache show python3.12 &>/dev/null 2>&1; then
                info "Python 3.12 niedostepny w repo — dodaje deadsnakes PPA..."
                run_cmd "Dodanie deadsnakes PPA" \
                    $SUDO add-apt-repository -y ppa:deadsnakes/ppa || step_ok=false
                run_cmd "Aktualizacja listy pakietow" \
                    $SUDO apt update -y || step_ok=false
            fi
            run_cmd "Instalacja python3.12" \
                $SUDO apt install -y python3.12 python3.12-venv python3.12-dev || step_ok=false
            ;;
        fedora)
            run_cmd "Instalacja python3.12" \
                $SUDO dnf install -y python3.12 python3.12-devel || step_ok=false
            ;;
        arch)
            # Sprawdz czy python312 jest w repo
            if pacman -Si python312 &>/dev/null 2>&1; then
                run_cmd "Instalacja python312 z repo" \
                    $SUDO pacman -S --noconfirm python312 || step_ok=false
            elif pacman -Si python3.12 &>/dev/null 2>&1; then
                run_cmd "Instalacja python3.12 z repo" \
                    $SUDO pacman -S --noconfirm python3.12 || step_ok=false
            else
                # Sprawdz czy python w repo to 3.12.x
                local sys_py_ver
                sys_py_ver=$(python3 --version 2>/dev/null | grep -oP '3\.\d+')
                if [[ "${sys_py_ver}" == "3.12" ]]; then
                    warn "Systemowy python3 to juz 3.12 — uzywam go."
                else
                    info "Python 3.12 niedostepny w repo — probuję z AUR (yay)..."
                    if command -v yay &>/dev/null; then
                        run_cmd "Instalacja python312 z AUR" \
                            yay -S --noconfirm python312 || step_ok=false
                    elif command -v paru &>/dev/null; then
                        run_cmd "Instalacja python312 z AUR" \
                            paru -S --noconfirm python312 || step_ok=false
                    else
                        error "Brak yay/paru. Zainstaluj python 3.12 recznie lub zainstaluj yay."
                        step_ok=false
                    fi
                fi
            fi
            ;;
    esac

    if $step_ok && command -v python3.12 &>/dev/null; then
        ok "Python 3.12 zainstalowany: $(python3.12 --version 2>&1)"
        mark_ok "Python 3.12"
    else
        error "Instalacja Python 3.12 nie powiodla sie."
        mark_fail "Python 3.12"
    fi
}

# =============================================================================
# Krok 2: Zaleznosci systemowe
# =============================================================================

install_system_deps() {
    step 2 "Zaleznosci systemowe (git, cmake, ninja, kompilatory)"

    local step_ok=true

    case "${DISTRO}" in
        ubuntu)
            run_cmd "Aktualizacja listy pakietow" \
                $SUDO apt update -y || true
            run_cmd "Instalacja build-essential, git, cmake, ninja-build" \
                $SUDO apt install -y build-essential git cmake ninja-build gcc g++ || step_ok=false
            ;;
        fedora)
            run_cmd "Instalacja git, cmake, ninja-build, gcc, gcc-c++, make" \
                $SUDO dnf install -y git cmake ninja-build gcc gcc-c++ make || step_ok=false
            ;;
        arch)
            run_cmd "Instalacja base-devel, git, cmake, ninja" \
                $SUDO pacman -S --noconfirm --needed base-devel git cmake ninja || step_ok=false
            ;;
    esac

    if $step_ok; then
        ok "Zaleznosci systemowe zainstalowane."
        mark_ok "Zaleznosci systemowe"
    else
        error "Niektore zaleznosci systemowe nie zostaly zainstalowane."
        mark_fail "Zaleznosci systemowe"
    fi
}

# =============================================================================
# Krok 3: CUDA Toolkit 12.8
# =============================================================================

install_cuda_toolkit() {
    step 3 "CUDA Toolkit 12.8 (bez drivera)"

    # Sprawdz czy nvcc juz istnieje
    if command -v nvcc &>/dev/null; then
        local nvcc_ver
        nvcc_ver=$(nvcc --version 2>/dev/null | grep -oP 'release \K[0-9.]+')
        warn "nvcc juz zainstalowany: wersja ${nvcc_ver}"
        mark_skip "CUDA Toolkit"
        return 0
    fi

    # Sprawdz czy CUDA jest zainstalowana ale nie ma w PATH
    if [[ -x /usr/local/cuda/bin/nvcc ]]; then
        warn "nvcc znaleziony w /usr/local/cuda/bin/ — dodaje do PATH."
        setup_cuda_env
        mark_skip "CUDA Toolkit"
        return 0
    fi

    local step_ok=true

    case "${DISTRO}" in
        ubuntu)
            info "Dodawanie repozytorium NVIDIA CUDA (Ubuntu)..."
            # Pobierz i zainstaluj klucz + repo
            local cuda_keyring="cuda-keyring_1.1-1_all.deb"
            run_cmd "Pobranie cuda-keyring" \
                wget -q "https://developer.download.nvidia.com/compute/cuda/repos/ubuntu2404/x86_64/${cuda_keyring}" \
                -O "/tmp/${cuda_keyring}" || step_ok=false

            if $step_ok; then
                run_cmd "Instalacja cuda-keyring" \
                    $SUDO dpkg -i "/tmp/${cuda_keyring}" || step_ok=false
                rm -f "/tmp/${cuda_keyring}"
                run_cmd "Aktualizacja listy pakietow" \
                    $SUDO apt update -y || true
                # Instaluj TYLKO toolkit — bez drivera
                run_cmd "Instalacja cuda-toolkit-12-8" \
                    $SUDO apt install -y cuda-toolkit-12-8 || step_ok=false
            fi
            ;;
        fedora)
            info "Dodawanie repozytorium NVIDIA CUDA (Fedora)..."
            local fedora_ver
            fedora_ver=$(rpm -E %fedora)
            run_cmd "Dodanie CUDA repo" \
                $SUDO dnf config-manager addrepo --from-repofile="https://developer.download.nvidia.com/compute/cuda/repos/fedora${fedora_ver}/x86_64/cuda-fedora${fedora_ver}.repo" \
                || step_ok=false

            if $step_ok; then
                # Instaluj TYLKO toolkit — bez drivera
                run_cmd "Instalacja cuda-toolkit-12-8" \
                    $SUDO dnf install -y cuda-toolkit-12-8 || step_ok=false
            fi
            ;;
        arch)
            # Na Archu/CachyOS pacman daje najnowsza CUDA
            run_cmd "Instalacja cuda (pacman)" \
                $SUDO pacman -S --noconfirm --needed cuda || step_ok=false
            ;;
    esac

    # Ustaw zmienne srodowiskowe
    setup_cuda_env

    # Weryfikacja
    if command -v nvcc &>/dev/null || [[ -x /usr/local/cuda/bin/nvcc ]]; then
        local final_nvcc
        if command -v nvcc &>/dev/null; then
            final_nvcc=$(nvcc --version 2>/dev/null | grep -oP 'release \K[0-9.]+')
        else
            final_nvcc=$(/usr/local/cuda/bin/nvcc --version 2>/dev/null | grep -oP 'release \K[0-9.]+')
        fi
        ok "CUDA Toolkit zainstalowany: wersja ${final_nvcc}"
        mark_ok "CUDA Toolkit"
    else
        if $step_ok; then
            warn "CUDA zainstalowana, ale nvcc nie jest jeszcze w PATH. Przeladuj shell."
            mark_ok "CUDA Toolkit"
        else
            error "Instalacja CUDA Toolkit nie powiodla sie."
            mark_fail "CUDA Toolkit"
        fi
    fi
}

setup_cuda_env() {
    # Dodaj CUDA do PATH i LD_LIBRARY_PATH w biezacej sesji
    local cuda_home=""
    if [[ -d /usr/local/cuda ]]; then
        cuda_home="/usr/local/cuda"
    elif [[ -d /opt/cuda ]]; then
        cuda_home="/opt/cuda"
    fi

    if [[ -z "${cuda_home}" ]]; then
        return
    fi

    export PATH="${cuda_home}/bin:${PATH}"
    export LD_LIBRARY_PATH="${cuda_home}/lib64:${LD_LIBRARY_PATH:-}"

    # Dodaj do .bashrc i .zshrc jesli jeszcze nie ma
    local env_lines=(
        "export PATH=\"${cuda_home}/bin:\${PATH}\""
        "export LD_LIBRARY_PATH=\"${cuda_home}/lib64:\${LD_LIBRARY_PATH:-}\""
    )

    for rcfile in "${HOME}/.bashrc" "${HOME}/.zshrc"; do
        if [[ -f "${rcfile}" ]]; then
            for line in "${env_lines[@]}"; do
                if ! grep -qF "${cuda_home}/bin" "${rcfile}" 2>/dev/null; then
                    echo "" >> "${rcfile}"
                    echo "# CUDA Toolkit (dodane przez setup_training.sh)" >> "${rcfile}"
                    echo "${env_lines[0]}" >> "${rcfile}"
                    echo "${env_lines[1]}" >> "${rcfile}"
                    ok "Dodano CUDA do ${rcfile}"
                    break
                fi
            done
        fi
    done
}

# =============================================================================
# Krok 4: Python venv
# =============================================================================

setup_venv() {
    step 4 "Srodowisko wirtualne Python (venv)"

    # Znajdz python3.12
    local py_bin=""
    if command -v python3.12 &>/dev/null; then
        py_bin="python3.12"
    elif [[ -x /usr/bin/python3.12 ]]; then
        py_bin="/usr/bin/python3.12"
    else
        error "Nie znaleziono python3.12 — pomijam tworzenie venv."
        mark_fail "Python venv"
        return 1
    fi

    if [[ -f "${VENV_DIR}/bin/activate" ]]; then
        warn "Venv juz istnieje w ${VENV_DIR}"
        # Aktywuj i sprawdz
        # shellcheck source=/dev/null
        source "${VENV_DIR}/bin/activate"
        ok "Venv aktywowany: $(python --version 2>&1)"
        mark_skip "Python venv"
        return 0
    fi

    local step_ok=true

    run_cmd "Tworzenie venv z ${py_bin}" \
        "${py_bin}" -m venv "${VENV_DIR}" || step_ok=false

    if $step_ok; then
        # shellcheck source=/dev/null
        source "${VENV_DIR}/bin/activate"
        run_cmd "Upgrade pip" \
            pip install --upgrade pip || step_ok=false
    fi

    if $step_ok; then
        ok "Venv utworzony i aktywowany: $(python --version 2>&1)"
        mark_ok "Python venv"
    else
        error "Tworzenie venv nie powiodlo sie."
        mark_fail "Python venv"
    fi
}

# =============================================================================
# Krok 5: Pakiety Python
# =============================================================================

install_python_packages() {
    step 5 "Pakiety Python (PyTorch, transformers, deepspeed, ...)"

    # Upewnij sie ze venv jest aktywny
    if [[ -z "${VIRTUAL_ENV}" ]]; then
        if [[ -f "${VENV_DIR}/bin/activate" ]]; then
            # shellcheck source=/dev/null
            source "${VENV_DIR}/bin/activate"
        else
            error "Brak aktywnego venv — pomijam instalacje pakietow."
            mark_fail "Pakiety Python"
            return 1
        fi
    fi

    local step_ok=true

    # PyTorch z CUDA 12.8
    info "Instalacja PyTorch + torchvision + torchaudio (CUDA 12.8)..."
    run_cmd "PyTorch (cu128)" \
        pip install torch torchvision torchaudio \
        --index-url https://download.pytorch.org/whl/cu128 || step_ok=false

    # Glowne pakiety ML
    info "Instalacja pakietow ML..."
    run_cmd "transformers, datasets, peft, trl, accelerate" \
        pip install \
        "transformers>=4.52" \
        "datasets>=3.0" \
        "peft>=0.15" \
        "trl>=0.18" \
        "accelerate>=1.5" || step_ok=false

    run_cmd "deepspeed" \
        pip install "deepspeed>=0.16" || step_ok=false

    run_cmd "bitsandbytes" \
        pip install "bitsandbytes>=0.45" || step_ok=false

    # Pakiety tokenizacji i serializacji
    info "Instalacja pakietow pomocniczych..."
    run_cmd "sentencepiece, protobuf, huggingface-hub, tokenizers, safetensors, scipy, ninja" \
        pip install \
        sentencepiece \
        protobuf \
        "huggingface-hub>=0.30" \
        "tokenizers>=0.22" \
        safetensors \
        scipy \
        ninja || step_ok=false

    # llama-cpp-python z CUDA
    info "Instalacja llama-cpp-python (kompilacja z CUDA)..."
    run_cmd "llama-cpp-python (CUDA)" \
        env CMAKE_ARGS="-DGGML_CUDA=on" pip install llama-cpp-python || step_ok=false

    # Weryfikacja PyTorch + CUDA
    info "Weryfikacja PyTorch CUDA..."
    if python -c "import torch; assert torch.cuda.is_available(), 'CUDA niedostepna'; print(f'PyTorch {torch.__version__}, CUDA: {torch.version.cuda}, GPU: {torch.cuda.get_device_name(0)}')" 2>/dev/null; then
        ok "PyTorch widzi GPU."
    else
        warn "PyTorch nie widzi CUDA — moze byc potrzebny restart lub poprawka PATH."
    fi

    if $step_ok; then
        ok "Wszystkie pakiety Python zainstalowane."
        mark_ok "Pakiety Python"
    else
        error "Niektore pakiety Python nie zostaly zainstalowane."
        mark_fail "Pakiety Python"
    fi
}

# =============================================================================
# Krok 6: llama.cpp (build from source)
# =============================================================================

build_llama_cpp() {
    step 6 "llama.cpp (build from source z CUDA)"

    # Sprawdz czy juz zbudowany
    if [[ -x "${LLAMA_CPP_DIR}/build/bin/llama-quantize" ]]; then
        warn "llama.cpp juz zbudowane — llama-quantize istnieje."
        mark_skip "llama.cpp"
        return 0
    fi

    local step_ok=true

    # Klonuj repo jesli nie istnieje
    if [[ -d "${LLAMA_CPP_DIR}" ]]; then
        info "Katalog ${LLAMA_CPP_DIR} juz istnieje — aktualizuje..."
        run_cmd "git pull llama.cpp" \
            git -C "${LLAMA_CPP_DIR}" pull --ff-only || true
    else
        run_cmd "Klonowanie llama.cpp" \
            git clone https://github.com/ggml-org/llama.cpp "${LLAMA_CPP_DIR}" || step_ok=false
    fi

    if $step_ok; then
        info "Budowanie llama.cpp z CUDA..."
        run_cmd "cmake configure" \
            cmake -S "${LLAMA_CPP_DIR}" -B "${LLAMA_CPP_DIR}/build" -DGGML_CUDA=ON || step_ok=false
    fi

    if $step_ok; then
        run_cmd "cmake build" \
            cmake --build "${LLAMA_CPP_DIR}/build" --config Release -j"$(nproc)" || step_ok=false
    fi

    # Weryfikacja
    if [[ -x "${LLAMA_CPP_DIR}/build/bin/llama-quantize" ]]; then
        ok "llama.cpp zbudowane: ${LLAMA_CPP_DIR}/build/bin/llama-quantize"
        mark_ok "llama.cpp"
    else
        error "Budowanie llama.cpp nie powiodlo sie lub brak llama-quantize."
        mark_fail "llama.cpp"
    fi
}

# =============================================================================
# Krok 7: Pobranie modeli bazowych
# =============================================================================

download_models() {
    step 7 "Pobranie modeli bazowych (Qwen3.5-0.8B, Llama-Prompt-Guard-2)"

    local download_script="${SCRIPTS_DIR}/download_models.py"

    if [[ ! -f "${download_script}" ]]; then
        error "Brak skryptu ${download_script}"
        mark_fail "Pobranie modeli"
        return 1
    fi

    # Upewnij sie ze venv jest aktywny
    if [[ -z "${VIRTUAL_ENV}" ]]; then
        if [[ -f "${VENV_DIR}/bin/activate" ]]; then
            # shellcheck source=/dev/null
            source "${VENV_DIR}/bin/activate"
        else
            error "Brak aktywnego venv — pomijam pobieranie modeli."
            mark_fail "Pobranie modeli"
            return 1
        fi
    fi

    local step_ok=true

    run_cmd "Pobieranie modeli bazowych" \
        python3 "${download_script}" || step_ok=false

    if $step_ok; then
        ok "Modele bazowe pobrane."
        mark_ok "Pobranie modeli"
    else
        error "Pobieranie modeli nie powiodlo sie (sprawdz token HuggingFace)."
        mark_fail "Pobranie modeli"
    fi
}

# =============================================================================
# Podsumowanie
# =============================================================================

print_summary() {
    echo ""
    echo -e "${BOLD}========================================${NC}"
    echo -e "${BOLD}         PODSUMOWANIE                   ${NC}"
    echo -e "${BOLD}========================================${NC}"

    if [[ ${#SUCCEEDED[@]} -gt 0 ]]; then
        echo -e "${GREEN}${BOLD}Udane:${NC}"
        for item in "${SUCCEEDED[@]}"; do
            echo -e "  ${GREEN}+${NC} ${item}"
        done
    fi

    if [[ ${#SKIPPED[@]} -gt 0 ]]; then
        echo -e "${YELLOW}${BOLD}Pominiete (juz zainstalowane):${NC}"
        for item in "${SKIPPED[@]}"; do
            echo -e "  ${YELLOW}~${NC} ${item}"
        done
    fi

    if [[ ${#FAILED[@]} -gt 0 ]]; then
        echo -e "${RED}${BOLD}Nieudane:${NC}"
        for item in "${FAILED[@]}"; do
            echo -e "  ${RED}x${NC} ${item}"
        done
    fi

    echo ""
    local total_ok=$(( ${#SUCCEEDED[@]} + ${#SKIPPED[@]} ))
    echo -e "${BOLD}Wynik: ${GREEN}${total_ok}/${TOTAL_STEPS} OK${NC}, ${RED}${#FAILED[@]}/${TOTAL_STEPS} BLEDY${NC}"

    if [[ ${#FAILED[@]} -eq 0 ]]; then
        echo ""
        echo -e "${GREEN}${BOLD}Srodowisko gotowe do trenowania!${NC}"
        echo -e "Aktywuj venv: ${CYAN}source ${VENV_DIR}/bin/activate${NC}"
    else
        echo ""
        echo -e "${YELLOW}Sprawdz bledy powyzej i uruchom skrypt ponownie.${NC}"
    fi
    echo ""
}

# =============================================================================
# Glowna funkcja
# =============================================================================

main() {
    echo -e "${BOLD}"
    echo "============================================="
    echo " TentaFlow — Setup Training Environment"
    echo " GPU: NVIDIA RTX 3090 x7"
    echo " CUDA wheels: 12.8 | Python: 3.12"
    echo "============================================="
    echo -e "${NC}"

    detect_distro
    check_sudo
    check_nvidia

    install_python
    install_system_deps
    install_cuda_toolkit
    setup_venv
    install_python_packages
    build_llama_cpp
    download_models

    print_summary
}

main "$@"
