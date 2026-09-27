#!/usr/bin/env python3
# =============================================================================
# Plik: scripts/download_pl_datasets.py
# Opis: Pobiera i normalizuje zbiory danych do fine-tuningu jezyka polskiego
#       oraz angielski bufor powtórzeniowy (replay buffer) i benchmarki.
#
# Zbiory:
#   1. Bielik Distill 10k (JohnTdi/bielik-distill-polish-10k) -> data_pl/raw/bielik_distill_10k.jsonl
#   2. OWCA Polish Alpaca (emplocity/owca) -> data_pl/raw/owca_polish.jsonl
#   3. Aya Collection Polish (CohereForAI/aya_collection_language_split) -> data_pl/raw/aya_polish.jsonl
#   4. GSM8K English Reasoning Replay (openai/gsm8k) -> data_pl/replay_en/gsm8k_reasoning.jsonl
#   5. Polish MT-Bench (lightblue/mt_bench_polish) -> data_pl/benchmarks/mt_bench_pl.jsonl
# =============================================================================

import argparse
import json
import os
import sys
from datasets import load_dataset

ROOT_DIR = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
DATA_PL_DIR = os.path.join(ROOT_DIR, "data_pl")
RAW_DIR = os.path.join(DATA_PL_DIR, "raw")
REPLAY_DIR = os.path.join(DATA_PL_DIR, "replay_en")
BENCH_DIR = os.path.join(DATA_PL_DIR, "benchmarks")

os.makedirs(RAW_DIR, exist_ok=True)
os.makedirs(REPLAY_DIR, exist_ok=True)
os.makedirs(BENCH_DIR, exist_ok=True)


def download_bielik_distill(limit: int = 0):
    dest = os.path.join(RAW_DIR, "bielik_distill_10k.jsonl")
    print(f"\n[1/5] Pobieranie Bielik Distill 10k -> {dest}")
    ds = load_dataset("JohnTdi/bielik-distill-polish-10k", split="train")
    total = len(ds)
    if limit > 0:
        total = min(limit, total)
        ds = ds.select(range(total))

    count = 0
    with open(dest, "w", encoding="utf-8") as f:
        for row in ds:
            msgs = row.get("messages", [])
            if msgs and len(msgs) >= 2:
                # Oczyszczenie wiadomości
                clean_msgs = []
                for m in msgs:
                    clean_msgs.append({
                        "role": m["role"].strip(),
                        "content": m["content"].strip()
                    })
                f.write(json.dumps({"messages": clean_msgs}, ensure_ascii=False) + "\n")
                count += 1
    print(f"  -> Zapisano {count} rekordów z Bielik Distill.")


def download_owca(limit: int = 15000):
    dest = os.path.join(RAW_DIR, "owca_polish.jsonl")
    print(f"\n[2/5] Pobieranie OWCA (Polish Alpaca) -> {dest}")
    ds = load_dataset("emplocity/owca", split="train")
    total = len(ds)
    if limit > 0:
        total = min(limit, total)
        ds = ds.select(range(total))

    count = 0
    with open(dest, "w", encoding="utf-8") as f:
        for row in ds:
            instr = (row.get("instruction") or "").strip()
            inp = (row.get("input") or "").strip()
            out = (row.get("output") or "").strip()

            if not instr or not out:
                continue

            user_text = instr
            if inp:
                user_text += f"\n\nKontekst / Wejście:\n{inp}"

            msgs = [
                {"role": "user", "content": user_text},
                {"role": "assistant", "content": out}
            ]
            f.write(json.dumps({"messages": msgs}, ensure_ascii=False) + "\n")
            count += 1
    print(f"  -> Zapisano {count} rekordów z OWCA.")


def download_aya_polish(limit: int = 15000):
    dest = os.path.join(RAW_DIR, "aya_polish.jsonl")
    print(f"\n[3/5] Pobieranie Aya Collection Polish -> {dest}")
    ds = load_dataset("CohereForAI/aya_collection_language_split", "polish", split="train")
    total = len(ds)
    if limit > 0:
        total = min(limit, total)
        # Bierzemy próbkę co N-ty element dla różnorodności zadań
        step = max(1, len(ds) // total)
        indices = list(range(0, len(ds), step))[:total]
        ds = ds.select(indices)

    count = 0
    with open(dest, "w", encoding="utf-8") as f:
        for row in ds:
            inp = (row.get("inputs") or "").strip()
            tgt = (row.get("targets") or "").strip()
            task_type = row.get("task_type", "general")

            # Filtrowanie zbyt krótkich lub zepsutych rekordów
            if len(inp) < 10 or len(tgt) < 3 or "<unk>" in tgt:
                continue

            msgs = [
                {"role": "user", "content": inp},
                {"role": "assistant", "content": tgt}
            ]
            record = {
                "messages": msgs,
                "metadata": {"task_type": task_type, "source": "aya_polish"}
            }
            f.write(json.dumps(record, ensure_ascii=False) + "\n")
            count += 1
    print(f"  -> Zapisano {count} zróżnicowanych rekordów z Aya Polish.")


def download_gsm8k_replay(limit: int = 5000):
    dest = os.path.join(REPLAY_DIR, "gsm8k_reasoning.jsonl")
    print(f"\n[4/5] Pobieranie GSM8K English Replay Buffer -> {dest}")
    ds = load_dataset("openai/gsm8k", "main", split="train")
    if limit > 0:
        ds = ds.select(range(min(limit, len(ds))))

    count = 0
    with open(dest, "w", encoding="utf-8") as f:
        for row in ds:
            q = (row.get("question") or "").strip()
            a = (row.get("answer") or "").strip()
            if not q or not a:
                continue

            msgs = [
                {
                    "role": "system",
                    "content": "You are a helpful, precise mathematical reasoning assistant. Think step by step and provide the final answer."
                },
                {"role": "user", "content": q},
                {"role": "assistant", "content": a}
            ]
            f.write(json.dumps({"messages": msgs}, ensure_ascii=False) + "\n")
            count += 1
    print(f"  -> Zapisano {count} rekordów z GSM8K (bufor anty-zapominanie matematyki/logiki).")


def download_mt_bench_pl():
    dest = os.path.join(BENCH_DIR, "mt_bench_pl.jsonl")
    print(f"\n[5/5] Pobieranie Polish MT-Bench (Benchmark) -> {dest}")
    ds = load_dataset("lightblue/mt_bench_polish", split="train")
    count = 0
    with open(dest, "w", encoding="utf-8") as f:
        for row in ds:
            record = {
                "question_id": row.get("question_id"),
                "category": row.get("category"),
                "turns": row.get("turns"),
                "references": row.get("references", [])
            }
            f.write(json.dumps(record, ensure_ascii=False) + "\n")
            count += 1
    print(f"  -> Zapisano {count} wieloturowych pytań ewaluacyjnych w MT-Bench PL.")


def main():
    parser = argparse.ArgumentParser(description="Pobiera zbiory danych PL, Replay EN oraz Benchmarki.")
    parser.add_argument("--all", action="store_true", default=True, help="Pobierz wszystkie zbiory")
    parser.add_argument("--bielik-limit", type=int, default=0, help="Limit Bielik (0=wszystko ~10.3k)")
    parser.add_argument("--owca-limit", type=int, default=15000, help="Limit OWCA (domyślnie 15k)")
    parser.add_argument("--aya-limit", type=int, default=15000, help="Limit Aya Polish (domyślnie 15k)")
    parser.add_argument("--gsm8k-limit", type=int, default=5000, help="Limit GSM8K (domyślnie 5k)")
    args = parser.parse_args()

    print("Rozpoczynanie pobierania datasetów do języka polskiego i bufora inteligencji...")
    download_bielik_distill(args.bielik_limit)
    download_owca(args.owca_limit)
    download_aya_polish(args.aya_limit)
    download_gsm8k_replay(args.gsm8k_limit)
    download_mt_bench_pl()

    print("\n" + "=" * 60)
    print("Wszystkie zbiory zostały pomyślnie pobrane i znormalizowane!")
    print(f"Dane surowe PL:     {RAW_DIR}")
    print(f"Bufor Replay EN:    {REPLAY_DIR}")
    print(f"Benchmarki:         {BENCH_DIR}")
    print("=" * 60)


if __name__ == "__main__":
    main()
