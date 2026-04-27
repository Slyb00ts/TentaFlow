#!/bin/bash
# =============================================================================
# Plik: build.sh
# Opis: Builder native bundla teams-bota. Faktyczna binarka tentaflow-meeting
#       jest budowana automatycznie przez `tentaflow/build.rs` (cargo build
#       w `cargo build --release` glownego projektu wciaga sidecar). Ten skrypt
#       sluzy tylko jako entrypoint dla manifest validation i ewentualnego
#       sciagania duzych asetow (np. silero_vad.onnx) ktorych nie commitujemy
#       do repo.
# =============================================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Pobierz Silero VAD model jesli go brakuje. URL z oficjalnego repo snakers4.
MODEL_DIR="${SCRIPT_DIR}/models"
MODEL_FILE="${MODEL_DIR}/silero_vad.onnx"
SILERO_URL="https://github.com/snakers4/silero-vad/raw/v5.1/src/silero_vad/data/silero_vad.onnx"

if [ ! -f "${MODEL_FILE}" ]; then
    mkdir -p "${MODEL_DIR}"
    echo "Pobieram Silero VAD: ${SILERO_URL}"
    if command -v curl >/dev/null 2>&1; then
        curl -fL "${SILERO_URL}" -o "${MODEL_FILE}"
    elif command -v wget >/dev/null 2>&1; then
        wget -O "${MODEL_FILE}" "${SILERO_URL}"
    else
        echo "BRAK curl ani wget — pobierz silero_vad.onnx recznie do ${MODEL_FILE}"
        exit 1
    fi
fi

echo "Native teams-bot bundle gotowy. Binarka tentaflow-meeting bedzie zbudowana"
echo "razem z 'cargo build --release' projektu tentaflow/."
