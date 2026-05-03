# TentaFlow Models — Trening

## Wymagania

```bash
cd ~/repos/rust/TentaFlow/tentaflow-models
source .venv/bin/activate
```

## Pobranie modeli (jednorazowo)

```bash
python3 scripts/download_models.py
```

## Generowanie danych

```bash
./scripts/generate.sh help              # pełna pomoc

# Wszystkie datasety
./scripts/generate.sh 10                # 10 iteracji wszystkiego

# Poszczególne datasety
./scripts/generate.sh intent 30         # intent router (50 rec/iter)
./scripts/generate.sh guard 20          # guard short + extended
./scripts/generate.sh guard short 50    # guard short only
./scripts/generate.sh model 20          # model router (30 rec/iter)
./scripts/generate.sh plan 20           # execution planning (15 rec/iter)
./scripts/generate.sh check 20          # result validation (30 rec/iter)
./scripts/generate.sh toolcalling 20    # tool calling (auto z addonów, 30 rec/iter)
./scripts/generate.sh memory 30         # memory wszystkie podtypy (round-robin)
./scripts/generate.sh memory documents 20
./scripts/generate.sh memory conversations 20
./scripts/generate.sh memory transcripts 20
./scripts/generate.sh memory summaries 20
./scripts/generate.sh memory extract 20
```

## Konwersja na format treningowy

```bash
python3 scripts/convert.py              # wszystko
python3 scripts/convert.py intent       # intent only
python3 scripts/convert.py guard        # guard only
python3 scripts/convert.py model        # model router only
python3 scripts/convert.py plan         # plan only
python3 scripts/convert.py check        # check only
python3 scripts/convert.py toolcalling  # toolcalling only
python3 scripts/convert.py memory       # memory only (wszystkie podtypy)
```

**Odpalaj za każdym razem gdy dogenerujesz nowe dane.**

## Trening

```bash
# Qwen na wszystkich datasetach
python3 scripts/train.py

# Qwen na konkretnym datasecie
python3 scripts/train.py guard
python3 scripts/train.py intent
python3 scripts/train.py model
python3 scripts/train.py plan
python3 scripts/train.py check
python3 scripts/train.py toolcalling
python3 scripts/train.py memory

# Llama Prompt Guard (guard short only)
python3 scripts/train.py guard --model llama

# Kontynuacja treningu z checkpointu
python3 scripts/train.py guard --resume output/qwen-guard-lora/checkpoint-270
```

## Retrain (pełny pipeline)

```bash
./scripts/retrain.sh                    # all, kontynuuj trening
./scripts/retrain.sh guard              # guard only, kontynuuj
./scripts/retrain.sh --fresh            # all od zera (usuwa starą LoRA)
./scripts/retrain.sh memory --fresh     # memory od zera
./scripts/retrain.sh --help             # pomoc
```

Pipeline: convert → train → merge LoRA → GGUF F16 → kwantyzacje (Q8, Q6, Q5, Q4, Q3, Q2)

## Benchmark

```bash
python3 scripts/benchmark.py                              # domyślne modele
python3 scripts/benchmark.py --models ft gguf-q5 haiku    # konkretne
python3 scripts/benchmark.py --models all                 # WSZYSTKO
```

## Eksport GGUF (ręczny)

```bash
./scripts/export_gguf.sh output/qwen-guard-lora guard
```

---

## Datasety

| Dataset | Token | Prompt | Rec/iter | Cel |
|---------|-------|--------|----------|-----|
| Intent router | `<\|intent\|>` | `intent_router.md` | 50 | Routing: TEXT,TOOLS,MODEL,MEMORY,FEEDBACK,RECALL,EXTRACT,PLAN |
| Guard | `<\|guard\|>` | `guard_short.md`, `guard_extended.md` | 50/15 | Security: 0=safe, 1=injection, 2=jailbreak |
| Model router | `<\|model\|>` | `model_router.md` | 30 | Wybór modelu LLM lub #UNAVAILABLE |
| Plan | `<\|plan\|>` | `plan.md` | 15 | Wielokrokowy plan lub #ASK |
| Check | `<\|check\|>` | `check.md` | 30 | OK / RETRY\|fix=... / ESCALATE\|reason=... |
| Toolcalling | `<\|tools\|>` | auto z addonów | 30 | TOON lub #UNAVAILABLE/#MISSING |
| Memory docs | `<\|memory\|>` | `memory_documents.md` | 20 | Wyciąganie faktów z dokumentów |
| Memory conv | `<\|memory\|>` | `memory_conversations.md` | 15 | Podsumowanie rozmów |
| Memory trans | `<\|memory\|>` | `memory_transcripts.md` | 10 | Transkrypcje spotkań |
| Memory sum | `<\|summary\|>` | `memory_summaries.md` | 15 | Hierarchiczne L1/L2/TOP |
| Memory extract | `<\|extract\|>` | `memory_extract.md` | 15 | Wyciąganie pól z dokumentów |

## Struktura

```
tentaflow-models/
├── MODEL.md                         # Dokumentacja modelu (architektura, tokeny, flow)
├── TRAINING.md                      # Ten plik
├── data/
│   ├── intent/                      # Intent router
│   ├── guard/                       # Security guard
│   ├── model/                       # Model router
│   ├── plan/                        # Execution planning
│   ├── check/                       # Result validation
│   ├── toolcalling/                 # Tool calling
│   └── memory/                      # Memory (6 podtypów)
│       ├── documents.jsonl
│       ├── conversations.jsonl
│       ├── transcripts.jsonl
│       ├── summaries.jsonl
│       └── extract.jsonl
├── prompts/                         # Prompty generatorów
├── models/                          # Pobrane modele bazowe
├── output/                          # LoRA adaptery + GGUF
├── scripts/
│   ├── generate.sh                  # Generowanie danych
│   ├── generate_toolcalling.py      # Auto-generowanie z addonów
│   ├── convert.py                   # Konwersja na format treningowy
│   ├── train.py                     # Trening (Qwen / Llama)
│   ├── retrain.sh                   # Pełny pipeline
│   ├── benchmark.py                 # Porównanie modeli
│   ├── export_gguf.sh               # LoRA → GGUF
│   ├── filter_jsonl.py              # Filtr JSONL
│   └── download_models.py           # Pobieranie modeli
└── toolcalling/
    └── TOOL_FLOW.md                 # Architektura pipeline tool calling
```

## Modele

| Model | Zadanie | Dane | Output |
|-------|---------|------|--------|
| Qwen/Qwen3.5-0.8B | ALL (orchestrator) | Wszystkie datasety | LoRA → GGUF Q5_K_M |
| Llama-Prompt-Guard-2-86M | guard only | guard short (<512 tok) | Pełny model |
