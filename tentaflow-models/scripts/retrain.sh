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
FRACTION="1.0"
BALANCE=false
QUANT_LEVELS="Q8_0 Q6_K Q5_K_M Q4_K_M Q3_K_M Q2_K"

# Parsuj argumenty
for arg in "$@"; do
    case "$arg" in
        --fresh) FRESH=true ;;
        --method=*) METHOD="${arg#--method=}" ;;
        --gpus=*) GPUS="${arg#--gpus=}" ;;
        --fraction=*) FRACTION="${arg#--fraction=}" ;;
        --balance) BALANCE=true ;;
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
            echo "  batch          — Trenuj 8 modeli testowych (guard low/med/high + llama + all x4 metody)"
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
            echo "  --fraction=N — Frakcja danych treningowych (0.0-1.0, domyslnie 1.0)"
            echo "  --balance    — Zrownowaz datasety (cap do mediany)"
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

# =============================================================================
# Funkcja: merge_and_export
# Opis: Merge LoRA + konwersja GGUF + kwantyzacja. Wspolna logika dla
#        pojedynczego i batch trybu.
# =============================================================================
merge_and_export() {
    local lora_name="$1"
    local method="$2"
    local quant_level="${3:-Q5_K_M}"

    local lora_dir="$ROOT/output/$lora_name"
    local merged_dir="$ROOT/output/${lora_name}-merged"
    local f16_gguf="$ROOT/output/${lora_name}-f16.gguf"

    # Sprawdz czy GGUF juz istnieje i fingerprint pasuje (skip)
    local all_exist=true
    for quant in $quant_level; do
        if [ ! -f "$ROOT/output/${lora_name}-${quant}.gguf" ]; then
            all_exist=false
            break
        fi
    done
    if $all_exist && [ -f "$lora_dir/.data_fingerprint" ]; then
        echo "  SKIP merge_and_export: GGUF juz istnieje dla $lora_name"
        return 0
    fi

    # Merge (pomijamy dla full fine-tune — model juz jest kompletny)
    if [ "$method" = "full" ]; then
        echo "  Full fine-tune — model juz jest kompletny, pomijam merge."
        merged_dir="$lora_dir"
    else
        echo "  Merge LoRA/DoRA z bazowym modelem..."
        python3 -c "
from peft import PeftModel
from transformers import Qwen3_5ForConditionalGeneration, AutoTokenizer
import torch

model = Qwen3_5ForConditionalGeneration.from_pretrained(
    '$ROOT/models/qwen3.5-0.8b-base', trust_remote_code=True, dtype=torch.float16)
tokenizer = AutoTokenizer.from_pretrained('$lora_dir', trust_remote_code=True)
model.resize_token_embeddings(len(tokenizer))
model = PeftModel.from_pretrained(model, '$lora_dir')
model = model.merge_and_unload()
model.save_pretrained('$merged_dir')
tokenizer.save_pretrained('$merged_dir')
print('Merge OK: $merged_dir')
"
    fi

    # Konwersja do GGUF F16
    echo "  Konwersja do GGUF F16..."
    python3 "$CONVERT" "$merged_dir" --outfile "$f16_gguf" --outtype f16

    # Kwantyzacja
    for quant in $quant_level; do
        local out_gguf="$ROOT/output/${lora_name}-${quant}.gguf"
        echo "  Kwantyzacja $quant..."
        $QUANTIZE "$f16_gguf" "$out_gguf" "$quant" 2>&1 | tail -1
        echo "  Output: $out_gguf ($(du -h "$out_gguf" | cut -f1))"
    done

    # Posprzataj F16
    rm -f "$f16_gguf"
}

# =============================================================================
# Tryb batch: 8 modeli sekwencyjnie
# =============================================================================
if [ "$TASK" = "batch" ]; then
    echo "=========================================="
    echo "  BATCH: 8 modeli treningowych"
    echo "  GPU: $GPUS"
    echo "=========================================="

    BATCH_QUANT="Q5_K_M"

    # Konwersja danych — raz na poczatku
    echo ""
    echo "[BATCH] Konwersja danych guard..."
    python3 "$SCRIPT_DIR/convert.py" guard

    # --- 1. Qwen guard LOW (1/3 danych, qlora) ---
    echo ""
    echo "[1/8] Qwen guard LOW (fraction=0.33, qlora)..."
    python3 "$SCRIPT_DIR/train.py" guard --method qlora --fraction 0.33 --gpus "$GPUS"
    merge_and_export "qwen-guard-qlora-low" "qlora" "$BATCH_QUANT"

    # --- 2. Qwen guard MEDIUM (2/3 danych, qlora) ---
    echo ""
    echo "[2/8] Qwen guard MEDIUM (fraction=0.66, qlora)..."
    python3 "$SCRIPT_DIR/train.py" guard --method qlora --fraction 0.66 --gpus "$GPUS"
    merge_and_export "qwen-guard-qlora-medium" "qlora" "$BATCH_QUANT"

    # --- 3. Qwen guard HIGH (pelny dataset, qlora) ---
    echo ""
    echo "[3/8] Qwen guard HIGH (fraction=1.0, qlora)..."
    python3 "$SCRIPT_DIR/train.py" guard --method qlora --gpus "$GPUS"
    # train.py zapisuje do qwen-guard-qlora (bez suffixu bo fraction=1.0)
    # Rename dla spojnej nazwy z low/medium
    if [ -d "$ROOT/output/qwen-guard-qlora" ]; then
        mv "$ROOT/output/qwen-guard-qlora" "$ROOT/output/qwen-guard-qlora-high"
    fi
    merge_and_export "qwen-guard-qlora-high" "qlora" "$BATCH_QUANT"

    # --- 4. Llama guard (caly dataset guard short) ---
    echo ""
    echo "[4/8] Llama guard..."
    python3 "$SCRIPT_DIR/train.py" guard --model llama

    # Konwersja danych all — raz przed 4 treningami
    echo ""
    echo "[BATCH] Konwersja danych all..."
    python3 "$SCRIPT_DIR/convert.py" intent
    python3 "$SCRIPT_DIR/convert.py" guard
    python3 "$SCRIPT_DIR/convert.py" model
    python3 "$SCRIPT_DIR/convert.py" plan
    python3 "$SCRIPT_DIR/convert.py" check
    python3 "$SCRIPT_DIR/convert.py" toolcalling
    python3 "$SCRIPT_DIR/convert.py" memory

    # --- 5-8. Qwen all (balanced) z 4 metodami ---
    BATCH_IDX=5
    for method in lora qlora dora full; do
        echo ""
        echo "[$BATCH_IDX/8] Qwen all ($method, balanced)..."
        python3 "$SCRIPT_DIR/train.py" all --method "$method" --balance --gpus "$GPUS"
        merge_and_export "qwen-all-${method}" "$method" "$BATCH_QUANT"
        BATCH_IDX=$((BATCH_IDX + 1))
    done

    # Podsumowanie batch
    echo ""
    echo "=========================================="
    echo "  BATCH ZAKONCZONE - 8 modeli"
    echo "=========================================="
    ls -lh "$ROOT/output/"*-Q5_K_M.gguf 2>/dev/null | awk '{print "  " $NF ": " $5}'

    exit 0
fi

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

# Budowanie nazwy LoRA: qwen-{task}-{method}[-fraction_suffix]
LORA_NAME="qwen-${TASK}-${METHOD}"

# Dodaj suffix frakcji jesli jawnie ustawiona (nie domyslna)
if [ "$FRACTION" != "1.0" ]; then
    # train.py uzywa tych samych progow: <=0.34 = low, <=0.67 = medium, >0.67 = high
    if (( $(echo "$FRACTION <= 0.34" | bc -l) )); then
        LORA_NAME="${LORA_NAME}-low"
    elif (( $(echo "$FRACTION <= 0.67" | bc -l) )); then
        LORA_NAME="${LORA_NAME}-medium"
    else
        LORA_NAME="${LORA_NAME}-high"
    fi
fi

LORA_DIR="$ROOT/output/$LORA_NAME"
MERGED_DIR="$ROOT/output/${LORA_NAME}-merged"
F16_GGUF="$ROOT/output/${LORA_NAME}-f16.gguf"

echo "=========================================="
echo "  Retrain: $TASK"
echo "  Method:  $METHOD"
echo "  GPUs:    $GPUS"
echo "  Model:   $LORA_NAME"
echo "  Fresh:   $FRESH"
echo "  Fraction: $FRACTION"
echo "  Balance: $BALANCE"
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

# Zbuduj komende treningowa z opcjonalnymi flagami
TRAIN_CMD="python3 $SCRIPT_DIR/train.py $TRAIN_TASK --method $METHOD --gpus $GPUS"
if [ "$FRACTION" != "1.0" ]; then
    TRAIN_CMD="$TRAIN_CMD --fraction $FRACTION"
fi
if [ "$BALANCE" = true ]; then
    TRAIN_CMD="$TRAIN_CMD --balance"
fi

if [ -n "$LAST_CKPT" ]; then
    echo "[2/5] Kontynuacja treningu (checkpoint: $(basename $LAST_CKPT))..."
    $TRAIN_CMD --resume "$LAST_CKPT"
else
    echo "[2/5] Trening od zera..."
    $TRAIN_CMD
fi

# 3-5. Merge + GGUF + kwantyzacja
echo ""
echo "[3-5/5] Merge, konwersja GGUF i kwantyzacja..."
merge_and_export "$LORA_NAME" "$METHOD" "$QUANT_LEVELS"

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
