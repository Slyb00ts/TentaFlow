#!/usr/bin/env python3
# =============================================================================
# Plik: export_nvfp4.py
# Opis: Kwantyzacja modelu HF (full fine-tune) do NVFP4 dla vLLM przez
#       llm-compressor (oneshot PTQ). Kalibracja na realnych promptach guard,
#       zeby skale FP4 byly dopasowane do dystrybucji tokenow <|guard|>.
#       Wynik: katalog compressed-tensors (przenosny, ladowany przez vLLM).
#       Na kartach < SM100 (np. RTX 3090) vLLM uruchomi go jako weight-only;
#       pelna akceleracja W4A4 dopiero na Blackwell.
# Wymaga: .venv-nvfp4 (llmcompressor z gita + transformers 5.x + torch CUDA).
# Uzycie:
#   CUDA_VISIBLE_DEVICES=1 .venv-nvfp4/bin/python scripts/export_nvfp4.py
#   .venv-nvfp4/bin/python scripts/export_nvfp4.py --model output/qwen-guard-full \
#       --output output/qwen-guard-nvfp4 --num-samples 512
# =============================================================================
import argparse
import os

from datasets import load_dataset
from transformers import AutoModelForCausalLM, AutoTokenizer

from llmcompressor import oneshot
from llmcompressor.modifiers.quantization import QuantizationModifier

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))


def main():
    parser = argparse.ArgumentParser(description="Eksport NVFP4 dla vLLM (llm-compressor)")
    parser.add_argument("--model", default=os.path.join(ROOT, "output", "qwen-guard-full"),
                        help="Sciezka modelu HF (plaski tekstowy CausalLM z flatten_guard.py)")
    parser.add_argument("--output", default=os.path.join(ROOT, "output", "qwen-guard-nvfp4"),
                        help="Katalog wyjsciowy NVFP4")
    parser.add_argument("--calib", default=os.path.join(ROOT, "data", "guard", "qwen_train.jsonl"),
                        help="Dane kalibracyjne (JSONL z polem 'messages')")
    parser.add_argument("--num-samples", type=int, default=512,
                        help="Liczba probek kalibracyjnych")
    parser.add_argument("--max-seq-len", type=int, default=2048,
                        help="Maks. dlugosc sekwencji kalibracyjnej")
    args = parser.parse_args()

    if not os.path.isdir(args.model):
        raise SystemExit(f"BLAD: nie znaleziono modelu HF: {args.model}\n"
                         "  Najpierw dokoncz full fine-tune: python3 scripts/train.py guard --method full")

    print("=" * 50)
    print("  Eksport NVFP4 (llm-compressor oneshot)")
    print(f"  Model:   {args.model}")
    print(f"  Output:  {args.output}")
    print(f"  Kalibr.: {args.calib} (do {args.num_samples} probek)")
    print("=" * 50)

    print("\nLadowanie modelu i tokenizera...")
    model = AutoModelForCausalLM.from_pretrained(
        args.model, torch_dtype="auto", device_map="auto", trust_remote_code=True,
    )
    tokenizer = AutoTokenizer.from_pretrained(args.model, trust_remote_code=True)

    print("Przygotowanie danych kalibracyjnych (chat template)...")
    ds = load_dataset("json", data_files=args.calib, split="train")
    ds = ds.shuffle(seed=42).select(range(min(args.num_samples, len(ds))))

    def to_text(example):
        return {"text": tokenizer.apply_chat_template(
            example["messages"], tokenize=False, add_generation_prompt=False)}

    def tokenize(example):
        return tokenizer(example["text"], padding=False,
                         max_length=args.max_seq_len, truncation=True,
                         add_special_tokens=False)

    ds = ds.map(to_text, remove_columns=ds.column_names)
    ds = ds.map(tokenize, remove_columns=ds.column_names)

    # NVFP4: wagi + aktywacje w FP4 (W4A4), lm_head zostaje w pelnej precyzji.
    recipe = QuantizationModifier(targets="Linear", scheme="NVFP4", ignore=["lm_head"])

    print("\nKwantyzacja NVFP4 (oneshot)...")
    # processor=tokenizer wymusza sciezke tekstowa — Qwen3.5 jest multimodalny,
    # a AutoProcessor probowalby zaladowac procesor wideo (wymaga torchvision).
    oneshot(
        model=model,
        processor=tokenizer,
        dataset=ds,
        recipe=recipe,
        max_seq_length=args.max_seq_len,
        num_calibration_samples=len(ds),
    )

    print(f"\nZapis do {args.output}...")
    model.save_pretrained(args.output, save_compressed=True)
    tokenizer.save_pretrained(args.output)
    print("Gotowe! Uruchom przez: vllm serve", args.output)


if __name__ == "__main__":
    main()
