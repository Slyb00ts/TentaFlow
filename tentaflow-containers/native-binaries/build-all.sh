#!/usr/bin/env bash
# =============================================================================
# Plik: build-all.sh
# Opis: Buduje wszystkie silniki dla hosta + preferowanego backendu.
#       Docelowo CI wola ten skrypt na kazdym runnerze (linux x86/arm, macos, win).
# =============================================================================

set -eo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

ENGINES=(llama-cpp whisper-cpp sherpa-onnx text-embeddings stable-diffusion-cpp)

OS="${1:-auto}"
ARCH="${2:-$(uname -m)}"
BACKEND="${3:-auto}"

for eng in "${ENGINES[@]}"; do
  echo ""
  echo "============================================"
  echo "  $eng"
  echo "============================================"
  bash "$SCRIPT_DIR/$eng/build.sh" "$OS" "$ARCH" "$BACKEND" || {
    echo "[build-all] $eng nieudany — kontynuuje" >&2
  }
done

echo ""
echo "Gotowe — artefakty w $SCRIPT_DIR/output/"
ls -lh "$SCRIPT_DIR/output/" 2>/dev/null || true
