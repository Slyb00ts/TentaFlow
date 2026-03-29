#!/bin/bash
# =============================================================================
# Plik: export_gguf.sh
# Opis: Merge LoRA z bazowym modelem i eksport do GGUF Q4_K_M.
# Wymaga: llama.cpp zainstalowane (llama-quantize w PATH)
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MODELS_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
VENV="$MODELS_DIR/.venv/bin/activate"

BASE_MODEL="$MODELS_DIR/models/qwen3.5-0.8b-base"
LORA_PATH="${1:?Użycie: ./export_gguf.sh <ścieżka-do-lora> [nazwa-output]}"
OUTPUT_NAME="${2:-tentaflow-model}"

MERGED_DIR="$MODELS_DIR/output/merged-${OUTPUT_NAME}"
GGUF_F16="$MODELS_DIR/output/${OUTPUT_NAME}-f16.gguf"
GGUF_Q4="$MODELS_DIR/output/${OUTPUT_NAME}-Q4_K_M.gguf"

echo "=========================================="
echo "Eksport do GGUF Q4_K_M"
echo "=========================================="
echo "Bazowy model: $BASE_MODEL"
echo "LoRA: $LORA_PATH"
echo "Output: $GGUF_Q4"
echo ""

# 1. Merge LoRA z bazowym modelem
echo "[1/3] Merge LoRA z bazowym modelem..."
source "$VENV"
python3 -c "
from peft import PeftModel
from transformers import AutoModelForCausalLM, AutoTokenizer
import torch

print('Ladowanie bazowego modelu...')
model = AutoModelForCausalLM.from_pretrained('$BASE_MODEL', torch_dtype=torch.float16)
tokenizer = AutoTokenizer.from_pretrained('$LORA_PATH')

print('Ladowanie LoRA...')
model = PeftModel.from_pretrained(model, '$LORA_PATH')

print('Merge...')
model = model.merge_and_unload()

print('Zapis merged modelu...')
model.save_pretrained('$MERGED_DIR')
tokenizer.save_pretrained('$MERGED_DIR')
print('Merged model zapisany do $MERGED_DIR')
"

# 2. Konwersja do GGUF float16
echo ""
echo "[2/3] Konwersja do GGUF F16..."
if command -v convert_hf_to_gguf.py &> /dev/null; then
    convert_hf_to_gguf.py "$MERGED_DIR" --outfile "$GGUF_F16" --outtype f16
elif [ -f "$HOME/llama.cpp/convert_hf_to_gguf.py" ]; then
    python3 "$HOME/llama.cpp/convert_hf_to_gguf.py" "$MERGED_DIR" --outfile "$GGUF_F16" --outtype f16
else
    echo "BŁĄD: Nie znaleziono convert_hf_to_gguf.py"
    echo "Zainstaluj llama.cpp: git clone https://github.com/ggml-org/llama.cpp ~/llama.cpp && cd ~/llama.cpp && cmake -B build && cmake --build build"
    exit 1
fi

# 3. Kwantyzacja Q4_K_M
echo ""
echo "[3/3] Kwantyzacja Q4_K_M..."
if command -v llama-quantize &> /dev/null; then
    llama-quantize "$GGUF_F16" "$GGUF_Q4" Q4_K_M
elif [ -f "$HOME/llama.cpp/build/bin/llama-quantize" ]; then
    "$HOME/llama.cpp/build/bin/llama-quantize" "$GGUF_F16" "$GGUF_Q4" Q4_K_M
else
    echo "BŁĄD: Nie znaleziono llama-quantize"
    echo "Zbuduj llama.cpp: cd ~/llama.cpp && cmake -B build && cmake --build build"
    exit 1
fi

# Podsumowanie
echo ""
echo "=========================================="
echo "Eksport zakończony!"
echo "F16:    $GGUF_F16 ($(du -h "$GGUF_F16" | cut -f1))"
echo "Q4_K_M: $GGUF_Q4 ($(du -h "$GGUF_Q4" | cut -f1))"
echo "=========================================="
