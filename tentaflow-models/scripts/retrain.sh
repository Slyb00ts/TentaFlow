#!/bin/bash
# =============================================================================
# Plik: retrain.sh
# Opis: Pelny pipeline: convert → train → merge LoRA → GGUF → kwantyzacje.
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
VENV="$ROOT/.venv/bin/activate"
QUANTIZE="$HOME/llama.cpp/build/bin/llama-quantize"
CONVERT="$HOME/llama.cpp/convert_hf_to_gguf.py"

TASK="all"
FRESH=false
METHOD="qlora"
GPUS=1

# Parsuj argumenty
for arg in "$@"; do
    case "$arg" in
        --fresh) FRESH=true ;;
        --method=*) METHOD="${arg#--method=}" ;;
        --gpus=*) GPUS="${arg#--gpus=}" ;;
        qlora|lora|full|dora) METHOD="$arg" ;;
        help|--help|-h)
            echo "Pelny pipeline: convert → train → merge LoRA → GGUF → kwantyzacje."
            echo ""
            echo "Uzycie: ./scripts/retrain.sh [task] [--fresh]"
            echo ""
            echo "Tasks:"
            echo "  all            — Qwen ze WSZYSTKIM (domyslne)"
            echo "  guard          — Qwen TYLKO guard (osobny model: qwen-guard)"
            echo "  orchestrator   — Qwen BEZ guard (osobny model: qwen-orchestrator)"
            echo "  intent         — Qwen intent only"
            echo "  model          — Qwen model router only"
            echo "  plan           — Qwen planowanie only"
            echo "  check          — Qwen walidacja only"
            echo "  toolcalling    — Qwen tool calling only"
            echo "  memory         — Qwen memory only"
            echo "  llama-guard    — Llama-Prompt-Guard-2-86M na guard (short only)"
            echo ""
            echo "Strategie 2-modelowe:"
            echo "  ./scripts/retrain.sh guard --fresh            # guard QLoRA"
            echo "  ./scripts/retrain.sh orchestrator --fresh    # orchestrator QLoRA"
            echo "  ./scripts/retrain.sh guard full --fresh              # guard full FT, 1 GPU"
            echo "  ./scripts/retrain.sh --fresh full --gpus=7          # all full FT, 7 GPU"
            echo "  ./scripts/retrain.sh orchestrator dora --gpus=7     # orchestrator DoRA, 7 GPU"
            echo "  ./scripts/retrain.sh llama-guard --fresh            # Llama Prompt Guard"
            echo ""
            echo "Strategia 1-modelowa:"
            echo "  ./scripts/retrain.sh --fresh              # jeden model: qwen-all"
            echo ""
            echo "Metody treningu:"
            echo "  qlora        — 4-bit + LoRA adapter (domyslna, ~8GB VRAM)"
            echo "  lora         — BF16 + LoRA adapter (~16GB VRAM)"
            echo "  dora         — BF16 + DoRA adapter (~18GB VRAM)"
            echo "  full         — pelny fine-tune calego modelu (~24GB VRAM)"
            echo ""
            echo "Opcje:"
            echo "  --fresh      — trening od zera (usun stara LoRA)"
            echo "               Bez --fresh: kontynuuj z ostatniego checkpointu"
            echo "  --gpus=N     — ile GPU uzyc (domyslnie: 1, >1 = DeepSpeed)"
            echo ""
            echo "Wynikowe modele (output/):"
            echo "  qwen-all-*           — jeden model ze wszystkim"
            echo "  qwen-guard-*         — tylko guard"
            echo "  qwen-orchestrator-*  — wszystko bez guard"
            exit 0
            ;;
        *) TASK="$arg" ;;
    esac
done

source "$VENV"

# --- Llama Guard: osobna sciezka ---
if [ "$TASK" = "llama-guard" ]; then
    LLAMA_DIR="$ROOT/output/llama-guard"

    echo "=========================================="
    echo "  Retrain: Llama-Prompt-Guard-2-86M"
    echo "  Dataset: guard short only"
    echo "  Fresh:   $FRESH"
    echo "=========================================="

    if [ "$FRESH" = true ] && [ -d "$LLAMA_DIR" ]; then
        echo "[0] Usuwanie starego modelu..."
        rm -rf "$LLAMA_DIR"
    fi

    echo "[1/2] Konwersja danych guard..."
    python3 "$SCRIPT_DIR/convert.py" guard

    echo "[2/2] Trening Llama Prompt Guard..."
    python3 "$SCRIPT_DIR/train.py" guard --model llama

    echo ""
    echo "=========================================="
    echo "  Gotowe! Llama Guard"
    echo "  Output: $LLAMA_DIR"
    echo "=========================================="
    exit 0
fi

# --- Qwen: glowna sciezka ---
case "$TASK" in
    all)          LORA_NAME="qwen-all-lora" ;;
    guard)        LORA_NAME="qwen-guard-lora" ;;
    orchestrator) LORA_NAME="qwen-orchestrator-lora" ;;
    *)            LORA_NAME="qwen-${TASK}-lora" ;;
esac

LORA_DIR="$ROOT/output/$LORA_NAME"
MERGED_DIR="$ROOT/output/${LORA_NAME}-merged"
F16_GGUF="$ROOT/output/${LORA_NAME}-f16.gguf"

echo "=========================================="
echo "  Retrain: $TASK"
echo "  Method:  $METHOD"
echo "  GPUs:    $GPUS"
echo "  Model:   $LORA_NAME"
echo "  Fresh:   $FRESH"
echo "=========================================="

# 0. Fresh
if [ "$FRESH" = true ] && [ -d "$LORA_DIR" ]; then
    echo ""
    echo "[0/5] Usuwanie starej LoRA ($LORA_DIR)..."
    rm -rf "$LORA_DIR"
fi

# 1. Konwersja danych
echo ""
echo "[1/5] Konwersja danych..."
case "$TASK" in
    all)
        python3 "$SCRIPT_DIR/convert.py" intent
        python3 "$SCRIPT_DIR/convert.py" guard
        python3 "$SCRIPT_DIR/convert.py" model
        python3 "$SCRIPT_DIR/convert.py" plan
        python3 "$SCRIPT_DIR/convert.py" check
        python3 "$SCRIPT_DIR/convert.py" toolcalling
        python3 "$SCRIPT_DIR/convert.py" memory
        ;;
    orchestrator)
        python3 "$SCRIPT_DIR/convert.py" intent
        python3 "$SCRIPT_DIR/convert.py" model
        python3 "$SCRIPT_DIR/convert.py" plan
        python3 "$SCRIPT_DIR/convert.py" check
        python3 "$SCRIPT_DIR/convert.py" toolcalling
        python3 "$SCRIPT_DIR/convert.py" memory
        ;;
    *)
        python3 "$SCRIPT_DIR/convert.py" "$TASK"
        ;;
esac

# 2. Trening
echo ""
TRAIN_TASK="$TASK"

# Znajdz checkpoint do resume
LAST_CKPT=""
if [ -d "$LORA_DIR" ] && [ "$FRESH" = false ]; then
    LAST_CKPT=$(ls -d "$LORA_DIR"/checkpoint-* 2>/dev/null | sort -t- -k2 -n | tail -1 || echo "")
fi

if [ -n "$LAST_CKPT" ]; then
    echo "[2/5] Kontynuacja treningu (checkpoint: $(basename $LAST_CKPT))..."
    python3 "$SCRIPT_DIR/train.py" "$TRAIN_TASK" --method "$METHOD" --gpus "$GPUS" --gpus "$GPUS" --resume "$LAST_CKPT"
else
    echo "[2/5] Trening od zera..."
    python3 "$SCRIPT_DIR/train.py" "$TRAIN_TASK" --method "$METHOD" --gpus "$GPUS"
fi

# 3. Merge LoRA (lub kopiuj dla full fine-tune)
echo ""
if [ "$METHOD" = "full" ]; then
    echo "[3/5] Full fine-tune — model juz jest kompletny, kopiuje..."
    MERGED_DIR="$LORA_DIR"
else
    echo "[3/5] Merge LoRA/DoRA z bazowym modelem..."
    python3 -c "
from peft import PeftModel
from transformers import Qwen3_5ForConditionalGeneration, AutoTokenizer
import torch

model = Qwen3_5ForConditionalGeneration.from_pretrained(
    '$ROOT/models/qwen3.5-0.8b-base', trust_remote_code=True, dtype=torch.float16)
tokenizer = AutoTokenizer.from_pretrained('$LORA_DIR', trust_remote_code=True)
model.resize_token_embeddings(len(tokenizer))
model = PeftModel.from_pretrained(model, '$LORA_DIR')
model = model.merge_and_unload()
model.save_pretrained('$MERGED_DIR')
tokenizer.save_pretrained('$MERGED_DIR')
print('Merge OK: $MERGED_DIR')
"
fi

# 4. Konwersja do GGUF F16
echo ""
echo "[4/5] Konwersja do GGUF F16..."
python3 "$CONVERT" "$MERGED_DIR" --outfile "$F16_GGUF" --outtype f16

# 5. Kwantyzacje
echo ""
echo "[5/5] Kwantyzacje..."
for QUANT in Q8_0 Q6_K Q5_K_M Q4_K_M Q3_K_M Q2_K; do
    OUT="$ROOT/output/${LORA_NAME}-${QUANT}.gguf"
    $QUANTIZE "$F16_GGUF" "$OUT" "$QUANT" 2>&1 | tail -1
    SIZE=$(du -h "$OUT" | cut -f1)
    echo "  $QUANT: $SIZE"
done

# Podsumowanie
echo ""
echo "=========================================="
echo "  Gotowe!"
echo "  Task:  $TASK"
echo "  Model: $LORA_NAME"
echo "=========================================="
ls -lh "$ROOT/output/${LORA_NAME}-"*.gguf | awk '{print "  " $NF ": " $5}'
echo ""
echo "Benchmark: python3 scripts/benchmark_all.py --gguf output/${LORA_NAME}-Q5_K_M.gguf"
