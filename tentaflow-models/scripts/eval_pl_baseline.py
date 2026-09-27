#!/usr/bin/env python3
# =============================================================================
# Plik: scripts/eval_pl_baseline.py
# Opis: Skrypt do ewaluacji baseline (stanu wyjściowego) oraz modeli po treningu.
#       Testuje:
#         1. Testy lingwistyczne j. polskiego (fleksja, wołacz, idiomy, zaprzeczenia)
#         2. Zdolności konwersacyjne (próbka z Polish MT-Bench)
#         3. Test regresyjny TentaFlow (bezpieczeństwo / guard / routing)
#
# Obsługuje dwa tryby generacji:
#   A. HuggingFace Transformers (lokalny model lub wagi HF)
#   B. HTTP OpenAI-compatible endpoint (vLLM, Ollama, TentaFlow, LiteLLM)
# =============================================================================

import argparse
import json
import os
import re
import sys
import time
from datetime import datetime
from typing import List, Dict, Any, Optional

ROOT_DIR = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
BENCH_DIR = os.path.join(ROOT_DIR, "data_pl", "benchmarks")
REPORTS_DIR = os.path.join(ROOT_DIR, "output", "eval_reports")
os.makedirs(REPORTS_DIR, exist_ok=True)


class ModelClient:
    """Uniwersalny interfejs generacyjny: HuggingFace lub OpenAI API / vLLM / Ollama."""

    def __init__(self, model_name_or_path: str, api_url: Optional[str] = None, api_key: str = "EMPTY"):
        self.model_name = model_name_or_path
        self.api_url = api_url
        self.api_key = api_key
        self.hf_model = None
        self.hf_tokenizer = None

        if not api_url:
            print(f"[ModelClient] Inicjalizacja HuggingFace dla: {model_name_or_path}...")
            import torch
            from transformers import AutoModelForCausalLM, AutoTokenizer, BitsAndBytesConfig

            self.hf_tokenizer = AutoTokenizer.from_pretrained(model_name_or_path, trust_remote_code=True)
            # Próba załadowania w 4-bit dla oszczędności pamięci przy dużych modelach
            bnb_config = BitsAndBytesConfig(
                load_in_4bit=True,
                bnb_4bit_quant_type="nf4",
                bnb_4bit_compute_dtype=torch.bfloat16
            )
            try:
                self.hf_model = AutoModelForCausalLM.from_pretrained(
                    model_name_or_path,
                    quantization_config=bnb_config,
                    device_map="auto",
                    trust_remote_code=True
                )
            except Exception as e:
                print(f"[ModelClient] Ostrzeżenie przy 4-bit ({e}), próba bfloat16/float16...")
                self.hf_model = AutoModelForCausalLM.from_pretrained(
                    model_name_or_path,
                    torch_dtype=torch.bfloat16 if torch.cuda.is_available() and torch.cuda.is_bf16_supported() else torch.float16,
                    device_map="auto",
                    trust_remote_code=True
                )
        else:
            print(f"[ModelClient] Używanie OpenAI-compatible API pod adresem: {api_url} (model: {model_name_or_path})")

    def generate(self, messages: List[Dict[str, str]], max_tokens: int = 512, temperature: float = 0.2) -> str:
        if self.api_url:
            import urllib.request
            url = self.api_url.rstrip("/") + "/chat/completions"
            payload = {
                "model": self.model_name,
                "messages": messages,
                "max_tokens": max_tokens,
                "temperature": temperature
            }
            req = urllib.request.Request(
                url,
                data=json.dumps(payload).encode("utf-8"),
                headers={"Content-Type": "application/json", "Authorization": f"Bearer {self.api_key}"},
                method="POST"
            )
            with urllib.request.urlopen(req, timeout=120) as resp:
                data = json.loads(resp.read().decode("utf-8"))
                return data["choices"][0]["message"]["content"].strip()
        else:
            import torch
            prompt = self.hf_tokenizer.apply_chat_template(messages, tokenize=False, add_generation_prompt=True)
            inputs = self.hf_tokenizer(prompt, return_tensors="pt").to(self.hf_model.device)
            with torch.no_grad():
                outputs = self.hf_model.generate(
                    **inputs,
                    max_new_tokens=max_tokens,
                    temperature=temperature,
                    do_sample=(temperature > 0.0),
                    pad_token_id=self.hf_tokenizer.eos_token_id
                )
            prompt_len = inputs["input_ids"].shape[1]
            generated_tokens = outputs[0][prompt_len:]
            return self.hf_tokenizer.decode(generated_tokens, skip_special_tokens=True).strip()


def run_linguistic_benchmark(client: ModelClient) -> Dict[str, Any]:
    test_path = os.path.join(BENCH_DIR, "polish_linguistic_test.jsonl")
    if not os.path.exists(test_path):
        print(f"[Błąd] Brak pliku {test_path}. Uruchom build_pl_dataset.py.")
        return {}

    with open(test_path, "r", encoding="utf-8") as f:
        tests = [json.loads(line) for line in f if line.strip()]

    print(f"\n" + "=" * 60)
    print(f"1. URUCHAMIANIE TESTU LINGWISTYCZNEGO JĘZYKA POLSKIEGO ({len(tests)} pytań)")
    print("=" * 60)

    results = []
    passed = 0

    for i, t in enumerate(tests, 1):
        prompt = t["prompt"]
        messages = [
            {"role": "system", "content": "Jesteś ekspertem języka polskiego. Odpowiadaj zwięźle, precyzyjnie i poprawnie gramatycznie."},
            {"role": "user", "content": prompt}
        ]
        try:
            resp = client.generate(messages, max_tokens=256, temperature=0.0)
        except Exception as e:
            resp = f"ERROR: {e}"

        resp_lower = resp.lower()

        # Sprawdzenie oczekiwanych podciągów
        has_expected = False
        if t["expected_substrings"]:
            has_expected = any(sub.lower() in resp_lower for sub in t["expected_substrings"])
        else:
            has_expected = True

        # Sprawdzenie zabronionych form błędnych
        has_forbidden = False
        if t.get("forbidden_substrings"):
            has_forbidden = any(sub.lower() in resp_lower for sub in t["forbidden_substrings"])

        is_correct = has_expected and not has_forbidden
        if is_correct:
            passed += 1

        status_str = "PASS [OK]" if is_correct else "FAIL [X]"
        print(f"[{i:02d}/{len(tests):02d}] {status_str} Kategoria: {t['category']}")
        print(f"     Pytanie:  {prompt[:70]}...")
        print(f"     Odpowiedź: {resp[:90]}...")
        if not is_correct:
            print(f"     Wymagane: {t['expected_substrings']} | Zakazane: {t.get('forbidden_substrings', [])}")

        results.append({
            "id": t["id"],
            "category": t["category"],
            "prompt": prompt,
            "response": resp,
            "is_correct": is_correct,
            "expected": t["expected_substrings"],
            "forbidden": t.get("forbidden_substrings", []),
            "explanation": t.get("explanation", "")
        })

    score_pct = round((passed / len(tests)) * 100, 2)
    print(f"\n>>> Wynik testu lingwistycznego: {passed}/{len(tests)} ({score_pct}%)")
    return {"total": len(tests), "passed": passed, "score_pct": score_pct, "details": results}


def run_mt_bench_sample(client: ModelClient, sample_count: int = 8) -> Dict[str, Any]:
    bench_path = os.path.join(BENCH_DIR, "mt_bench_pl.jsonl")
    if not os.path.exists(bench_path):
        print(f"[Ostrzeżenie] Brak {bench_path}")
        return {}

    with open(bench_path, "r", encoding="utf-8") as f:
        bench_data = [json.loads(line) for line in f if line.strip()]

    # Wybór po jednym pytaniu z różnych kategorii
    categories = list({r["category"] for r in bench_data if "category" in r})
    selected = []
    for cat in categories[:sample_count]:
        cat_items = [r for r in bench_data if r.get("category") == cat]
        if cat_items:
            selected.append(cat_items[0])

    print(f"\n" + "=" * 60)
    print(f"2. URUCHAMIANIE TESTU WIELOTUROWEGO MT-BENCH PL ({len(selected)} konwersacji)")
    print("=" * 60)

    conv_results = []
    for i, item in enumerate(selected, 1):
        cat = item.get("category", "ogólna")
        turns = item.get("turns", [])
        if not turns:
            continue

        print(f"\n[Rozmowa {i}/{len(selected)}] Kategoria: {cat}")
        messages = [{"role": "system", "content": "Jesteś pomocnym asystentem AI. Odpowiadaj w sposób wyczerpujący i naturalny w języku polskim."}]

        turn_logs = []
        for t_idx, turn_text in enumerate(turns, 1):
            messages.append({"role": "user", "content": turn_text})
            print(f"  Tura {t_idx} (User): {turn_text[:80]}...")
            try:
                resp = client.generate(messages, max_tokens=384, temperature=0.3)
            except Exception as e:
                resp = f"ERROR: {e}"
            print(f"  Tura {t_idx} (Model): {resp[:120]}...")
            messages.append({"role": "assistant", "content": resp})
            turn_logs.append({"turn": t_idx, "prompt": turn_text, "response": resp})

        conv_results.append({
            "question_id": item.get("question_id"),
            "category": cat,
            "turns": turn_logs
        })

    return {"conversations_count": len(conv_results), "conversations": conv_results}


def run_tentaflow_regression(client: ModelClient, sample_count: int = 20) -> Dict[str, Any]:
    eval_path = os.path.join(BENCH_DIR, "tentaflow_regression_eval.jsonl")
    if not os.path.exists(eval_path):
        print(f"[Ostrzeżenie] Brak pliku {eval_path}")
        return {}

    with open(eval_path, "r", encoding="utf-8") as f:
        samples = [json.loads(line) for line in f if line.strip()]

    selected = samples[:sample_count]
    print(f"\n" + "=" * 60)
    print(f"3. TEST REGRESYJNY TENTAFLOW GUARDRAILS ({len(selected)} próbek)")
    print("=" * 60)

    passed = 0
    details = []

    for i, item in enumerate(selected, 1):
        msgs = item["messages"]
        expected = msgs[-1]["content"].strip()
        input_msgs = msgs[:-1]

        try:
            resp = client.generate(input_msgs, max_tokens=16, temperature=0.0)
        except Exception as e:
            resp = f"ERR: {e}"

        # Klasyfikator guard powinien zwrócić cyfrę 0, 1 lub 2
        clean_resp = re.sub(r"[^0-2]", "", resp)
        is_ok = (clean_resp == expected)
        if is_ok:
            passed += 1

        print(f"  [{i:02d}/{len(selected):02d}] {'OK ' if is_ok else 'ERR'} Oczekiwano: '{expected}' | Otrzymano: '{resp}'")
        details.append({
            "expected": expected,
            "received": resp,
            "is_correct": is_ok
        })

    score_pct = round((passed / len(selected)) * 100, 2)
    print(f"\n>>> Wynik testu regresji Guardrails: {passed}/{len(selected)} ({score_pct}%)")
    return {"total": len(selected), "passed": passed, "score_pct": score_pct, "details": details}


def generate_markdown_report(report_data: Dict[str, Any], output_path: str):
    model_name = report_data["model"]
    timestamp = report_data["timestamp"]
    ling = report_data.get("linguistic", {})
    regr = report_data.get("regression", {})
    convs = report_data.get("mt_bench", {}).get("conversations", [])

    md = []
    md.append(f"# Raport Ewaluacyjny Baseline: {model_name}")
    md.append(f"**Data wykonania:** {timestamp}\n")
    md.append("## Podsumowanie Wyników")
    md.append("| Metryka / Obszar | Wynik | Procent poprawności |")
    md.append("|---|---|---|")
    if ling:
        md.append(f"| **Poprawność Lingwistyczna PL** | {ling['passed']}/{ling['total']} | **{ling['score_pct']}%** |")
    if regr:
        md.append(f"| **Regresja Guardrails (Bezpieczeństwo)** | {regr['passed']}/{regr['total']} | **{regr['score_pct']}%** |")
    md.append(f"| **Przetestowane Dialogi MT-Bench PL** | {len(convs)} konwersacji | Zakończone |\n")

    if ling and "details" in ling:
        md.append("## Szczegóły Testów Lingwistycznych")
        md.append("| Kategoria | Pytanie | Odpowiedź Modelu | Status |")
        md.append("|---|---|---|---|")
        for d in ling["details"]:
            p = d["prompt"].replace("\n", " ")[:60]
            r = d["response"].replace("\n", " ")[:60]
            status = "✅ PASS" if d["is_correct"] else "❌ FAIL"
            md.append(f"| {d['category']} | {p}... | {r}... | {status} |")
        md.append("")

    if convs:
        md.append("## Przykłady Dialogów MT-Bench PL")
        for c in convs[:3]:
            md.append(f"### Kategoria: {c['category']}")
            for t in c["turns"]:
                md.append(f"**Użytkownik:** {t['prompt']}")
                md.append(f"**Model:** {t['response']}\n")

    with open(output_path, "w", encoding="utf-8") as f:
        f.write("\n".join(md))
    print(f"\n[Raport] Wygenerowano czytelny raport Markdown: {output_path}")


def main():
    parser = argparse.ArgumentParser(description="Ewaluacja baseline i post-train dla modeli w języku polskim.")
    parser.add_argument("--model", type=str, default="Qwen/Qwen2.5-0.5B-Instruct", help="Ścieżka do modelu HF lub nazwa repozytorium")
    parser.add_argument("--api-url", type=str, default=None, help="Opcjonalny OpenAI-compatible URL (np. http://localhost:8000/v1)")
    parser.add_argument("--api-key", type=str, default="EMPTY", help="Klucz API jeśli wymagany")
    parser.add_argument("--skip-mt-bench", action="store_true", help="Pomiń test MT-Bench")
    parser.add_argument("--skip-regression", action="store_true", help="Pomiń test regresji guardrails")
    parser.add_argument("--mt-bench-samples", type=int, default=5, help="Liczba kategorii MT-Bench do przetestowania")
    args = parser.parse_args()

    clean_model_name = os.path.basename(args.model.rstrip("/")).replace(":", "_")
    timestamp_str = datetime.now().strftime("%Y%m%d_%H%M%S")

    client = ModelClient(args.model, api_url=args.api_url, api_key=args.api_key)

    report = {
        "model": args.model,
        "timestamp": datetime.now().isoformat(),
        "linguistic": run_linguistic_benchmark(client),
        "regression": {} if args.skip_regression else run_tentaflow_regression(client),
        "mt_bench": {} if args.skip_mt_bench else run_mt_bench_sample(client, sample_count=args.mt_bench_samples)
    }

    json_report_path = os.path.join(REPORTS_DIR, f"baseline_report_{clean_model_name}_{timestamp_str}.json")
    md_report_path = os.path.join(REPORTS_DIR, f"baseline_report_{clean_model_name}_{timestamp_str}.md")

    with open(json_report_path, "w", encoding="utf-8") as f:
        json.dump(report, f, indent=2, ensure_ascii=False)
    print(f"[Raport] Zapisano raport JSON: {json_report_path}")

    generate_markdown_report(report, md_report_path)


if __name__ == "__main__":
    main()
