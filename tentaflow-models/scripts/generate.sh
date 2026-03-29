#!/bin/bash
# =============================================================================
# Plik: generate.sh
# Opis: Generuje dane treningowe za pomoca Claude CLI.
# Uzycie:
#   ./generate.sh                    — wszystkie datasety
#   ./generate.sh guard              — guard short + extended
#   ./generate.sh guard short        — guard short only
#   ./generate.sh guard extended     — guard extended only
#   ./generate.sh toolcalling        — toolcalling
#   ./generate.sh memory             — memory
# Opcjonalnie: ./generate.sh guard short 10  — 10 iteracji
# =============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FILTER="$SCRIPT_DIR/filter_jsonl.py"

DATASET="${1:-all}"
SUBSET="${2:-}"
ITERATIONS="${3:-5}"

# Help
if [ "$DATASET" = "help" ] || [ "$DATASET" = "-h" ] || [ "$DATASET" = "--help" ]; then
    echo "Generowanie danych treningowych za pomoca Claude CLI."
    echo ""
    echo "Uzycie: ./scripts/generate.sh [dataset] [subset/iteracje] [iteracje]"
    echo ""
    echo "Datasety:"
    echo "  all                        — wszystko (domyslne)"
    echo "  intent [N]                 — intent router (50 rec/iter)"
    echo "  guard [N]                  — guard short + extended"
    echo "  guard short [N]            — guard short only (50 rec/iter)"
    echo "  guard extended [N]         — guard extended only (15 rec/iter)"
    echo "  model [N]                  — model router (30 rec/iter)"
    echo "  plan [N]                   — execution planning (15 rec/iter)"
    echo "  check [N]                  — result validation (30 rec/iter)"
    echo "  toolcalling [N]            — tool calling (auto z addonow, 30 rec/iter)"
    echo "  memory [N]                 — wszystkie podtypy memory (round-robin)"
    echo "  memory documents [N]       — podsumowania dokumentow (20 rec/iter)"
    echo "  memory conversations [N]   — podsumowania rozmow z AI (15 rec/iter)"
    echo "  memory rag [N]             — rozmowy z dokumentami (15 rec/iter)"
    echo "  memory transcripts [N]     — transkrypcje spotkan (10 rec/iter)"
    echo "  memory summaries [N]       — hierarchiczne podsumowania L1/L2/TOP (15 rec/iter)"
    echo "  memory extract [N]         — wyciaganie pol z dokumentow (15 rec/iter)"
    echo ""
    echo "N = liczba iteracji (domyslnie 5)"
    echo ""
    echo "Przyklady:"
    echo "  ./scripts/generate.sh guard 20           # guard short+extended, 20 iteracji"
    echo "  ./scripts/generate.sh guard short 50     # guard short, 50 iteracji"
    echo "  ./scripts/generate.sh memory 30          # wszystkie memory, 30 rund"
    echo "  ./scripts/generate.sh memory extract 20  # extract only, 20 iteracji"
    echo "  ./scripts/generate.sh intent 30          # intent router, 30 iteracji"
    exit 0
fi

# Jesli drugi argument to liczba — traktuj jako iteracje, nie subset
if [[ "$SUBSET" =~ ^[0-9]+$ ]]; then
    ITERATIONS="$SUBSET"
    SUBSET=""
fi

generate_dataset() {
    local prompt_file="$1"
    local output_file="$2"
    local name="$3"
    local iters="$4"

    if [ ! -f "$prompt_file" ]; then
        echo "  SKIP: brak promptu $prompt_file"
        return
    fi

    touch "$output_file"

    for ((i=1; i<=iters; i++)); do
        before=$(wc -l < "$output_file")
        claude -p --dangerously-skip-permissions < "$prompt_file" 2>/dev/null | python3 "$FILTER" >> "$output_file"
        after=$(wc -l < "$output_file")
        if [ "$iters" -gt 1 ]; then
            echo "  [$i/$iters] $name: +$((after - before)) (total: $after)"
        else
            echo "  $name: +$((after - before)) (total: $after)"
        fi
    done
}

echo "=========================================="
echo "Generowanie danych ($ITERATIONS iteracji)"
echo "=========================================="

# Guard
if [ "$DATASET" = "all" ] || [ "$DATASET" = "guard" ]; then
    if [ "$SUBSET" = "" ] || [ "$SUBSET" = "short" ]; then
        echo ""
        echo "--- Guard Short ---"
        generate_dataset "$ROOT/prompts/guard_short.md" "$ROOT/data/guard/short.jsonl" "guard-short" "$ITERATIONS"
    fi
    if [ "$SUBSET" = "" ] || [ "$SUBSET" = "extended" ]; then
        echo ""
        echo "--- Guard Extended ---"
        generate_dataset "$ROOT/prompts/guard_extended.md" "$ROOT/data/guard/extended.jsonl" "guard-extended" "$ITERATIONS"
    fi
fi

# Intent router
if [ "$DATASET" = "all" ] || [ "$DATASET" = "intent" ]; then
    echo ""
    echo "--- Intent Router ---"
    generate_dataset "$ROOT/prompts/intent_router.md" "$ROOT/data/intent/raw.jsonl" "intent" "$ITERATIONS"
fi

# Model router
if [ "$DATASET" = "all" ] || [ "$DATASET" = "model" ]; then
    echo ""
    echo "--- Model Router ---"
    generate_dataset "$ROOT/prompts/model_router.md" "$ROOT/data/model/raw.jsonl" "model" "$ITERATIONS"
fi

# Plan
if [ "$DATASET" = "all" ] || [ "$DATASET" = "plan" ]; then
    echo ""
    echo "--- Plan ---"
    generate_dataset "$ROOT/prompts/plan.md" "$ROOT/data/plan/raw.jsonl" "plan" "$ITERATIONS"
fi

# Check
if [ "$DATASET" = "all" ] || [ "$DATASET" = "check" ]; then
    echo ""
    echo "--- Check ---"
    generate_dataset "$ROOT/prompts/check.md" "$ROOT/data/check/raw.jsonl" "check" "$ITERATIONS"
fi

# Toolcalling (automatycznie z addonow)
if [ "$DATASET" = "all" ] || [ "$DATASET" = "toolcalling" ]; then
    echo ""
    echo "--- Toolcalling (auto z addonow) ---"
    python3 "$SCRIPT_DIR/generate_toolcalling.py" --iterations "$ITERATIONS"
fi

# Memory
if [ "$DATASET" = "all" ] || [ "$DATASET" = "memory" ]; then
    if [ -n "$SUBSET" ]; then
        # Konkretny podtyp
        case "$SUBSET" in
            documents)    generate_dataset "$ROOT/prompts/memory_documents.md" "$ROOT/data/memory/documents.jsonl" "memory-docs" "$ITERATIONS" ;;
            conversations) generate_dataset "$ROOT/prompts/memory_conversations.md" "$ROOT/data/memory/conversations.jsonl" "memory-conv" "$ITERATIONS" ;;
            rag)          generate_dataset "$ROOT/prompts/memory_rag.md" "$ROOT/data/memory/rag.jsonl" "memory-rag" "$ITERATIONS" ;;
            transcripts)  generate_dataset "$ROOT/prompts/memory_transcripts.md" "$ROOT/data/memory/transcripts.jsonl" "memory-trans" "$ITERATIONS" ;;
            summaries)    generate_dataset "$ROOT/prompts/memory_summaries.md" "$ROOT/data/memory/summaries.jsonl" "memory-sum" "$ITERATIONS" ;;
            extract)      generate_dataset "$ROOT/prompts/memory_extract.md" "$ROOT/data/memory/extract.jsonl" "memory-extract" "$ITERATIONS" ;;
            *)            echo "Nieznany subset memory: $SUBSET" ;;
        esac
    else
        # Wszystkie podtypy — 1 iteracja kazdego w petli
        echo ""
        echo "--- Memory: wszystkie podtypy (po 1 iteracji w petli) ---"
        for ((i=1; i<=ITERATIONS; i++)); do
            echo ""
            echo "[$i/$ITERATIONS] Memory round"
            generate_dataset "$ROOT/prompts/memory_documents.md" "$ROOT/data/memory/documents.jsonl" "docs" 1
            generate_dataset "$ROOT/prompts/memory_conversations.md" "$ROOT/data/memory/conversations.jsonl" "conv" 1
            generate_dataset "$ROOT/prompts/memory_rag.md" "$ROOT/data/memory/rag.jsonl" "rag" 1
            generate_dataset "$ROOT/prompts/memory_transcripts.md" "$ROOT/data/memory/transcripts.jsonl" "trans" 1
            generate_dataset "$ROOT/prompts/memory_summaries.md" "$ROOT/data/memory/summaries.jsonl" "sum" 1
            generate_dataset "$ROOT/prompts/memory_extract.md" "$ROOT/data/memory/extract.jsonl" "extract" 1
        done
    fi
fi

# Podsumowanie
echo ""
echo "=========================================="
echo "Podsumowanie:"
for f in "$ROOT"/data/intent/raw.jsonl "$ROOT"/data/guard/short.jsonl "$ROOT"/data/guard/extended.jsonl "$ROOT"/data/model/raw.jsonl "$ROOT"/data/plan/raw.jsonl "$ROOT"/data/check/raw.jsonl "$ROOT"/data/toolcalling/raw.jsonl "$ROOT"/data/memory/documents.jsonl "$ROOT"/data/memory/conversations.jsonl "$ROOT"/data/memory/rag.jsonl "$ROOT"/data/memory/transcripts.jsonl "$ROOT"/data/memory/summaries.jsonl "$ROOT"/data/memory/extract.jsonl; do
    if [ -f "$f" ] && [ -s "$f" ]; then
        name=$(echo "$f" | sed "s|$ROOT/data/||")
        echo "  $name: $(wc -l < "$f") rekordow"
    fi
done
echo "=========================================="
