#!/usr/bin/env python3
# =============================================================================
# Plik: quantize_nvfp4.py
# Opis: Kwantyzacja modelu HF (safetensors) do NVFP4 przez llm-compressor.
#       Wynik to checkpoint NVFP4 ladowany bezposrednio przez vLLM. NVFP4 ma
#       sprzetowa akceleracje tylko na Blackwell (sm_100/120/121), ale checkpoint
#       laduje sie i na starszych GPU (emulacja) — mniej VRAM, lekko szybszy
#       decode (memory-bound). Dwa schematy:
#         NVFP4    — W4A4 (wagi+aktywacje FP4, skale FP8), wymaga kalibracji.
#         NVFP4A16 — wagi FP4, aktywacje 16-bit, data-free (bez datasetu).
# Przyklad:
#   quantize_nvfp4.py --src /data/src --out /data/nvfp4 --scheme NVFP4
# =============================================================================

import argparse
import sys


def log(msg: str) -> None:
    print(f"[quantize-nvfp4] {msg}", file=sys.stderr, flush=True)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--src", required=True, help="katalog zrodlowy HF (safetensors)")
    ap.add_argument("--out", required=True, help="katalog wyjsciowy NVFP4")
    ap.add_argument(
        "--scheme",
        default="NVFP4",
        choices=["NVFP4", "NVFP4A16"],
        help="NVFP4 = W4A4 (kalibracja), NVFP4A16 = wagi FP4 data-free",
    )
    ap.add_argument("--dataset", default="open_platypus", help="dataset kalibracyjny (tylko NVFP4)")
    ap.add_argument("--num-samples", type=int, default=512)
    ap.add_argument("--max-seq-len", type=int, default=2048)
    args = ap.parse_args()

    from transformers import AutoModelForCausalLM, AutoTokenizer
    from llmcompressor import oneshot
    from llmcompressor.modifiers.quantization import QuantizationModifier

    log(f"laduje model zrodlowy: {args.src}")
    model = AutoModelForCausalLM.from_pretrained(args.src, torch_dtype="auto")
    tokenizer = AutoTokenizer.from_pretrained(args.src)

    # lm_head zostaje w wyzszej precyzji — kwantyzacja glowicy psuje jakosc
    # bez realnego zysku pamieci.
    recipe = QuantizationModifier(targets="Linear", scheme=args.scheme, ignore=["lm_head"])

    oneshot_kwargs = dict(model=model, recipe=recipe, output_dir=args.out)
    if args.scheme == "NVFP4":
        # W4A4 wymaga kalibracji aktywacji na realnych probkach.
        oneshot_kwargs.update(
            dataset=args.dataset,
            num_calibration_samples=args.num_samples,
            max_seq_length=args.max_seq_len,
        )
        log(f"kwantyzacja {args.scheme} z kalibracja ({args.dataset}, {args.num_samples} probek)")
    else:
        log(f"kwantyzacja {args.scheme} (data-free, wagi-only)")

    oneshot(**oneshot_kwargs)
    tokenizer.save_pretrained(args.out)
    log(f"gotowe: {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
