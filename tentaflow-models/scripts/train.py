#!/usr/bin/env python3
# =============================================================================
# Plik: train.py
# Opis: Unified trening — Qwen3.5-0.8B (QLoRA) lub Llama Prompt Guard (full).
# Uzycie:
#   python3 train.py                      — trenuj Qwen na WSZYSTKICH datasetach
#   python3 train.py guard                — trenuj Qwen na guard
#   python3 train.py toolcalling          — trenuj Qwen na toolcalling
#   python3 train.py memory               — trenuj Qwen na memory
#   python3 train.py guard --model llama  — trenuj Llama Prompt Guard na guard short
# =============================================================================
import argparse
import hashlib
import json
import os
import random as _random
import re
import sys
import time
from contextlib import contextmanager
from datetime import datetime

import torch
from datasets import Dataset, concatenate_datasets
from transformers import (
    AutoTokenizer,
    AutoModelForCausalLM,
    AutoModelForSequenceClassification,
    BitsAndBytesConfig,
    Qwen3_5ForConditionalGeneration,
)
from peft import LoraConfig, get_peft_model, prepare_model_for_kbit_training
from trl import SFTTrainer, SFTConfig

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))

# ---------------------------------------------------------------------------
# Optymalizacje GPU — Ampere (RTX 3090): TF32 + NCCL tuning
# ---------------------------------------------------------------------------
torch.backends.cuda.matmul.allow_tf32 = True
torch.backends.cudnn.allow_tf32 = True

# NCCL — optymalizacja komunikacji multi-GPU przez PCIe (bez NVLink)
os.environ.setdefault("NCCL_P2P_LEVEL", "PHB")
os.environ.setdefault("NCCL_IB_DISABLE", "1")
os.environ.setdefault("NCCL_TREE_THRESHOLD", "0")

# Mniej fragmentacji pamieci CUDA
os.environ.setdefault("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True")

# ---------------------------------------------------------------------------
# Sciezki modeli
# ---------------------------------------------------------------------------
QWEN_MODEL = os.path.join(ROOT, "models", "qwen3.5-0.8b-base")
LLAMA_MODEL = os.path.join(ROOT, "models", "llama-prompt-guard-86m")

# Fallback na HF jesli model nie pobrany lokalnie
if not os.path.exists(QWEN_MODEL):
    QWEN_MODEL = "Qwen/Qwen3.5-0.8B"
if not os.path.exists(LLAMA_MODEL):
    LLAMA_MODEL = "meta-llama/Llama-Prompt-Guard-2-86M"

# ---------------------------------------------------------------------------
# Dane treningowe
# ---------------------------------------------------------------------------

def load_jsonl(path):
    records = []
    if not os.path.exists(path):
        return records
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                records.append(json.loads(line))
    return records


# ---------------------------------------------------------------------------
# Fingerprinting danych — wykrywanie zmian w datasetach
# ---------------------------------------------------------------------------

FINGERPRINT_FILE = ".data_fingerprint"

def compute_data_fingerprint(file_paths):
    """Oblicza SHA256 z zawartosci plikow treningowych."""
    h = hashlib.sha256()
    for path in sorted(file_paths):
        if os.path.exists(path):
            h.update(path.encode())
            h.update(str(os.path.getsize(path)).encode())
            with open(path, "rb") as f:
                for chunk in iter(lambda: f.read(65536), b""):
                    h.update(chunk)
    return h.hexdigest()


def save_fingerprint(output_dir, fingerprint):
    """Zapisuje fingerprint danych do katalogu modelu."""
    os.makedirs(output_dir, exist_ok=True)
    with open(os.path.join(output_dir, FINGERPRINT_FILE), "w") as f:
        f.write(fingerprint)


def load_fingerprint(output_dir):
    """Wczytuje zapisany fingerprint. Zwraca None jesli brak."""
    path = os.path.join(output_dir, FINGERPRINT_FILE)
    if os.path.exists(path):
        with open(path, "r") as f:
            return f.read().strip()
    return None


def find_last_checkpoint(output_dir):
    """Znajduje ostatni checkpoint w katalogu."""
    if not os.path.isdir(output_dir):
        return None
    checkpoints = [d for d in os.listdir(output_dir)
                   if d.startswith("checkpoint-") and os.path.isdir(os.path.join(output_dir, d))]
    if not checkpoints:
        return None
    checkpoints.sort(key=lambda x: int(x.split("-")[1]))
    return os.path.join(output_dir, checkpoints[-1])


def check_training_status(output_dir, current_fingerprint):
    """Sprawdza status treningu. Zwraca: 'skip', 'resume', 'fresh'."""
    saved_fp = load_fingerprint(output_dir)

    if saved_fp is None:
        # Brak fingerprinta — ale moze byc stary output bez fingerprinta
        if os.path.isdir(output_dir) and find_last_checkpoint(output_dir):
            return "resume"  # stary trening bez fingerprinta — resume
        return "fresh"

    if saved_fp == current_fingerprint:
        # Dane sie nie zmienily
        # Sprawdz czy trening byl ukonczony (final model istnieje)
        has_final = (os.path.exists(os.path.join(output_dir, "adapter_config.json"))
                     or os.path.exists(os.path.join(output_dir, "config.json")))
        if has_final:
            return "skip"
        # Fingerprint pasuje ale brak finalnego modelu — resume z checkpointu
        return "resume"

    # Dane sie zmienily — dotrenuj z ostatniego checkpointu
    return "resume"


# ---------------------------------------------------------------------------
# Timing instrumentation
# ---------------------------------------------------------------------------

def slugify(s):
    """Zamienia znaki niealfanumeryczne na '-' (np. Qwen3.5-4B -> Qwen3-5-4B)."""
    return re.sub(r"[^a-zA-Z0-9]+", "-", s).strip("-")


@contextmanager
def phase_timer(name, phases):
    """Mierzy czas wykonania bloku, zapisuje sekundy do phases dict."""
    t0 = time.perf_counter()
    yield
    phases[name] = time.perf_counter() - t0


def fmt_duration(seconds):
    """Formatuje sekundy jako HH:MM:SS lub MM:SS."""
    seconds = int(round(seconds))
    h, rem = divmod(seconds, 3600)
    m, s = divmod(rem, 60)
    if h > 0:
        return f"{h:02d}:{m:02d}:{s:02d}"
    return f"{m:02d}:{s:02d}"


def print_and_save_timing(phases, output_dir, run_id, started_at, finished_at,
                          config, training_stats):
    """Printuje ASCII summary + zapisuje timing-<id>.json + symlink timing-latest.json."""
    total = finished_at - started_at
    sum_phases = sum(phases.values())

    print()
    print("=" * 60)
    print("  TIMING SUMMARY")
    print("=" * 60)
    print(f"  Started:  {datetime.fromtimestamp(started_at).isoformat(timespec='seconds')}")
    print(f"  Finished: {datetime.fromtimestamp(finished_at).isoformat(timespec='seconds')}")
    print(f"  Total:    {fmt_duration(total)}")
    print()
    print(f"  {'Phase':<28} {'Duration':>10}    {'%':>6}")
    print(f"  {'-' * 28} {'-' * 10}    {'-' * 6}")
    for name, dur in phases.items():
        pct = (dur / total * 100) if total > 0 else 0
        print(f"  {name:<28} {fmt_duration(dur):>10}    {pct:5.1f}%")
    print(f"  {'-' * 28} {'-' * 10}    {'-' * 6}")
    print()
    if training_stats:
        print("  Training stats:")
        if training_stats.get("total_steps"):
            print(f"    Steps:           {training_stats['total_steps']}")
        if training_stats.get("avg_seconds_per_step"):
            print(f"    Avg/step:        {training_stats['avg_seconds_per_step']:.2f}s")
        if training_stats.get("effective_batch"):
            print(f"    Effective batch: {training_stats['effective_batch']}")
        if training_stats.get("samples_per_sec"):
            print(f"    Samples/sec:     {training_stats['samples_per_sec']:.2f}")
        if training_stats.get("tokens_per_sec"):
            print(f"    Tokens/sec:      {training_stats['tokens_per_sec']:,.0f}")
        if training_stats.get("final_loss") is not None:
            print(f"    Final loss:      {training_stats['final_loss']:.4f}")
    print("=" * 60)

    os.makedirs(output_dir, exist_ok=True)
    payload = {
        "run_id": run_id,
        "timestamps": {
            "started": datetime.fromtimestamp(started_at).isoformat(timespec="seconds"),
            "finished": datetime.fromtimestamp(finished_at).isoformat(timespec="seconds"),
            "duration_seconds": round(total, 2),
        },
        "config": config,
        "phases": {k: round(v, 2) for k, v in phases.items()},
        "training_stats": training_stats,
    }
    timing_path = os.path.join(output_dir, f"timing-{run_id}.json")
    with open(timing_path, "w") as f:
        json.dump(payload, f, indent=2, ensure_ascii=False)

    latest_link = os.path.join(output_dir, "timing-latest.json")
    if os.path.islink(latest_link) or os.path.exists(latest_link):
        try:
            os.unlink(latest_link)
        except OSError:
            pass
    try:
        os.symlink(os.path.basename(timing_path), latest_link)
    except OSError:
        pass

    print(f"  Saved timing: {timing_path}")
    print(f"  Latest:       {latest_link}")


def get_qwen_datasets(task, fraction=1.0, balance=False, extra_train_files=None, extra_eval_files=None):
    """Zwraca (train_dataset, eval_dataset) dla Qwen."""
    train_files = []
    eval_files = []
    extra_train_files = extra_train_files or []
    extra_eval_files = extra_eval_files or []

    datasets = {
        "intent": "intent",
        "guard": "guard",
        "model": "model",
        "plan": "plan",
        "check": "check",
        "toolcalling": "toolcalling",
        "memory": "memory",
    }

    # orchestrator = all BEZ guard
    orchestrator_tasks = {"intent", "model", "plan", "check", "toolcalling", "memory"}

    ds_names_for_files = []
    for ds_name, ds_dir in datasets.items():
        include = False
        if task == "all":
            include = True
        elif task == "orchestrator":
            include = ds_name in orchestrator_tasks
        elif task == ds_name:
            include = True

        if include:
            t = os.path.join(ROOT, "data", ds_dir, "qwen_train.jsonl")
            e = os.path.join(ROOT, "data", ds_dir, "qwen_eval.jsonl")
            if os.path.exists(t):
                train_files.append(t)
                eval_files.append(e)
                ds_names_for_files.append(ds_name)

    # Zbierz rekordy per dataset (potrzebne do balansowania)
    per_dataset_records = {}
    for f, ds_name in zip(train_files, ds_names_for_files):
        records = load_jsonl(f)
        if records:
            per_dataset_records[ds_name] = records
            print(f"  {f}: {len(records)} rekordow")

    # Balansowanie — najwiekszy dataset cappowany do max 30% calego zbioru
    # Reszta zostaje bez zmian (male datasety zachowane w calosci)
    if balance and len(per_dataset_records) > 1:
        max_pct = 0.30
        rng = _random.Random(42)
        largest_name = max(per_dataset_records, key=lambda k: len(per_dataset_records[k]))
        rest_total = sum(len(r) for name, r in per_dataset_records.items()
                         if name != largest_name)
        max_for_largest = int(rest_total * max_pct / (1.0 - max_pct))
        if len(per_dataset_records[largest_name]) > max_for_largest:
            original_len = len(per_dataset_records[largest_name])
            recs = per_dataset_records[largest_name]
            rng.shuffle(recs)
            per_dataset_records[largest_name] = recs[:max_for_largest]
            total = rest_total + max_for_largest
            print(f"  Balance: {largest_name} capped {original_len} -> {max_for_largest} ({max_for_largest*100//total}% of {total})")

    # Zlacz wszystkie rekordy treningowe
    train_records = []
    for recs in per_dataset_records.values():
        train_records.extend(recs)

    # Fraction — weź podzbiór danych treningowych
    if fraction < 1.0:
        train_records_original_len = len(train_records)
        rng = _random.Random(42)
        rng.shuffle(train_records)
        n = max(1, int(train_records_original_len * fraction))
        train_records = train_records[:n]
        print(f"  Fraction {fraction:.2f}: {n}/{train_records_original_len} rekordow")

    eval_records = []
    for f in eval_files:
        records = load_jsonl(f)
        if records:
            eval_records.extend(records)

    for f in extra_train_files:
        records = load_jsonl(f)
        if records:
            train_records.extend(records)
            print(f"  extra train {f}: {len(records)} rekordow")

    for f in extra_eval_files:
        records = load_jsonl(f)
        if records:
            eval_records.extend(records)
            print(f"  extra eval {f}: {len(records)} rekordow")

    return train_records, eval_records


def get_llama_datasets():
    """Zwraca (train_dataset, eval_dataset) dla Llama Prompt Guard."""
    train = load_jsonl(os.path.join(ROOT, "data", "guard", "llama_train.jsonl"))
    eval_ = load_jsonl(os.path.join(ROOT, "data", "guard", "llama_eval.jsonl"))
    return train, eval_


# ---------------------------------------------------------------------------
# Trening Qwen (QLoRA)
# ---------------------------------------------------------------------------

def train_qwen(task, resume_from=None, method="qlora", fraction=1.0, balance=False,
               extra_train_files=None, extra_eval_files=None):
    """Trening Qwen — qlora, lora, full, dora."""
    qwen_label = os.path.basename(QWEN_MODEL.rstrip("/"))
    qwen_slug = slugify(qwen_label)
    print("=" * 60)
    print(f"Trening {qwen_label} | task: {task} | method: {method}")
    print("=" * 60)

    phases = {}
    started_at_wall = time.time()
    run_id = datetime.fromtimestamp(started_at_wall).strftime("%Y%m%d-%H%M%S")

    # SETUP — manual timer because of early-return on skip
    setup_t0 = time.perf_counter()

    dir_name = f"{qwen_slug}-{task}-{method}"
    if fraction < 1.0:
        if fraction <= 0.34:
            dir_name += "-low"
        elif fraction <= 0.67:
            dir_name += "-medium"
        else:
            dir_name += "-high"
    output_dir = os.path.join(ROOT, "output", dir_name)

    datasets_map = {
        "intent": "intent", "guard": "guard", "model": "model",
        "plan": "plan", "check": "check", "toolcalling": "toolcalling",
        "memory": "memory",
    }
    orchestrator_tasks = {"intent", "model", "plan", "check", "toolcalling", "memory"}
    fp_files = []
    for ds_name, ds_dir in datasets_map.items():
        include = (task == "all" or task == ds_name
                   or (task == "orchestrator" and ds_name in orchestrator_tasks))
        if include:
            fp_files.append(os.path.join(ROOT, "data", ds_dir, "qwen_train.jsonl"))
    extra_train_files = extra_train_files or []
    extra_eval_files = extra_eval_files or []
    current_fp = compute_data_fingerprint(fp_files + extra_train_files + extra_eval_files)

    if resume_from is None:
        status = check_training_status(output_dir, current_fp)
        if status == "skip":
            print(f"\n  SKIP: dane nie zmienione, model juz wytrenowany ({output_dir})")
            return
        elif status == "resume":
            resume_from = find_last_checkpoint(output_dir)
            if resume_from:
                print(f"\n  RESUME: dane zmienione lub trening niedokonczony")
                print(f"  Checkpoint: {resume_from}")
            else:
                print(f"\n  FRESH: brak checkpointu, trening od zera")

    is_distributed = int(os.environ.get("WORLD_SIZE", "1")) > 1
    dm = None if is_distributed else "auto"
    if is_distributed:
        print(f"  Multi-GPU: device_map=None (DeepSpeed zarzadza dystrybucja)")

    try:
        import flash_attn  # noqa: F401
        attn_impl = "flash_attention_2"
        print("  Attention: flash_attention_2")
    except ImportError:
        attn_impl = "sdpa"
        print("  Attention: sdpa (PyTorch native)")

    load_kwargs = dict(
        device_map=dm,
        trust_remote_code=True,
        dtype=torch.bfloat16,
    )
    if attn_impl:
        load_kwargs["attn_implementation"] = attn_impl
    if method == "qlora":
        load_kwargs["quantization_config"] = BitsAndBytesConfig(
            load_in_4bit=True,
            bnb_4bit_quant_type="nf4",
            bnb_4bit_compute_dtype=torch.bfloat16,
            bnb_4bit_use_double_quant=True,
        )

    phases["setup"] = time.perf_counter() - setup_t0

    with phase_timer("tokenizer", phases):
        print("\nTokenizer...")
        tokenizer = AutoTokenizer.from_pretrained(QWEN_MODEL, trust_remote_code=True)
        if tokenizer.pad_token is None:
            tokenizer.pad_token = tokenizer.eos_token

        special_tokens = [
            "<|guard|>", "<|intent|>", "<|tools|>", "<|query|>",
            "<|memory|>", "<|summary|>", "<|feedback|>", "<|recall|>",
            "<|extract|>", "<|model|>", "<|plan|>", "<|check|>",
        ]
        num_added = tokenizer.add_special_tokens({"additional_special_tokens": special_tokens})
        print(f"  Special tokeny: +{num_added}")

    with phase_timer("model_load", phases):
        load_label = {
            "full": "full fine-tune, bf16",
            "lora": "bf16 + LoRA",
            "dora": "bf16 + DoRA",
            "qlora": "4-bit QLoRA",
        }[method]
        print(f"Model ({load_label})...")
        model = Qwen3_5ForConditionalGeneration.from_pretrained(QWEN_MODEL, **load_kwargs)
        model.resize_token_embeddings(len(tokenizer))

    with phase_timer("peft_wrap", phases):
        if method == "full":
            total_params = sum(p.numel() for p in model.parameters())
            trainable_params = sum(p.numel() for p in model.parameters() if p.requires_grad)
            print(f"  trainable params: {trainable_params:,} || all params: {total_params:,} || trainable%: 100.0")
        else:
            if method == "qlora":
                model = prepare_model_for_kbit_training(model)
                lora_config = LoraConfig(
                    r=32, lora_alpha=64, lora_dropout=0.05,
                    target_modules=["q_proj", "k_proj", "v_proj", "o_proj",
                                    "gate_proj", "up_proj", "down_proj"],
                    bias="none", task_type="CAUSAL_LM",
                )
            elif method == "dora":
                lora_config = LoraConfig(
                    r=64, lora_alpha=128, lora_dropout=0.05,
                    target_modules=["q_proj", "k_proj", "v_proj", "o_proj",
                                    "gate_proj", "up_proj", "down_proj"],
                    bias="none", task_type="CAUSAL_LM", use_dora=True,
                )
            else:  # lora
                lora_config = LoraConfig(
                    r=64, lora_alpha=128, lora_dropout=0.05,
                    target_modules=["q_proj", "k_proj", "v_proj", "o_proj",
                                    "gate_proj", "up_proj", "down_proj"],
                    bias="none", task_type="CAUSAL_LM",
                )
            print(f"{method.upper()}...")
            model = get_peft_model(model, lora_config)
            model.print_trainable_parameters()

    train_dataset = None
    eval_dataset = None
    total_train_tokens = 0
    with phase_timer("dataset", phases):
        print("\nDane...")
        train_records, eval_records = get_qwen_datasets(
            task,
            fraction=fraction,
            balance=balance,
            extra_train_files=extra_train_files,
            extra_eval_files=extra_eval_files,
        )
        if train_records:
            def format_chat(record):
                return tokenizer.apply_chat_template(
                    record["messages"], tokenize=False, add_generation_prompt=False
                )

            train_texts = [format_chat(r) for r in train_records]
            eval_texts = [format_chat(r) for r in eval_records] if eval_records else []

            train_dataset = Dataset.from_dict({"text": train_texts})
            eval_dataset = Dataset.from_dict({"text": eval_texts}) if eval_texts else None

            total_train_tokens = sum(
                len(tokenizer.encode(t, add_special_tokens=False)) for t in train_texts
            )
            print(f"  Train: {len(train_texts)}, Eval: {len(eval_texts)}, Tokens: {total_train_tokens:,}")

    if train_dataset is None:
        print("BLAD: Brak danych! Uruchom convert.py najpierw.")
        return

    num_gpus = int(os.environ.get("WORLD_SIZE", "1"))
    if method == "full":
        lr = 5e-5
        batch = 8
        grad_accum = 4
        epochs = 3
    elif method in ("lora", "dora"):
        lr = 1e-4
        batch = 14
        grad_accum = 3
        epochs = 5
    else:  # qlora
        lr = 2e-4
        batch = 16
        grad_accum = 2
        epochs = 5

    eff_batch = batch * grad_accum * num_gpus
    max_length = 2048
    packing = False
    gradient_checkpointing = False

    with phase_timer("trainer_init", phases):
        print(f"\nTrening (method={method}, lr={lr}, batch={batch}x{grad_accum}x{num_gpus}gpu={eff_batch})...")
        training_args = SFTConfig(
            output_dir=output_dir,
            num_train_epochs=epochs,
            per_device_train_batch_size=batch,
            per_device_eval_batch_size=batch,
            gradient_accumulation_steps=grad_accum,
            learning_rate=lr,
            lr_scheduler_type="cosine",
            warmup_steps=50,
            weight_decay=0.01,
            bf16=True,
            tf32=True,
            gradient_checkpointing=gradient_checkpointing,
            optim="adamw_torch_fused",
            use_liger_kernel=True,
            dataloader_num_workers=8,
            dataloader_pin_memory=True,
            dataloader_persistent_workers=True,
            dataloader_prefetch_factor=4,
            logging_steps=10,
            eval_strategy="no",
            save_strategy="steps",
            save_steps=500,
            save_total_limit=2,
            report_to="none",
            max_grad_norm=1.0,
            max_length=max_length,
            dataset_text_field="text",
            packing=packing,
        )
        trainer = SFTTrainer(
            model=model,
            args=training_args,
            train_dataset=train_dataset,
            eval_dataset=eval_dataset,
            processing_class=tokenizer,
        )

    with phase_timer("training", phases):
        if resume_from:
            print(f"  Resume from: {resume_from}")
            trainer.train(resume_from_checkpoint=resume_from)
        else:
            trainer.train()

    with phase_timer("save", phases):
        print(f"\nZapis do {output_dir}...")
        if method == "full":
            model.save_pretrained(output_dir)
            print(f"  Zapisano pelny model ({method})")
        else:
            trainer.save_model(output_dir)
            print(f"  Zapisano adapter ({method})")
        tokenizer.save_pretrained(output_dir)
        save_fingerprint(output_dir, current_fp)
    print(f"Qwen trening zakonczony! (method={method})")

    finished_at_wall = time.time()

    training_seconds = phases.get("training", 0.0)
    total_steps = int(getattr(trainer.state, "global_step", 0))
    avg_per_step = (training_seconds / total_steps) if total_steps > 0 else 0.0
    samples_per_sec = (total_steps * eff_batch / training_seconds) if training_seconds > 0 else 0.0
    total_tokens_seen = total_train_tokens * epochs
    tokens_per_sec = (total_tokens_seen / training_seconds) if training_seconds > 0 else 0.0

    final_loss = None
    for entry in reversed(trainer.state.log_history or []):
        if "loss" in entry:
            final_loss = float(entry["loss"])
            break

    config = {
        "task": task,
        "method": method,
        "qwen_base": qwen_label,
        "fraction": fraction,
        "balance": balance,
        "batch_size": batch,
        "grad_accum": grad_accum,
        "effective_batch": eff_batch,
        "epochs": epochs,
        "learning_rate": lr,
        "max_length": max_length,
        "packing": packing,
        "gradient_checkpointing": gradient_checkpointing,
        "num_gpus": num_gpus,
    }
    training_stats = {
        "total_steps": total_steps,
        "avg_seconds_per_step": round(avg_per_step, 3),
        "effective_batch": eff_batch,
        "samples_per_sec": round(samples_per_sec, 2),
        "tokens_per_sec": round(tokens_per_sec, 0),
        "total_tokens_seen": int(total_tokens_seen),
        "final_loss": final_loss,
    }
    print_and_save_timing(phases, output_dir, run_id, started_at_wall, finished_at_wall,
                          config, training_stats)


# ---------------------------------------------------------------------------
# Trening Llama Prompt Guard (full fine-tune)
# ---------------------------------------------------------------------------

def train_llama():
    """Trening Llama Prompt Guard 86M — klasyfikator BERT-like."""
    from transformers import TrainingArguments, Trainer, DataCollatorWithPadding

    print("=" * 60)
    print("Trening Llama Prompt Guard 86M | task: guard (short only)")
    print("=" * 60)

    phases = {}
    started_at_wall = time.time()
    run_id = datetime.fromtimestamp(started_at_wall).strftime("%Y%m%d-%H%M%S")

    setup_t0 = time.perf_counter()

    output_dir = os.path.join(ROOT, "output", "llama-guard")
    fp_files = [
        os.path.join(ROOT, "data", "guard", "llama_train.jsonl"),
        os.path.join(ROOT, "data", "guard", "llama_eval.jsonl"),
    ]
    current_fp = compute_data_fingerprint(fp_files)

    status = check_training_status(output_dir, current_fp)
    if status == "skip":
        print(f"\n  SKIP: dane nie zmienione, model juz wytrenowany ({output_dir})")
        return

    resume_from = None
    if status == "resume":
        resume_from = find_last_checkpoint(output_dir)
        if resume_from:
            print(f"\n  RESUME: checkpoint {resume_from}")

    phases["setup"] = time.perf_counter() - setup_t0

    with phase_timer("tokenizer", phases):
        print("\nTokenizer...")
        tokenizer = AutoTokenizer.from_pretrained(LLAMA_MODEL)

    with phase_timer("model_load", phases):
        print("Model...")
        model = AutoModelForSequenceClassification.from_pretrained(
            LLAMA_MODEL,
            num_labels=3,
            id2label={0: "SAFE", 1: "INJECTION", 2: "JAILBREAK"},
            label2id={"SAFE": 0, "INJECTION": 1, "JAILBREAK": 2},
            ignore_mismatched_sizes=True,
        )

    train_dataset = None
    eval_dataset = None
    total_train_tokens = 0
    with phase_timer("dataset", phases):
        print("Dane...")
        train_records, eval_records = get_llama_datasets()
        if train_records:
            train_dataset = Dataset.from_list(train_records)
            eval_dataset = Dataset.from_list(eval_records) if eval_records else None

            def tokenize(examples):
                return tokenizer(examples["text"], truncation=True, padding=True, max_length=512)

            train_dataset = train_dataset.map(tokenize, batched=True)
            if eval_dataset:
                eval_dataset = eval_dataset.map(tokenize, batched=True)

            total_train_tokens = sum(len(ids) for ids in train_dataset["input_ids"])
            print(f"  Train: {len(train_dataset)}, Eval: {len(eval_dataset) if eval_dataset else 0}, Tokens: {total_train_tokens:,}")

    if train_dataset is None:
        print("BLAD: Brak danych! Uruchom convert.py guard najpierw.")
        return

    batch = 16
    epochs = 3
    eff_batch = batch * int(os.environ.get("WORLD_SIZE", "1"))

    with phase_timer("trainer_init", phases):
        print("\nTrening...")
        training_args = TrainingArguments(
            output_dir=output_dir,
            num_train_epochs=epochs,
            per_device_train_batch_size=batch,
            per_device_eval_batch_size=batch,
            learning_rate=2e-5,
            weight_decay=0.01,
            eval_strategy="epoch" if eval_dataset else "no",
            save_strategy="epoch",
            save_total_limit=2,
            load_best_model_at_end=True if eval_dataset else False,
            report_to="none",
            bf16=True,
            optim="adamw_torch_fused",
            dataloader_num_workers=8,
            dataloader_pin_memory=True,
            dataloader_persistent_workers=True,
            dataloader_prefetch_factor=4,
        )
        trainer = Trainer(
            model=model,
            args=training_args,
            train_dataset=train_dataset,
            eval_dataset=eval_dataset,
            processing_class=tokenizer,
            data_collator=DataCollatorWithPadding(tokenizer=tokenizer),
        )

    with phase_timer("training", phases):
        if resume_from:
            print(f"  Resume from: {resume_from}")
            trainer.train(resume_from_checkpoint=resume_from)
        else:
            trainer.train()

    with phase_timer("save", phases):
        print(f"\nZapis do {output_dir}...")
        model.save_pretrained(output_dir)
        tokenizer.save_pretrained(output_dir)
        save_fingerprint(output_dir, current_fp)
    print("Llama Guard trening zakonczony!")

    finished_at_wall = time.time()
    training_seconds = phases.get("training", 0.0)
    total_steps = int(getattr(trainer.state, "global_step", 0))
    avg_per_step = (training_seconds / total_steps) if total_steps > 0 else 0.0
    samples_per_sec = (total_steps * eff_batch / training_seconds) if training_seconds > 0 else 0.0
    tokens_per_sec = (total_train_tokens * epochs / training_seconds) if training_seconds > 0 else 0.0

    final_loss = None
    for entry in reversed(trainer.state.log_history or []):
        if "loss" in entry:
            final_loss = float(entry["loss"])
            break

    config = {
        "model": "llama-prompt-guard-86m",
        "task": "guard",
        "method": "full",
        "batch_size": batch,
        "effective_batch": eff_batch,
        "epochs": epochs,
        "learning_rate": 2e-5,
        "max_length": 512,
    }
    training_stats = {
        "total_steps": total_steps,
        "avg_seconds_per_step": round(avg_per_step, 3),
        "effective_batch": eff_batch,
        "samples_per_sec": round(samples_per_sec, 2),
        "tokens_per_sec": round(tokens_per_sec, 0),
        "total_tokens_seen": int(total_train_tokens * epochs),
        "final_loss": final_loss,
    }
    print_and_save_timing(phases, output_dir, run_id, started_at_wall, finished_at_wall,
                          config, training_stats)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(
        description="TentaFlow model training",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Datasety:
  all            Jeden model ze WSZYSTKIM (domyslnie)
  orchestrator   Wszystko BEZ guard (osobny model)
  guard          TYLKO guard — detekcja injection/jailbreak
  intent         Intent router — routing taskow
  model          Model router — wybor modelu LLM
  plan           Plan — planowanie wielokrokowe
  check          Check — walidacja wynikow
  toolcalling    Tool calling — TOON format
  memory         Memory — fakty, podsumowania, feedback

Metody treningu:
  qlora   4-bit quantyzacja + LoRA adapter (domyslna, ~8GB VRAM)
  lora    BF16 model + LoRA adapter (lepsza, ~16GB VRAM)
  dora    BF16 model + DoRA adapter (najlepsza z adapterow, ~18GB VRAM)
  full    Pelny fine-tune calego modelu (najlepsza jakosc, ~24GB VRAM)

Strategie:
  1-modelowa:  python3 scripts/train.py                  # all
  2-modelowa:  python3 scripts/train.py guard             # model 1
               python3 scripts/train.py orchestrator      # model 2

Przyklady:
  python3 scripts/train.py                              # QLoRA na wszystkim
  python3 scripts/train.py --method full                # full fine-tune
  python3 scripts/train.py guard --method dora          # DoRA na guard
  python3 scripts/train.py orchestrator --method full   # full FT orchestrator
  python3 scripts/train.py guard --model llama          # Llama Prompt Guard
  python3 scripts/train.py --resume output/qwen-all-lora/checkpoint-100
""")
    # Przechwycenie 'help' jako pozycyjnego argumentu
    if len(sys.argv) > 1 and sys.argv[1] in ("help", "-h", "--help"):
        parser.print_help()
        sys.exit(0)

    parser.add_argument("task", nargs="?", default="all",
                        choices=["all", "orchestrator", "intent", "guard", "model", "plan", "check", "toolcalling", "memory"],
                        help="Ktory dataset (domyslnie: all)")
    parser.add_argument("--model", default="qwen",
                        choices=["qwen", "llama"],
                        help="qwen = Qwen3.5-0.8B (domyslny), llama = Llama-Prompt-Guard-86M")
    parser.add_argument("--method", default="qlora",
                        choices=["qlora", "lora", "full", "dora"],
                        help="Metoda treningu (domyslnie: qlora)")
    parser.add_argument("--gpus", type=int, default=1,
                        help="Ile GPU uzyc (>1 = DeepSpeed, domyslnie: 1)")
    parser.add_argument("--resume", default=None,
                        help="Sciezka do checkpointu do kontynuacji treningu")
    parser.add_argument("--fraction", type=float, default=1.0,
                        help="Frakcja danych treningowych (0.0-1.0, domyslnie 1.0)")
    parser.add_argument("--balance", action="store_true",
                        help="Zrownowaz datasety (cap do mediany)")
    parser.add_argument("--qwen-base", default=None,
                        help="Nazwa katalogu bazowego Qwen w models/ albo absolutna sciezka (np. Qwen3.5-4B)")
    parser.add_argument("--extra-train", nargs="+", action="append", default=[],
                        help="Dodatkowe pliki JSONL w formacie Qwen chat dolaczane do train")
    parser.add_argument("--extra-eval", nargs="+", action="append", default=[],
                        help="Dodatkowe pliki JSONL w formacie Qwen chat dolaczane do eval")
    args = parser.parse_args()
    args.extra_train = [path for group in args.extra_train for path in group]
    args.extra_eval = [path for group in args.extra_eval for path in group]

    if args.qwen_base:
        global QWEN_MODEL
        QWEN_MODEL = args.qwen_base if os.path.isabs(args.qwen_base) \
            else os.path.join(ROOT, "models", args.qwen_base)
        if not os.path.exists(QWEN_MODEL):
            print(f"Brak katalogu bazowego Qwen: {QWEN_MODEL}")
            sys.exit(1)

    if args.model == "llama":
        if args.task not in ("guard", "all"):
            print("Llama Prompt Guard obsluguje TYLKO task 'guard'")
            sys.exit(1)
        train_llama()
    elif args.gpus > 1 and os.environ.get("LOCAL_RANK") is None:
        # Multi-GPU: relansuj przez accelerate z DeepSpeed
        ds_config = os.path.join(ROOT, "configs",
            "deepspeed_zero3.json" if args.method == "full" else "deepspeed_zero2.json")
        cmd = [
            "accelerate", "launch",
            "--num_processes", str(args.gpus),
            "--use_deepspeed",
            "--deepspeed_config_file", ds_config,
            __file__,
            args.task,
            "--method", args.method,
            "--gpus", "1",  # wewnatrz juz nie relansuj
        ]
        if args.resume:
            cmd.extend(["--resume", args.resume])
        if args.fraction < 1.0:
            cmd.extend(["--fraction", str(args.fraction)])
        if args.balance:
            cmd.append("--balance")
        if args.qwen_base:
            cmd.extend(["--qwen-base", args.qwen_base])
        for path in args.extra_train:
            cmd.extend(["--extra-train", path])
        for path in args.extra_eval:
            cmd.extend(["--extra-eval", path])
        print(f"Multi-GPU: {args.gpus} kart, DeepSpeed {'ZeRO-3' if args.method == 'full' else 'ZeRO-2'}")
        print(f"Komenda: {' '.join(cmd)}")
        os.execvp(cmd[0], cmd)
    else:
        train_qwen(args.task, resume_from=args.resume, method=args.method,
                   fraction=args.fraction, balance=args.balance,
                   extra_train_files=args.extra_train,
                   extra_eval_files=args.extra_eval)


if __name__ == "__main__":
    main()
