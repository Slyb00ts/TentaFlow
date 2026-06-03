#!/bin/bash
# =============================================================================
# Plik: export_q5.sh
# Opis: Konwersja modelu HF (full fine-tune) do GGUF Q5_K_M dla llama.cpp.
#       W przeciwienstwie do export_gguf.sh NIE merguje LoRA — full fine-tune
#       jest juz kompletnym modelem, wiec idziemy wprost: HF -> GGUF F16 -> Q5.
# Wymaga: llama.cpp (convert_hf_to_gguf.py + llama-quantize w PATH lub ~/llama.cpp).
# Uzycie: ./scripts/export_q5.sh [sciezka-modelu-hf] [nazwa-output] [poziom-kwant]
#   ./scripts/export_q5.sh                                  # qwen-guard-full -> Q5_K_M
#   ./scripts/export_q5.sh output/qwen-guard-full guard Q5_K_M
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

HF_PATH="${1:-$ROOT/output/qwen-guard-full}"
OUTPUT_NAME="${2:-qwen-guard}"
QUANT="${3:-Q5_K_M}"

F16_GGUF="$ROOT/output/${OUTPUT_NAME}-f16.gguf"
OUT_GGUF="$ROOT/output/${OUTPUT_NAME}-${QUANT}.gguf"

# Interpreter z transformers + torch — convert_hf_to_gguf.py wymaga obu.
# Preferuj .venv-nvfp4 (transformers 5.x + torch), potem treningowy .venv, potem python3.
PY=""
for cand in "$ROOT/.venv-nvfp4/bin/python" "$ROOT/.venv/bin/python" python3; do
    if "$cand" -c "import transformers, torch" 2>/dev/null; then PY="$cand"; break; fi
done
if [ -z "$PY" ]; then
    echo "BLAD: nie znaleziono pythona z 'transformers' + 'torch' (potrzebne do konwersji GGUF)"
    exit 1
fi

# Lokalizacja convert_hf_to_gguf.py z llama.cpp
if [ -f "$HOME/llama.cpp/convert_hf_to_gguf.py" ]; then
    CONVERT="$PY $HOME/llama.cpp/convert_hf_to_gguf.py"
else
    echo "BLAD: brak convert_hf_to_gguf.py (zainstaluj llama.cpp w ~/llama.cpp)"
    exit 1
fi

if command -v llama-quantize &>/dev/null; then
    QUANTIZE="llama-quantize"
elif [ -f "$HOME/llama.cpp/build/bin/llama-quantize" ]; then
    QUANTIZE="$HOME/llama.cpp/build/bin/llama-quantize"
else
    echo "BLAD: brak llama-quantize (zbuduj llama.cpp: cmake -B build && cmake --build build)"
    exit 1
fi

if [ ! -d "$HF_PATH" ]; then
    echo "BLAD: nie znaleziono modelu HF: $HF_PATH"
    echo "  Najpierw dokoncz full fine-tune: python3 scripts/train.py guard --method full"
    exit 1
fi

echo "=========================================="
echo "  Eksport GGUF $QUANT"
echo "  Model HF: $HF_PATH"
echo "  Output:   $OUT_GGUF"
echo "=========================================="

echo "[1/2] Konwersja HF -> GGUF F16..."
$CONVERT "$HF_PATH" --outfile "$F16_GGUF" --outtype f16

echo "[2/2] Kwantyzacja $QUANT..."
$QUANTIZE "$F16_GGUF" "$OUT_GGUF" "$QUANT"
rm -f "$F16_GGUF"

echo ""
echo "=========================================="
echo "  Gotowe! GGUF: $OUT_GGUF ($(du -h "$OUT_GGUF" | cut -f1))"
echo "=========================================="
