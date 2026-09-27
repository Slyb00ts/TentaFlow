#!/usr/bin/env python3
# =============================================================================
# Plik: scripts/build_pl_dataset.py
# Opis: Buduje zbalansowany zbiór treningowy do nauki języka polskiego
#       z mechanizmem ochronnym przed katastrofalnym zapominaniem (Replay Buffer).
#
# Proporcje docelowe:
#   - ~60% Polish Instruction / Conversation (Bielik, Aya, OWCA)
#   - ~25% English Reasoning Replay (GSM8k, logic, math)
#   - ~15% TentaFlow Domain Anchor (guard, toolcalling, intent z data/)
# =============================================================================

import argparse
import json
import os
import random
from typing import List, Dict, Any

ROOT_DIR = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
DATA_DIR = os.path.join(ROOT_DIR, "data")
DATA_PL_DIR = os.path.join(ROOT_DIR, "data_pl")
RAW_DIR = os.path.join(DATA_PL_DIR, "raw")
REPLAY_DIR = os.path.join(DATA_PL_DIR, "replay_en")
ANCHOR_DIR = os.path.join(DATA_PL_DIR, "tentaflow_anchor")
BENCH_DIR = os.path.join(DATA_PL_DIR, "benchmarks")
PROCESSED_DIR = os.path.join(DATA_PL_DIR, "processed")

os.makedirs(ANCHOR_DIR, exist_ok=True)
os.makedirs(BENCH_DIR, exist_ok=True)
os.makedirs(PROCESSED_DIR, exist_ok=True)


# =============================================================================
# 1. Tworzenie testu lingwistycznego języka polskiego (Linguistic Benchmark)
# =============================================================================
POLISH_LINGUISTIC_TESTS = [
    {
        "id": "genitive_negation_1",
        "category": "fleksja_przeczenie",
        "prompt": "Uzupełnij poprawną formą słowa 'chleb' w zdaniu: 'Wczoraj w sklepie nie kupiłem ani jednego [chleb]'. Podaj całe zdanie.",
        "expected_substrings": ["chleba"],
        "forbidden_substrings": ["chleb ", "chlebu"],
        "explanation": "Po czasowniku zaprzeczonym w języku polskim stosujemy dopełniacz (kogo? czego? -> chleba), a nie biernik."
    },
    {
        "id": "genitive_negation_2",
        "category": "fleksja_przeczenie",
        "prompt": "Przekształć zdanie na przeczące: 'Mam czas na rozmowę'.",
        "expected_substrings": ["Nie mam czasu"],
        "forbidden_substrings": ["Nie mam czas"],
        "explanation": "Przeczenie wymaga formy dopełniacza: 'Nie mam czasu'."
    },
    {
        "id": "vocative_1",
        "category": "wołacz",
        "prompt": "Napisz oficjalne powitanie mailowe do pana o imieniu Krzysztof i nazwisku Kowalski. Użyj wołacza.",
        "expected_substrings": ["Krzysztofie", "Panie Krzysztofie"],
        "forbidden_substrings": ["Panie Krzysztof,"],
        "explanation": "Poprawna forma wołacza to 'Panie Krzysztofie'."
    },
    {
        "id": "vocative_2",
        "category": "wołacz",
        "prompt": "Zwróć się bezpośrednio do koleżanki o imieniu Kasia.",
        "expected_substrings": ["Kasiu"],
        "forbidden_substrings": [],
        "explanation": "Wołacz imienia Kasia to Kasiu."
    },
    {
        "id": "numeral_agreement_1",
        "category": "liczebniki",
        "prompt": "Jak poprawnie powiedzieć po polsku liczbę 24 z rzeczownikiem 'okno'? (np. w pokoju są...)",
        "expected_substrings": ["dwadzieścia cztery okna"],
        "forbidden_substrings": ["dwadzieścia cztery okien", "dwadzieścia cztery okno"],
        "explanation": "Liczebniki kończące się na 2, 3, 4 (z wyjątkiem 12, 13, 14) łączą się z mianownikiem l. mnogiej: 24 okna."
    },
    {
        "id": "numeral_agreement_2",
        "category": "liczebniki",
        "prompt": "Wybierz poprawną formę: 'W konferencji wzięło udział [pięć / pięcioro] profesorów'.",
        "expected_substrings": ["pięciu profesorów", "pięcioro profesorów"],
        "forbidden_substrings": ["pięć profesorów"],
        "explanation": "Męskoosobowa forma wymaga 'pięciu profesorów' lub liczebnika zbiorowego."
    },
    {
        "id": "idiom_1",
        "category": "związki_frazeologiczne",
        "prompt": "Co oznacza polski związek frazeologiczny 'wiercić komuś dziurę w brzuchu'? Wyjaśnij krótko w jednym zdaniu.",
        "expected_substrings": ["nalegać", "napkrzykrzać", "męczyć", "natrętnie", "dopytywać", "prosić"],
        "forbidden_substrings": ["fizycznie", "dziura w ciele"],
        "explanation": "Związek oznacza natrętne proszenie lub dopytywanie."
    },
    {
        "id": "idiom_2",
        "category": "związki_frazeologiczne",
        "prompt": "Co oznacza powiedzenie 'rzucić grochem o ścianę'?",
        "expected_substrings": ["daremny", "bezskuteczn", "nie słucha", "bez efektu", "nie reaguje"],
        "forbidden_substrings": [],
        "explanation": "Oznacza daremny wysiłek, mówienie do kogoś, kto nie słucha."
    },
    {
        "id": "false_friends_1",
        "category": "falszywi_przyjaciele",
        "prompt": "Co oznacza angielskie słowo 'actually' i dlaczego błędem jest tłumaczenie go jako 'aktualnie'? Odpowiedz zwięźle.",
        "expected_substrings": ["w rzeczywistości", "faktycznie", "właściwie", "naprawdę"],
        "forbidden_substrings": [],
        "explanation": "'Actually' oznacza 'właściwie/faktycznie', natomiast 'aktualnie' to 'currently'."
    },
    {
        "id": "particle_distinction_1",
        "category": "drobne_niuanse",
        "prompt": "Jaka jest różnica w znaczeniu między słowami 'przynajmniej' a 'bynajmniej'? Podaj po jednym przykładzie użycia.",
        "expected_substrings": ["przynajmniej", "bynajmniej", "wcale", "chociaż"],
        "forbidden_substrings": [],
        "explanation": "'Bynajmniej' to zaprzeczenie ('wcale nie'), 'przynajmniej' to 'chociaż'."
    },
    {
        "id": "formal_register_1",
        "category": "rejestr_formalny",
        "prompt": "Napisz krótkie oficjalne podanie do dziekana uczelni z prośbą o przesunięcie terminu egzaminu z powodu choroby. Zadbaj o elegancki styl urzędowy.",
        "expected_substrings": ["Szanowny Panie Dziekanie", "Zwracam się z uprzejmą prośbą", "Z poważaniem"],
        "forbidden_substrings": ["Hej", "Cześć", "Nara"],
        "explanation": "Wymaga formalnego stylu kancelaryjnego i zwrotów grzecznościowych."
    },
    {
        "id": "complex_grammar_1",
        "category": "imieslowy",
        "prompt": "Popraw błąd w zdaniu: 'Idąc do pracy, zaczął padać deszcz'. Dlaczego to zdanie jest błędne?",
        "expected_substrings": ["imiesłów", "podmiot", "kiedy szedłem", "gdy szedłem"],
        "forbidden_substrings": [],
        "explanation": "Błąd tzw. imiesłowowego równoważnika zdania (deszcz nie szedł do pracy)."
    }
]


def create_linguistic_benchmark():
    dest = os.path.join(BENCH_DIR, "polish_linguistic_test.jsonl")
    print(f"\n[1/4] Zapisywanie testu lingwistycznego języka polskiego -> {dest}")
    with open(dest, "w", encoding="utf-8") as f:
        for t in POLISH_LINGUISTIC_TESTS:
            f.write(json.dumps(t, ensure_ascii=False) + "\n")
    print(f"  -> Zapisano {len(POLISH_LINGUISTIC_TESTS)} precyzyjnych testów lingwistycznych.")


# =============================================================================
# 2. Pobieranie próbek kotwiczących z TentaFlow (Anchor Data)
# =============================================================================
def extract_tentaflow_anchors(guard_count: int = 2000, tool_count: int = 1500, intent_count: int = 1500):
    dest = os.path.join(ANCHOR_DIR, "tentaflow_domain_anchors.jsonl")
    eval_dest = os.path.join(BENCH_DIR, "tentaflow_regression_eval.jsonl")
    print(f"\n[2/4] Ekstrakcja kotwic TentaFlow (Guard + Tools + Intent) -> {dest}")

    anchors = []
    eval_anchors = []

    # 1. Guard train & eval
    guard_train_path = os.path.join(DATA_DIR, "guard", "qwen_train.jsonl")
    guard_eval_path = os.path.join(DATA_DIR, "guard", "qwen_eval.jsonl")
    if os.path.exists(guard_train_path):
        with open(guard_train_path, "r", encoding="utf-8") as f:
            lines = [json.loads(line) for line in f if line.strip()]
        random.seed(42)
        random.shuffle(lines)
        for r in lines[:guard_count]:
            anchors.append({"messages": r["messages"], "metadata": {"task": "guard", "domain": "security"}})
        print(f"  -> Pobrano {min(guard_count, len(lines))} próbek guardrails.")

    if os.path.exists(guard_eval_path):
        with open(guard_eval_path, "r", encoding="utf-8") as f:
            lines = [json.loads(line) for line in f if line.strip()]
        random.seed(42)
        random.shuffle(lines)
        for r in lines[:200]:
            eval_anchors.append({"messages": r["messages"], "metadata": {"task": "guard"}})

    # 2. Toolcalling train
    tool_train_path = os.path.join(DATA_DIR, "toolcalling", "qwen_train.jsonl")
    if os.path.exists(tool_train_path):
        with open(tool_train_path, "r", encoding="utf-8") as f:
            lines = [json.loads(line) for line in f if line.strip()]
        random.seed(42)
        random.shuffle(lines)
        for r in lines[:tool_count]:
            anchors.append({"messages": r["messages"], "metadata": {"task": "toolcalling", "domain": "tools"}})
        print(f"  -> Pobrano {min(tool_count, len(lines))} próbek toolcalling.")

    # 3. Intent train
    intent_train_path = os.path.join(DATA_DIR, "intent", "qwen_train.jsonl")
    if os.path.exists(intent_train_path):
        with open(intent_train_path, "r", encoding="utf-8") as f:
            lines = [json.loads(line) for line in f if line.strip()]
        random.seed(42)
        random.shuffle(lines)
        for r in lines[:intent_count]:
            anchors.append({"messages": r["messages"], "metadata": {"task": "intent", "domain": "routing"}})
        print(f"  -> Pobrano {min(intent_count, len(lines))} próbek intent routing.")

    # Zapis kotwic
    with open(dest, "w", encoding="utf-8") as f:
        for a in anchors:
            f.write(json.dumps(a, ensure_ascii=False) + "\n")
    print(f"  -> Zapisano łącznie {len(anchors)} kotwic TentaFlow.")

    # Zapis ewaluacji regresji
    with open(eval_dest, "w", encoding="utf-8") as f:
        for ea in eval_anchors:
            f.write(json.dumps(ea, ensure_ascii=False) + "\n")
    print(f"  -> Zapisano {len(eval_anchors)} próbek regresyjnych w {eval_dest}.")

    return len(anchors)


# =============================================================================
# 3. Ładowanie i łączenie zbiorów według receptury miksu
# =============================================================================
def load_jsonl_messages(path: str, max_count: int = 0) -> List[Dict[str, Any]]:
    records = []
    if not os.path.exists(path):
        print(f"  [Ostrzeżenie] Brak pliku {path}")
        return records
    with open(path, "r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                try:
                    data = json.loads(line)
                    if "messages" in data and len(data["messages"]) >= 2:
                        records.append(data)
                except json.JSONDecodeError:
                    continue
    if max_count > 0 and len(records) > max_count:
        random.seed(42)
        random.shuffle(records)
        records = records[:max_count]
    return records


def build_combined_dataset(
    bielik_count: int = 10300,
    aya_count: int = 10000,
    owca_count: int = 8000,
    gsm8k_count: int = 5000,
    anchor_count: int = 5000,
    val_ratio: float = 0.05
):
    print("\n[3/4] Łączenie zbiorów w zbalansowany mix treningowy...")

    # 1. Polish instruction data (~60%)
    bielik_path = os.path.join(RAW_DIR, "bielik_distill_10k.jsonl")
    aya_path = os.path.join(RAW_DIR, "aya_polish.jsonl")
    owca_path = os.path.join(RAW_DIR, "owca_polish.jsonl")

    bielik_data = load_jsonl_messages(bielik_path, bielik_count)
    aya_data = load_jsonl_messages(aya_path, aya_count)
    owca_data = load_jsonl_messages(owca_path, owca_count)

    polish_total = len(bielik_data) + len(aya_data) + len(owca_data)
    print(f"  -> Język polski: {polish_total} próbek (Bielik: {len(bielik_data)}, Aya: {len(aya_data)}, OWCA: {len(owca_data)})")

    # 2. English Replay Buffer (~20-25%)
    gsm8k_path = os.path.join(REPLAY_DIR, "gsm8k_reasoning.jsonl")
    gsm8k_data = load_jsonl_messages(gsm8k_path, gsm8k_count)
    print(f"  -> English Replay Buffer: {len(gsm8k_data)} próbek (GSM8k reasoning)")

    # 3. TentaFlow Domain Anchors (~10-15%)
    anchor_path = os.path.join(ANCHOR_DIR, "tentaflow_domain_anchors.jsonl")
    anchor_data = load_jsonl_messages(anchor_path, anchor_count)
    print(f"  -> TentaFlow Domain Anchors: {len(anchor_data)} próbek")

    # Połączenie wszystkich zbiorów
    all_records = []
    for r in bielik_data:
        r["lang"] = "pl"
        r["type"] = "instruct_pl_bielik"
        all_records.append(r)
    for r in aya_data:
        r["lang"] = "pl"
        r["type"] = "instruct_pl_aya"
        all_records.append(r)
    for r in owca_data:
        r["lang"] = "pl"
        r["type"] = "instruct_pl_owca"
        all_records.append(r)
    for r in gsm8k_data:
        r["lang"] = "en"
        r["type"] = "replay_en_gsm8k"
        all_records.append(r)
    for r in anchor_data:
        r["lang"] = "en_pl"
        r["type"] = "domain_tentaflow"
        all_records.append(r)

    total_samples = len(all_records)
    print(f"\n[4/4] Wszystkich próbek łącznie: {total_samples}")

    # Tasowanie
    random.seed(2026)
    random.shuffle(all_records)

    # Podział Train / Val
    val_size = int(total_samples * val_ratio)
    val_records = all_records[:val_size]
    train_records = all_records[val_size:]

    train_path = os.path.join(PROCESSED_DIR, "train_pl_combined.jsonl")
    val_path = os.path.join(PROCESSED_DIR, "val_pl_combined.jsonl")
    summary_path = os.path.join(PROCESSED_DIR, "dataset_summary.json")

    with open(train_path, "w", encoding="utf-8") as f:
        for r in train_records:
            clean_rec = {"messages": r["messages"]}
            f.write(json.dumps(clean_rec, ensure_ascii=False) + "\n")

    with open(val_path, "w", encoding="utf-8") as f:
        for r in val_records:
            clean_rec = {"messages": r["messages"]}
            f.write(json.dumps(clean_rec, ensure_ascii=False) + "\n")

    summary = {
        "total_samples": total_samples,
        "train_samples": len(train_records),
        "val_samples": len(val_records),
        "breakdown": {
            "polish_instruction": {
                "count": polish_total,
                "percentage": round(polish_total / total_samples * 100, 2),
                "subsets": {
                    "bielik_distill_10k": len(bielik_data),
                    "aya_polish": len(aya_data),
                    "owca_polish": len(owca_data)
                }
            },
            "english_replay_buffer": {
                "count": len(gsm8k_data),
                "percentage": round(len(gsm8k_data) / total_samples * 100, 2),
                "subsets": {
                    "gsm8k_reasoning": len(gsm8k_data)
                }
            },
            "tentaflow_anchors": {
                "count": len(anchor_data),
                "percentage": round(len(anchor_data) / total_samples * 100, 2),
                "subsets": {
                    "guard_tool_intent": len(anchor_data)
                }
            }
        },
        "output_files": {
            "train": train_path,
            "val": val_path
        }
    }

    with open(summary_path, "w", encoding="utf-8") as f:
        json.dump(summary, f, indent=2, ensure_ascii=False)

    print("\n" + "=" * 60)
    print("Zbalansowany zbiór danych został pomyślnie zbudowany!")
    print(f"  Train:     {train_path} ({len(train_records)} próbek)")
    print(f"  Val:       {val_path} ({len(val_records)} próbek)")
    print(f"  Statystyki: {summary_path}")
    print(f"  Udział j. polskiego:     {summary['breakdown']['polish_instruction']['percentage']}%")
    print(f"  Udział Replay EN (Math): {summary['breakdown']['english_replay_buffer']['percentage']}%")
    print(f"  Udział Kotwic TentaFlow: {summary['breakdown']['tentaflow_anchors']['percentage']}%")
    print("=" * 60)


def main():
    parser = argparse.ArgumentParser(description="Buduje zbalansowany mix danych treningowych PL + Replay + TentaFlow.")
    parser.add_argument("--bielik-count", type=int, default=10300)
    parser.add_argument("--aya-count", type=int, default=10000)
    parser.add_argument("--owca-count", type=int, default=8000)
    parser.add_argument("--gsm8k-count", type=int, default=5000)
    parser.add_argument("--anchor-count", type=int, default=5000)
    parser.add_argument("--val-ratio", type=float, default=0.05)
    args = parser.parse_args()

    create_linguistic_benchmark()
    extract_tentaflow_anchors(guard_count=2000, tool_count=1500, intent_count=1500)
    build_combined_dataset(
        bielik_count=args.bielik_count,
        aya_count=args.aya_count,
        owca_count=args.owca_count,
        gsm8k_count=args.gsm8k_count,
        anchor_count=args.anchor_count,
        val_ratio=args.val_ratio
    )


if __name__ == "__main__":
    main()
