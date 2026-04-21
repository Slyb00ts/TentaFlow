#!/usr/bin/env bash
# =============================================================================
# Plik: build-natives.sh
# Opis: Buduje wszystkie natywne silniki AI dla hosta + preferowanego backendu.
#       Docelowo CI wola ten skrypt na kazdym runnerze (linux x86/arm, macos, win).
# =============================================================================

set -eo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Mapa engine -> kategoria (silniki zgrupowane wedlug nowej struktury)
ENGINES=(
  "llm/llama-cpp"
  "stt/whisper-cpp"
  "tts/sherpa-onnx"
  "embeddings/text-embeddings"
  "image-gen/stable-diffusion-cpp"
)

OS="${1:-auto}"
ARCH="${2:-$(uname -m)}"
BACKEND="${3:-auto}"

for entry in "${ENGINES[@]}"; do
  category="${entry%%/*}"
  eng="${entry##*/}"
  build_script="$SCRIPT_DIR/$category/native/$eng/build.sh"

  echo ""
  echo "============================================"
  echo "  $category / $eng"
  echo "============================================"

  if [ ! -f "$build_script" ]; then
    echo "[build-natives] brak $build_script — pomijam" >&2
    continue
  fi

  bash "$build_script" "$OS" "$ARCH" "$BACKEND" || {
    echo "[build-natives] $entry nieudany — kontynuuje" >&2
  }
done

echo ""
echo "Gotowe — artefakty w $SCRIPT_DIR/output/"
ls -lh "$SCRIPT_DIR/output/" 2>/dev/null || true
