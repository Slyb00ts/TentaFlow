# TentaFlow — Fine-Tuning Języka Polskiego (Polish Language Adaptation)

Niniejszy przewodnik opisuje architekturę, pipeline danych, techniki SOTA zapobiegające katastrofalnemu zapominaniu (Catastrophic Forgetting) oraz procedury ewaluacji modeli wielojęzycznych (np. **Qwen 27B**, **Spark-X2.5**) w TentaFlow.

---

## 1. Architektura i Ochrona Inteligencji Ogólnej

Podczas adaptacji modelu do nowego języka największym ryzykiem jest **utrata zdolności logicznych, matematycznych i programistycznych** (model mówi płynnie po polsku, ale przestaje poprawnie myśleć, halucynuje lub gubi formatowanie JSON).

W TentaFlow wdrożono 4 mechanizmy obronne:

```
┌────────────────────────────────────────────────────────────────────────┐
│                        ZBALANSOWANY MIX DANYCH                         │
├──────────────────────────┬───────────────────────┬─────────────────────┤
│   Język Polski (~76%)    │  Replay Buffer (~13%) │ Kotwice TF (~10%)   │
│  - Bielik Distill 10k    │  - GSM8K Math         │ - Guard (ataki)     │
│  - Aya Collection PL     │  - Rozumowanie logicz.│ - Tool Calling      │
│  - OWCA (Alpaca PL)      │                       │ - Intent Routing    │
└──────────────────────────┴───────────────────────┴─────────────────────┘
```

1. **Replay Buffer (Bufor Powtórzeniowy)**:
   * Do danych treningowych wstrzykiwane są zadania matematyczne i logiczne (GSM8K). Zapobiega to degradacji reprezentacji wag logicznych modelu.
2. **Kotwice Domenowe TentaFlow (Zero-Regression Guard)**:
   * Zbiór zawiera próbki z istniejących datasetów TentaFlow (`guard`, `toolcalling`, `intent`). Dzięki temu model po douczeniu na język polski **nadal bezbłędnie klasyfikuje ataki (0/1/2) i wywołuje narzędzia**.
3. **High-Rank QLoRA ($r=64, \alpha=128$)**:
   * Wszystkie wagi bazowe modelu 27B pozostają **zamrożone** w 4-bit NF4.
   * Uczą się wyłącznie macierze adaptera nałożone na wszystkie warstwy liniowe (`q, k, v, o, gate, up, down`).
4. **NEFTune (Noisy Embeddings)**:
   * Podczas treningu dodawany jest kontrolowany szum Gaussa do wektorów wejściowych (`neftune_noise_alpha=5.0`), co zapobiega przeuczeniu do szablonów promptów.

---

## 2. Struktura Katalogów (`data_pl/`)

```
tentaflow-models/
├── data/                                 # [NIE RUSZANE] Istniejące dane (guard, intent, tools)
├── data_pl/                              # Nowy moduł języka polskiego
│   ├── raw/                              # Pobrane surowe zbiory po polsku
│   │   ├── bielik_distill_10k.jsonl      # 10 304 instrukcji z modelu Bielik-11B v3
│   │   ├── aya_polish.jsonl              # 14 864 instrukcji z Aya Collection (Cohere)
│   │   └── owca_polish.jsonl             # 15 000 instrukcji z polskiej wersji Alpaca
│   ├── replay_en/                        # Bufor zapobiegający degradacji logiki
│   │   └── gsm8k_reasoning.jsonl         # 5 000 problemów matematycznych i logicznych
│   ├── tentaflow_anchor/                 # Kotwice domenowe chroniące specyfikę TF
│   │   └── tentaflow_domain_anchors.jsonl# 3 874 próbek (guardrails + tools + intent)
│   ├── benchmarks/                       # Zbiory do pomiaru baseline i post-train
│   │   ├── mt_bench_pl.jsonl             # 80 wieloturowych pytań (konwersacja, roleplay, kod)
│   │   ├── polish_linguistic_test.jsonl  # Precyzyjne testy fleksji, wołacza i frazeologii
│   │   └── tentaflow_regression_eval.jsonl # 200 próbek weryfikacji regresji bezpieczeństwa
│   └── processed/                        # Gotowy, zbalansowany mix treningowy
│       ├── train_pl_combined.jsonl       # 35 316 próbek (ChatML messages)
│       ├── val_pl_combined.jsonl         # 1 858 próbek walidacyjnych
│       └── dataset_summary.json          # Metadane i statystyki podziału
└── scripts/
    ├── download_pl_datasets.py           # Narzędzie pobierania i aktualizacji danych
    ├── build_pl_dataset.py               # Narzędzie łączenia i tworzenia miksu
    ├── eval_pl_baseline.py               # Narzędzie pomiaru baseline i raportowania
    └── train_pl.py                       # Skrypt treningowy QLoRA dla modeli 27B / Spark
```

---

## 3. Krok 0 — Pomiar Baseline (Stan Wyjściowy Przed Treningiem)

Zanim uruchomicie jakikolwiek trening, zmierzcie aktualny poziom modelu (np. Qwen 27B lub Spark-X2.5), aby mieć punkt odniesienia.

### A. Ewaluacja przez API (vLLM / Ollama / TentaFlow Gateway)
Jeśli model jest już uruchomiony w vLLM, Ollamie lub na serwerze:
```bash
python3 scripts/eval_pl_baseline.py \
    --model "Qwen/Qwen2.5-27B-Instruct" \
    --api-url "http://localhost:8000/v1"
```

### B. Ewaluacja lokalna przez HuggingFace
```bash
python3 scripts/eval_pl_baseline.py \
    --model "/sciezka/do/modelu_lub_repo_HF" \
    --mt-bench-samples 8
```

Skrypt automatycznie:
1. Sprawdzi 12 kluczowych zasad języka polskiego (dopełniacz w zaprzeczeniach, wołacz, poprawność liczebników, idiomy, różnicę bynajmniej/przynajmniej).
2. Przetestuje wieloturowe dialogi konwersacyjne z MT-Bench PL.
3. Przeprowadzi test regresyjny klasyfikacji ataków (0/1/2).
4. Wygeneruje raport JSON i czytelny raport Markdown w `output/eval_reports/`.

---

## 4. Przebudowa lub aktualizacja datasetu

Jeśli chcecie zmienić proporcje lub dociągnąć więcej danych:
```bash
# Pobranie świeżych danych (jeśli potrzebne)
python3 scripts/download_pl_datasets.py

# Przebudowa miksu z własnymi proporcjami (np. większy bufor replay)
python3 scripts/build_pl_dataset.py \
    --bielik-count 10300 \
    --aya-count 10000 \
    --owca-count 8000 \
    --gsm8k-count 5000 \
    --anchor-count 4000
```

---

## 5. Uruchomienie Treningu (Gdy będziecie gotowi)

Skrypt `scripts/train_pl.py` został w pełni przygotowany i zoptymalizowany pod modele 27B i większe.

### Sprawdzenie poprawności bez dotykania GPU (Dry-Run):
```bash
python3 scripts/train_pl.py --model "Qwen/Qwen2.5-27B-Instruct" --dry-run
```

### Pełny trening na 1x GPU (np. RTX 3090 / 4090 / A100 - wymaga ~20 GB VRAM):
```bash
python3 scripts/train_pl.py \
    --model "Qwen/Qwen2.5-27B-Instruct" \
    --epochs 2 \
    --batch-size 2 \
    --grad-accum 8 \
    --lr 1.5e-4 \
    --lora-r 64 \
    --lora-alpha 128 \
    --output-dir "output/qwen-27b-pl-lora"
```

### Trening Multi-GPU z DeepSpeed ZeRO-2:
W katalogu `configs/` znajdują się gotowe konfiguracje DeepSpeed:
```bash
accelerate launch --config_file configs/deepspeed_zero2.json scripts/train_pl.py \
    --model "Qwen/Qwen2.5-27B-Instruct" \
    --output-dir "output/qwen-27b-pl-lora"
```

---

## 6. Weryfikacja Post-Training (A/B Test)

Po zakończeniu treningu uruchamiamy skrypt ewaluacyjny na wytrenowanym adapterze:
```bash
python3 scripts/eval_pl_baseline.py \
    --model "output/qwen-27b-pl-lora"
```

Porównanie raportu wyjściowego z raportem bazowym wskaże:
* O ile procent wzrosła poprawność gramatyczna i fleksyjna.
* Jak zmieniła się naturalność wieloturowej konwersacji w języku polskim.
* Czy model zachował 100% skuteczności w detekcji ataków i logice TentaFlow.
