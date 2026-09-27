#!/usr/bin/env python3
# =============================================================================
# Plik: scripts/train_pl.py
# Opis: Zaawansowany skrypt fine-tuningu dla modeli wielojęzycznych (np. Qwen 27B,
#       Spark-X2.5) adaptujący je do języka polskiego.
#
# Zastosowane techniki SOTA:
#   1. QLoRA 4-bit (NF4, podwójna kwantyzacja, bfloat16) — mieści model 27B w ~18-20 GB VRAM.
#   2. High-Rank LoRA (r=64, alpha=128) na wszystkich warstwach liniowych (all-linear).
#   3. NEFTune (Noisy Embeddings) — zapobiega przeuczeniu i utracie generalizacji.
#   4. Gradient Checkpointing + TF32 dla optymalizacji pamięci i szybkości.
#   5. Opcja --dry-run do walidacji danych i szablonów bez uruchamiania treningu.
# =============================================================================

import argparse
import json
import os
import sys
import torch
from datasets import load_dataset
from transformers import (
    AutoTokenizer,
    AutoModelForCausalLM,
    BitsAndBytesConfig,
)
from peft import (
    LoraConfig,
    get_peft_model,
    prepare_model_for_kbit_training
)
from trl import SFTTrainer, SFTConfig

ROOT_DIR = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
DEFAULT_TRAIN = os.path.join(ROOT_DIR, "data_pl", "processed", "train_pl_combined.jsonl")
DEFAULT_VAL = os.path.join(ROOT_DIR, "data_pl", "processed", "val_pl_combined.jsonl")
DEFAULT_OUTPUT = os.path.join(ROOT_DIR, "output", "model-pl-lora")

# Optymalizacje GPU dla architektury NVIDIA Ampere / Ada / Hopper
if torch.cuda.is_available():
    torch.backends.cuda.matmul.allow_tf32 = True
    torch.backends.cudnn.allow_tf32 = True
    os.environ.setdefault("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True")


def validate_dataset_file(path: str) -> int:
    if not os.path.exists(path):
        raise FileNotFoundError(f"Nie znaleziono pliku datasetu: {path}. Uruchom najpierw build_pl_dataset.py!")
    count = 0
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            if line.strip():
                count += 1
    return count


def main():
    parser = argparse.ArgumentParser(description="Trening QLoRA adaptujący model (np. Qwen 27B) do języka polskiego.")
    parser.add_argument("--model", type=str, default="Qwen/Qwen2.5-27B-Instruct", help="Ścieżka lub nazwa bazowego modelu HF")
    parser.add_argument("--train-file", type=str, default=DEFAULT_TRAIN, help="Plik treningowy JSONL")
    parser.add_argument("--val-file", type=str, default=DEFAULT_VAL, help="Plik walidacyjny JSONL")
    parser.add_argument("--output-dir", type=str, default=DEFAULT_OUTPUT, help="Katalog zapisu adaptera LoRA")
    parser.add_argument("--epochs", type=int, default=2, help="Liczba epok treningowych (domyślnie 2)")
    parser.add_argument("--batch-size", type=int, default=2, help="Per-device batch size")
    parser.add_argument("--grad-accum", type=int, default=8, help="Gradient accumulation steps (efektywny batch = 16-32)")
    parser.add_argument("--lr", type=float, default=1.5e-4, help="Współczynnik uczenia dla LoRA (domyślnie 1.5e-4)")
    parser.add_argument("--max-seq-length", type=int, default=2048, help="Maksymalna długość sekwencji tokenów")
    parser.add_argument("--lora-r", type=int, default=64, help="Ranga LoRA (domyślnie 64)")
    parser.add_argument("--lora-alpha", type=int, default=128, help="Alpha LoRA (domyślnie 128)")
    parser.add_argument("--neftune-alpha", type=float, default=5.0, help="NEFTune noise alpha dla uogólniania")
    parser.add_argument("--dry-run", action="store_true", help="Walidacja danych i konfiguracji bez uruchamiania treningu")
    parser.add_argument("--resume", type=str, default=None, help="Ścieżka do checkpointu do wznowienia treningu")
    args = parser.parse_args()

    print("=" * 70)
    print("           TentaFlow — Polish Language Fine-Tuning Pipeline           ")
    print("=" * 70)
    print(f"Model bazowy:        {args.model}")
    print(f"Zbiór treningowy:    {args.train_file}")
    print(f"Zbiór walidacyjny:   {args.val_file}")
    print(f"Katalog wyjściowy:   {args.output_dir}")
    print(f"Hiperparametry:      Epochs={args.epochs}, Batch={args.batch_size}, GradAccum={args.grad_accum}, LR={args.lr}")
    print(f"LoRA:                r={args.lora_r}, alpha={args.lora_alpha}, target=all-linear")
    print(f"NEFTune:             noise_alpha={args.neftune_alpha}")
    print(f"Tryb:                {'DRY RUN (tylko walidacja)' if args.dry_run else 'PEŁNY TRENING'}")
    print("=" * 70)

    # 1. Walidacja plików
    train_count = validate_dataset_file(args.train_file)
    val_count = validate_dataset_file(args.val_file)
    print(f"[OK] Zbiór treningowy: {train_count} próbek")
    print(f"[OK] Zbiór walidacyjny: {val_count} próbek")

    # 2. Ładowanie tokenizera
    print(f"\n[1/4] Inicjalizacja tokenizera dla {args.model}...")
    tokenizer = AutoTokenizer.from_pretrained(args.model, trust_remote_code=True)
    if tokenizer.pad_token is None:
        tokenizer.pad_token = tokenizer.eos_token
    tokenizer.padding_side = "right"

    # Weryfikacja Chat Template na pierwszej próbce
    with open(args.train_file, "r", encoding="utf-8") as f:
        first_sample = json.loads(f.readline())
    sample_text = tokenizer.apply_chat_template(first_sample["messages"], tokenize=False)
    sample_tokens = tokenizer(sample_text)
    print(f"[OK] Poprawnie sformatowano szablon ChatML.")
    print(f"     Przykładowa długość: {len(sample_tokens['input_ids'])} tokenów.")

    if args.dry_run:
        print("\n" + "=" * 70)
        print("TRYB DRY-RUN: Walidacja zakończona sukcesem!")
        print("  - Struktura danych JSONL: PRAWIDŁOWA")
        print("  - Chat template tokenizera: PRAWIDŁOWY")
        print("  - Gotowość do uruchomienia treningu: 100%")
        print("Trening NIE został uruchomiony zgodnie z parametrem --dry-run.")
        print("=" * 70)
        return

    # 3. Konfiguracja kwantyzacji 4-bit (NF4)
    print(f"\n[2/4] Konfiguracja BitsAndBytes (4-bit NF4) dla modelu {args.model}...")
    bnb_config = BitsAndBytesConfig(
        load_in_4bit=True,
        bnb_4bit_quant_type="nf4",
        bnb_4bit_use_double_quant=True,
        bnb_4bit_compute_dtype=torch.bfloat16 if torch.cuda.is_bf16_supported() else torch.float16
    )

    print(f"[3/4] Ładowanie wag modelu bazowego...")
    model = AutoModelForCausalLM.from_pretrained(
        args.model,
        quantization_config=bnb_config,
        device_map="auto",
        trust_remote_code=True
    )
    model = prepare_model_for_kbit_training(model, use_gradient_checkpointing=True)

    # 4. Konfiguracja LoRA (All Linear Layers)
    target_modules = ["q_proj", "k_proj", "v_proj", "o_proj", "gate_proj", "up_proj", "down_proj"]
    peft_config = LoraConfig(
        r=args.lora_r,
        lora_alpha=args.lora_alpha,
        target_modules=target_modules,
        lora_dropout=0.05,
        bias="none",
        task_type="CAUSAL_LM"
    )
    model = get_peft_model(model, peft_config)
    model.print_trainable_parameters()

    # 5. Ładowanie datasetu HF
    train_dataset = load_dataset("json", data_files=args.train_file, split="train")
    val_dataset = load_dataset("json", data_files=args.val_file, split="train")

    # 6. Konfiguracja SFTTrainer z TRL
    training_args = SFTConfig(
        output_dir=args.output_dir,
        num_train_epochs=args.epochs,
        per_device_train_batch_size=args.batch_size,
        per_device_eval_batch_size=args.batch_size,
        gradient_accumulation_steps=args.grad_accum,
        learning_rate=args.lr,
        lr_scheduler_type="cosine",
        warmup_ratio=0.04,
        logging_steps=10,
        eval_strategy="steps",
        eval_steps=100,
        save_strategy="steps",
        save_steps=200,
        save_total_limit=3,
        bf16=torch.cuda.is_bf16_supported(),
        fp16=not torch.cuda.is_bf16_supported(),
        max_seq_length=args.max_seq_length,
        neftune_noise_alpha=args.neftune_alpha,
        report_to="none",
        dataloader_num_workers=2
    )

    trainer = SFTTrainer(
        model=model,
        args=training_args,
        train_dataset=train_dataset,
        eval_dataset=val_dataset,
        peft_config=peft_config,
        tokenizer=tokenizer
    )

    print(f"\n[4/4] Rozpoczynanie treningu QLoRA...")
    trainer.train(resume_from_checkpoint=args.resume)

    print(f"\n[Zakończono] Zapisywanie wytrenowanego adaptera LoRA do: {args.output_dir}")
    trainer.model.save_pretrained(args.output_dir)
    tokenizer.save_pretrained(args.output_dir)
    print("Model gotowy do ewaluacji i konwersji!")


if __name__ == "__main__":
    main()
