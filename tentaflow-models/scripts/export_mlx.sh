#!/bin/bash
# =============================================================================
# Plik: export_mlx.sh
# Opis: Konwersja modelu HF (full fine-tune) do formatu MLX 4-bit (affine)
#       dla Apple Silicon / mlx-swift. Swiadomie NIE uzywa nvfp4 — na Apple nie
#       ma sprzetowej akceleracji FP4, a implementacja nvfp4 w MLX klipuje skale
#       (signed E4M3 zamiast UE4M3, patrz ml-explore/mlx#2962). Affine 4-bit jest
#       dojrzaly i daje lepsza/rownorzedna jakosc przy tym samym rozmiarze.
# Wymaga: .venv-mlx (mlx[cpu] + mlx-lm) na Linux, albo python3 z mlx_lm na Macu.
# Uzycie: ./scripts/export_mlx.sh [sciezka-modelu-hf] [nazwa-output]
#   ./scripts/export_mlx.sh                                    # domyslnie qwen-guard-full
#   ./scripts/export_mlx.sh output/qwen-guard-full guard-mlx
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

HF_PATH="${1:-$ROOT/output/qwen-guard-full}"
OUTPUT_NAME="${2:-qwen-guard-mlx-4bit}"
MLX_OUT="$ROOT/output/$OUTPUT_NAME"

Q_BITS=4
Q_GROUP_SIZE=64

# Wybor interpretera z mlx_lm: na Linux dedykowany .venv-mlx, na Macu globalny python3.
if [ -x "$ROOT/.venv-mlx/bin/python" ]; then
    PY="$ROOT/.venv-mlx/bin/python"
elif python3 -c "import mlx_lm" 2>/dev/null; then
    PY="python3"
else
    echo "BLAD: brak mlx_lm."
    echo "  Linux: uv pip install --python .venv-mlx 'mlx[cpu]' mlx-lm"
    echo "  macOS: pip install mlx-lm"
    exit 1
fi

if [ ! -d "$HF_PATH" ]; then
    echo "BLAD: nie znaleziono modelu HF: $HF_PATH"
    echo "  Najpierw dokoncz full fine-tune: python3 scripts/train.py guard --method full"
    exit 1
fi

echo "=========================================="
echo "  Eksport MLX 4-bit (affine)"
echo "  Model HF: $HF_PATH"
echo "  Output:   $MLX_OUT"
echo "  Bity:     $Q_BITS | group_size: $Q_GROUP_SIZE"
echo "  Python:   $PY"
echo "=========================================="

rm -rf "$MLX_OUT"
"$PY" -m mlx_lm convert \
    --hf-path "$HF_PATH" \
    -q --q-bits "$Q_BITS" --q-group-size "$Q_GROUP_SIZE" \
    --mlx-path "$MLX_OUT"

echo ""
echo "=========================================="
echo "  Gotowe! MLX 4-bit: $MLX_OUT ($(du -sh "$MLX_OUT" | cut -f1))"
echo "  Skopiuj caly katalog na Apple (mlx-swift) lub do bundla iOS/macOS."
echo "=========================================="
