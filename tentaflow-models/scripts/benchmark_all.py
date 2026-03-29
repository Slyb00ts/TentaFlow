#!/usr/bin/env python3
# =============================================================================
# Plik: benchmark_all.py
# Opis: Unified benchmark — testuje WSZYSTKIE taski Qwen orchestratora.
# Uzycie:
#   python3 scripts/benchmark_all.py                    — wszystkie taski
#   python3 scripts/benchmark_all.py --tasks intent guard
#   python3 scripts/benchmark_all.py --gguf output/qwen-all-lora-Q5_K_M.gguf
#   python3 scripts/benchmark_all.py --help
# =============================================================================
import argparse
import json
import os
import sys
import time
from collections import Counter, defaultdict

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
TEST_FILE = os.path.join(ROOT, "data", "test_benchmark.jsonl")

ALL_TASKS = ["intent", "guard", "model", "check", "extract", "memory", "summary", "feedback"]

# System prompty per task
SYSTEM_PROMPTS = {
    "intent": (
        "You are a task router. Output a comma-separated list of tasks.\n"
        "Tasks: TEXT, TOOLS, MODEL, MEMORY, FEEDBACK, RECALL, EXTRACT, PLAN\n"
        "TEXT is standalone. Output ONLY the task list."
    ),
    "guard": (
        "You are a security classifier. Output ONLY a single digit:\n"
        "0 = benign, 1 = prompt injection, 2 = jailbreak.\n"
        "If BOTH injection AND jailbreak, output 1."
    ),
    "model": (
        "You are a model router. Select the best model for the task.\n"
        "Output: alias|task=desc or #UNAVAILABLE|reason=explanation"
    ),
    "check": (
        "You are a result validator.\n"
        "Output: OK, RETRY|fix=suggestion, or ESCALATE|reason=why"
    ),
    "extract": (
        "You are a field extractor. Extract requested fields from document.\n"
        "Output: field=value, one per line. If not found: field=BRAK"
    ),
    "memory": (
        "You are a fact extractor. Extract facts and relations from text.\n"
        "Output structured facts: FACT, RELATION, LAYER lines."
    ),
    "summary": (
        "You are a summarization assistant. Create structured summaries.\n"
        "For L1: TEMAT, USTALENIA, PROBLEMY, FEEDBACK, STATUS.\n"
        "For TOP_UPDATE: max 300 tokens rolling summary."
    ),
    "feedback": (
        "You are a feedback detector. Classify user feedback.\n"
        "Output: TYPE: CORRECTION/POSITIVE/NEGATIVE/PREFERENCE/NONE + details."
    ),
}

# Mapowanie task -> special token prefix
TASK_PREFIX = {
    "intent": "<|intent|>\n",
    "guard": "<|guard|>\n",
    "model": "",  # input juz zawiera <|model|>
    "check": "",  # input juz zawiera <|check|>
    "extract": "",  # input juz zawiera <|extract|>
    "memory": "<|memory|>\n",
    "summary": "",  # input juz zawiera <|summary|>
    "feedback": "<|feedback|>\n",
}


def load_tests():
    records = []
    with open(TEST_FILE, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                records.append(json.loads(line))
    return records


# ---------------------------------------------------------------------------
# GGUF inference (llama-cpp-python)
# ---------------------------------------------------------------------------

_llm = None
_llm_path = None


def load_model(model_path):
    global _llm, _llm_path
    if _llm is not None and _llm_path == model_path:
        return _llm

    if _llm is not None:
        del _llm

    from llama_cpp import Llama
    print(f"  Ladowanie modelu: {model_path}")
    _llm = Llama(model_path=model_path, n_gpu_layers=-1, n_ctx=2048, verbose=False)
    _llm_path = model_path
    return _llm


def predict_gguf(model_path, task, input_text):
    llm = load_model(model_path)

    system = SYSTEM_PROMPTS.get(task, "")
    prefix = TASK_PREFIX.get(task, "")
    user_content = f"{prefix}{input_text}" if prefix and not input_text.startswith("<|") else input_text

    messages = [
        {"role": "system", "content": system},
        {"role": "user", "content": user_content},
    ]

    start = time.perf_counter()
    response = llm.create_chat_completion(messages=messages, max_tokens=200, temperature=0)
    elapsed = time.perf_counter() - start

    raw = response["choices"][0]["message"]["content"].strip()
    return raw, elapsed


# ---------------------------------------------------------------------------
# HuggingFace inference
# ---------------------------------------------------------------------------

_hf_model = None
_hf_tokenizer = None


def load_hf_model(lora_path=None):
    global _hf_model, _hf_tokenizer
    if _hf_model is not None:
        return _hf_model, _hf_tokenizer

    import torch
    from transformers import AutoTokenizer, Qwen3_5ForConditionalGeneration

    base_path = os.path.join(ROOT, "models", "qwen3.5-0.8b-base")
    tok_path = lora_path if lora_path else base_path

    print(f"  Ladowanie HF model...")
    _hf_tokenizer = AutoTokenizer.from_pretrained(tok_path, trust_remote_code=True)
    if _hf_tokenizer.pad_token is None:
        _hf_tokenizer.pad_token = _hf_tokenizer.eos_token

    special_tokens = [
        "<|guard|>", "<|intent|>", "<|tools|>", "<|query|>",
        "<|memory|>", "<|summary|>", "<|feedback|>", "<|recall|>",
        "<|extract|>", "<|model|>", "<|plan|>", "<|check|>",
    ]
    _hf_tokenizer.add_special_tokens({"additional_special_tokens": special_tokens})

    _hf_model = Qwen3_5ForConditionalGeneration.from_pretrained(
        base_path, device_map="auto", trust_remote_code=True,
        dtype=torch.bfloat16,
    )
    _hf_model.resize_token_embeddings(len(_hf_tokenizer))

    if lora_path:
        from peft import PeftModel
        _hf_model = PeftModel.from_pretrained(_hf_model, lora_path)
        _hf_model = _hf_model.merge_and_unload()

    _hf_model.eval()
    return _hf_model, _hf_tokenizer


def predict_hf(task, input_text, lora_path=None):
    import torch
    model, tokenizer = load_hf_model(lora_path)

    system = SYSTEM_PROMPTS.get(task, "")
    prefix = TASK_PREFIX.get(task, "")
    user_content = f"{prefix}{input_text}" if prefix and not input_text.startswith("<|") else input_text

    messages = [
        {"role": "system", "content": system},
        {"role": "user", "content": user_content},
    ]

    chat_text = tokenizer.apply_chat_template(messages, tokenize=False, add_generation_prompt=True)
    inputs = tokenizer(chat_text, return_tensors="pt").to(model.device)

    start = time.perf_counter()
    with torch.no_grad():
        outputs = model.generate(**inputs, max_new_tokens=200, do_sample=False)
    elapsed = time.perf_counter() - start

    generated = outputs[0][inputs["input_ids"].shape[1]:]
    raw = tokenizer.decode(generated, skip_special_tokens=True).strip()
    return raw, elapsed


# ---------------------------------------------------------------------------
# Scoring
# ---------------------------------------------------------------------------

def score_result(task, raw, record):
    """Ocenia wynik — zwraca (exact_match, partial_match, details)."""

    if "expected" in record:
        expected = str(record["expected"])

        if task == "guard":
            # Guard: exact digit match
            pred = "-1"
            for ch in raw:
                if ch in "012":
                    pred = ch
                    break
            exact = pred == expected
            # Safe/unsafe match (1 i 2 oba = unsafe)
            safe_match = (pred == "0") == (expected == "0")
            return exact, safe_match, f"exp={expected} pred={pred}"

        elif task == "intent":
            # Intent: porownaj zbiory taskow
            exp_set = set(expected.split(","))
            pred_set = set(raw.strip().split(","))
            exact = exp_set == pred_set
            partial = len(exp_set & pred_set) > 0
            return exact, partial, f"exp={expected} pred={raw.strip()}"

        elif task == "check":
            # Check: OK/RETRY/ESCALATE
            exp_type = expected.split("|")[0] if "|" in expected else expected
            pred_type = raw.split("|")[0] if "|" in raw else raw.strip()
            exact = pred_type == exp_type
            return exact, exact, f"exp={exp_type} pred={pred_type}"

    if "expected_contains" in record:
        contains = record["expected_contains"]
        found = contains.lower() in raw.lower()
        return found, found, f"contains='{contains}' in raw={'yes' if found else 'NO'}"

    return False, False, "no expected defined"


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def run_benchmark(predict_fn, tests, label):
    print(f"\n{'='*70}")
    print(f"  {label}")
    print(f"{'='*70}")

    task_results = defaultdict(lambda: {"total": 0, "exact": 0, "partial": 0, "times": []})
    all_results = []

    for i, rec in enumerate(tests):
        task = rec["task"]
        input_text = rec["input"]

        raw, elapsed = predict_fn(task, input_text)
        exact, partial, details = score_result(task, raw, rec)

        task_results[task]["total"] += 1
        if exact:
            task_results[task]["exact"] += 1
        if partial:
            task_results[task]["partial"] += 1
        task_results[task]["times"].append(elapsed)

        status = "OK" if exact else ("OK~" if partial else "FAIL")
        cat = rec.get("category", "")
        print(f"  [{i+1:2d}/{len(tests)}] {status:4s} | {task:8s} | {elapsed:.3f}s | {cat}")
        if not exact:
            print(f"         {details}")
            if not partial:
                print(f"         raw: {raw[:80]}")

        all_results.append({
            "task": task, "exact": exact, "partial": partial,
            "time": elapsed, "category": cat,
        })

    # Podsumowanie per task
    print(f"\n  {'Task':<12} {'Exact':>8} {'Partial':>8} {'Avg time':>10}")
    print(f"  {'-'*40}")

    total_exact = 0
    total_partial = 0
    total_count = 0
    total_times = []

    for task in sorted(task_results):
        r = task_results[task]
        exact_pct = r["exact"] / r["total"] * 100 if r["total"] > 0 else 0
        partial_pct = r["partial"] / r["total"] * 100 if r["total"] > 0 else 0
        avg_t = sum(r["times"]) / len(r["times"]) if r["times"] else 0
        print(f"  {task:<12} {exact_pct:>7.1f}% {partial_pct:>7.1f}% {avg_t:>9.3f}s")
        total_exact += r["exact"]
        total_partial += r["partial"]
        total_count += r["total"]
        total_times.extend(r["times"])

    overall_exact = total_exact / total_count * 100 if total_count > 0 else 0
    overall_partial = total_partial / total_count * 100 if total_count > 0 else 0
    avg_all = sum(total_times) / len(total_times) if total_times else 0

    print(f"  {'-'*40}")
    print(f"  {'TOTAL':<12} {overall_exact:>7.1f}% {overall_partial:>7.1f}% {avg_all:>9.3f}s")
    print(f"  ({total_exact}/{total_count} exact, {total_partial}/{total_count} partial)")

    return {
        "label": label,
        "overall_exact": overall_exact,
        "overall_partial": overall_partial,
        "avg_time": avg_all,
        "per_task": {t: {
            "exact": r["exact"] / r["total"] * 100,
            "partial": r["partial"] / r["total"] * 100,
            "count": r["total"],
        } for t, r in task_results.items()},
    }


def main():
    parser = argparse.ArgumentParser(
        description="TentaFlow unified benchmark",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Przyklady:
  python3 scripts/benchmark_all.py                              # fine-tuned HF (domyslne)
  python3 scripts/benchmark_all.py --base                       # bazowy model (bez fine-tuningu)
  python3 scripts/benchmark_all.py --gguf output/qwen-all-lora-Q5_K_M.gguf
  python3 scripts/benchmark_all.py --tasks intent guard         # tylko wybrane taski
  python3 scripts/benchmark_all.py --compare                    # bazowy vs fine-tuned obok siebie
  python3 scripts/benchmark_all.py --compare --gguf output/qwen-all-lora-Q5_K_M.gguf  # bazowy GGUF vs FT GGUF
""")
    parser.add_argument("--tasks", nargs="+", default=None,
                        help=f"Filter tasks: {', '.join(ALL_TASKS)}")
    parser.add_argument("--gguf", default=None,
                        help="Path to GGUF fine-tuned model")
    parser.add_argument("--lora", default=None,
                        help="Path to LoRA adapter (default: output/qwen-all-lora)")
    parser.add_argument("--base", action="store_true",
                        help="Testuj bazowy model (bez fine-tuningu)")
    parser.add_argument("--compare", action="store_true",
                        help="Porownaj bazowy vs fine-tuned")

    if len(sys.argv) > 1 and sys.argv[1] in ("help", "-h", "--help"):
        parser.print_help()
        sys.exit(0)

    args = parser.parse_args()

    # Wczytaj testy
    tests = load_tests()
    if args.tasks:
        tests = [t for t in tests if t["task"] in args.tasks]
    print(f"Test cases: {len(tests)}")
    task_counts = Counter(t["task"] for t in tests)
    for t, c in sorted(task_counts.items()):
        print(f"  {t}: {c}")

    if not tests:
        print("Brak testow!")
        return

    all_results = []

    # --compare: bazowy + fine-tuned
    if args.compare:
        if args.gguf:
            # GGUF compare: bazowy GGUF vs fine-tuned GGUF
            base_gguf = os.path.join(ROOT, "output", "qwen-base-Q5_K_M.gguf")
            if not os.path.exists(base_gguf):
                print(f"Brak bazowego GGUF: {base_gguf}")
                print("Wygeneruj go:")
                print(f"  python3 ~/llama.cpp/convert_hf_to_gguf.py models/qwen3.5-0.8b-base --outfile output/qwen-base-f16.gguf --outtype f16")
                print(f"  ~/llama.cpp/build/bin/llama-quantize output/qwen-base-f16.gguf output/qwen-base-Q5_K_M.gguf Q5_K_M")
                return
            r1 = run_benchmark(lambda t, i: predict_gguf(base_gguf, t, i), tests, "GGUF bazowy (Q5_K_M)")
            all_results.append(r1)
            # Zwolnij model
            global _llm, _llm_path
            del _llm; _llm = None; _llm_path = None
            r2 = run_benchmark(lambda t, i: predict_gguf(args.gguf, t, i), tests, f"GGUF fine-tuned ({os.path.basename(args.gguf)})")
            all_results.append(r2)
        else:
            # HF compare: bazowy vs fine-tuned
            r1 = run_benchmark(lambda t, i: predict_hf(t, i, lora_path=None), tests, "HF bazowy (bez LoRA)")
            all_results.append(r1)
            # Zwolnij model
            global _hf_model, _hf_tokenizer
            _hf_model = None; _hf_tokenizer = None
            import torch; torch.cuda.empty_cache()
            lora = args.lora or os.path.join(ROOT, "output", "qwen-all-lora")
            if not os.path.exists(lora):
                lora = os.path.join(ROOT, "output", "qwen-guard-lora")
            r2 = run_benchmark(lambda t, i: predict_hf(t, i, lora_path=lora), tests, f"HF fine-tuned ({os.path.basename(lora)})")
            all_results.append(r2)

    # --base: tylko bazowy
    elif args.base:
        if args.gguf:
            # Bazowy GGUF — szukaj lub każ wygenerowac
            base_gguf = args.gguf  # user moze podac sciezke do bazowego GGUF
            r = run_benchmark(lambda t, i: predict_gguf(base_gguf, t, i), tests, f"GGUF bazowy ({os.path.basename(base_gguf)})")
        else:
            r = run_benchmark(lambda t, i: predict_hf(t, i, lora_path=None), tests, "HF bazowy (bez LoRA)")
        all_results.append(r)

    # --gguf: fine-tuned GGUF
    elif args.gguf:
        if not os.path.exists(args.gguf):
            print(f"Brak pliku: {args.gguf}")
            return
        r = run_benchmark(lambda t, i: predict_gguf(args.gguf, t, i), tests, f"GGUF: {os.path.basename(args.gguf)}")
        all_results.append(r)

    # Domyslne: fine-tuned HF
    else:
        lora_path = args.lora or os.path.join(ROOT, "output", "qwen-all-lora")
        if not os.path.exists(lora_path):
            lora_path = os.path.join(ROOT, "output", "qwen-guard-lora")
        if not os.path.exists(lora_path):
            print(f"Brak LoRA: {lora_path}")
            return
        r = run_benchmark(lambda t, i: predict_hf(t, i, lora_path), tests, f"HF fine-tuned: {os.path.basename(lora_path)}")
        all_results.append(r)

    # Podsumowanie porownawcze
    if len(all_results) > 1:
        print(f"\n{'='*70}")
        print(f"  POROWNANIE")
        print(f"{'='*70}")
        print(f"  {'Model':<35} {'Exact':>8} {'Partial':>8} {'Time':>8}")
        print(f"  {'-'*60}")
        for r in all_results:
            print(f"  {r['label']:<35} {r['overall_exact']:>7.1f}% {r['overall_partial']:>7.1f}% {r['avg_time']:>7.3f}s")

    # Zapisz wyniki
    out_path = os.path.join(ROOT, "output", "benchmark_all_results.json")
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    with open(out_path, "w") as f:
        json.dump(all_results if len(all_results) > 1 else all_results[0], f, indent=2, ensure_ascii=False)
    print(f"\nWyniki: {out_path}")


if __name__ == "__main__":
    main()
