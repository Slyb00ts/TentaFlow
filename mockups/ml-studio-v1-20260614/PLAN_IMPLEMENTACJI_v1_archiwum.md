# Moduł „ML Studio" — plan mockupów i realnej implementacji

Cel: jeden moduł TentaFlow do uczenia maszynowego, dotrenowywania modeli (LLM/vision/audio/obraz), wykrywania anomalii i anotacji danych — z silnikami „automatycznymi" (AutoML), połączeniami do danych i wstępnym oznaczaniem przez inne modele z serwisu.

Zasada nadrzędna: **nie obiecujemy nic, czego nie da się zbudować.** Każda funkcja poniżej ma przypisany realny mechanizm (komponent UI, host-function, flow engine, bundle Python w `tentaflow-containers`). UI/UX = priorytet.

---

## 0. KOREKTA (2026-06-14): PROJEKTY jako centrum + wizardy z głębią

Pierwsza wersja była zbyt „ekranami stanu końcowego" — brakowało rdzenia: **projektów** i **wizardów krok-po-kroku z realną konfiguracją** (jak dodać klasę, atrybut, co jest OCR, jak utworzyć coś od zera). Korekta:

### Projekt = jednostka pracy
Ekran główny modułu to **PROJEKTY** (lista kart): np. „Rozpoznawanie znaków na cysternach", „Trenowanie modelu Guard", „RAG dla dokumentów". Każdy projekt jest samodzielny (własne dane, schemat, anotacje, treningi, modele). Źródła danych są **opcjonalnie współdzielone** między projektami (biblioteka źródeł).

### Kreator nowego projektu (wizard)
1. Nazwa + opis. 2. **Typ** (determinuje resztę): Rozpoznawanie obrazu · Fine-tuning LLM · Fine-tuning vision/audio/obraz · ML tabelaryczne/anomalie · RAG · Destylacja. 3. Modalność/dane (źródła nowe lub z biblioteki). 4. **Schemat zadania** (zależny od typu). 5. Konfiguracja startowa.

### Schemat zadania (dla rozpoznawania) — to czego brakowało
Definicja „co wykrywam i jakie ma atrybuty" — wyklikiwana, edytowalna od zera. Przykład cystern (odtworzenie pythonowego PoC):
- `+ Dodaj klasę` → nazwa, kolor, kształt (box/poligon/punkt).
- Per klasa `+ Dodaj atrybut` → typ atrybutu: **lista wyboru** / **tekst** / **OCR** / **liczba** / **klasyfikator** (osobny model).
  - `tablica_adr`: atrybut `kod` typu OCR → + **lookup** (słownik kod→UN+nazwa).
  - `tablica_rejestracyjna`: atrybut `numer` typu OCR + walidacja formatu (PL/UA/RO).
  - `nalepka`: atrybut `klasa` (lista 2.1/3/5.1...) + atrybut `stan` (klasyfikator: czysta/brudna/uszkodzona...).
- Schemat zapisany → zasila anotację (klasy/atrybuty widoczne) i trening (głowy modelu).

### Fine-tuning odwzorowuje realny workflow Guarda (tentaflow-models)
Guard to dowód, że to się da odtworzyć. Pipeline (wspólny dla 11 zadań): `generate.sh` (generacja danych przez LLM) → `convert.py` (chat template + augmentacja zero-width + split) → `train.py --method [qlora|lora|dora|full] --gpus N` → merge+flatten → GGUF/kwantyzacje (Q2..Q8) / NVFP4 / MLX / HF → `benchmark.py` (per-class P/R/F1, porównanie wariantów + baseline Claude).

Kreator fine-tuningu (kroki = realne komendy):
1. **Model bazowy** (Qwen3.5-0.8B / Llama-Guard-86M / własny HF).
2. **Dane** — upload JSONL/CSV (`text,label`) LUB generacja (prompt + N iteracji); **augmentacja** (zero-width/homoglyph/encoding — toggle); split train/eval.
3. **Metoda** — QLoRA / LoRA / DoRA / Full (z auto-estymatą VRAM: ~8/16/18/24 GB) + hiperparametry (lr, batch, grad-accum, epoki, LoRA r/alpha).
4. **Zasoby** — liczba GPU (DeepSpeed ZeRO przy >1).
5. **Trening** — live loss/eval, checkpointy, resume.
6. **Eksport** — GGUF (Q2..Q8) / NVFP4 (vLLM) / MLX (macOS) / HF.
7. **Benchmark + Deploy** — metryki per-klasa, porównanie, deploy do `models/` + TOML.

Realność: te skrypty już istnieją w `tentaflow-models/scripts/`. UI = generyczny wrapper (wzór wspólny dla 11 zadań) uruchamiający je jako joby (bundle Python w `tentaflow-containers/ml-training/` + flow engine). Każdy parametr UI = flaga CLI (`--method`, `--gpus`, `--fraction`, `--balance`, `--resume`, quantization).

---

## 1. Decyzja architektoniczna: JEDEN moduł, wiele przestrzeni

ML + Fine-tuning + anotacja + dane dzielą te same pojęcia (zbiory, joby, modele, silniki), więc rozdzielanie na dwa moduły utrudnia. **Jeden moduł „ML Studio"** z przestrzeniami (sidebar):

| # | Przestrzeń | Co robi |
|---|---|---|
| M1 | **Przegląd** | modele w serwisie, ostatnie treningi/joby, zbiory, GPU, skróty |
| M2 | **Dane i połączenia** | upload xlsx/csv/parquet, podpięcie zewn. baz, mapowanie pól→role |
| M3 | **Studio anotacji** | oznaczanie zdjęć/tekstu/audio + WSTĘPNE oznaczanie innym modelem |
| M4 | **Trenuj (silniki)** | AutoML, anomalie, klasyczne ML — wskazujesz dane+cel, silnik robi resztę |
| M5 | **Fine-tuning** | LLM/vision/audio/obraz: LoRA/DoRA/QLoRA/pełny/od zera/DPO/destylacja |
| M6 | **Joby i treningi** | długie zadania w tle: żywe wykresy strat/metryk, logi, postęp, anuluj |
| M7 | **Model: szczegóły** | ewaluacja, porównanie, deploy, „użyj w serwisie" |

---

## 2. Fundament techniczny (co już istnieje — używamy)

- **UI**: schema-driven (`tentaflow-ui-schema`, addon zwraca `PanelTree` JSON → core renderuje `tf-*`). Gotowe komponenty: `tf-table` (paginacja), `tf-line/bar/area/pie-chart`, `tf-gauge`, `tf-file-input` + `FileUpload`, `tf-progress-bar`, `tf-canvas`/`tf-stage`, `tf-stepper/Wizard`, `tf-modal`, `tf-tabs`, `tf-toast`, `tf-command-palette`. Design system: `mockups/tentavision-v1/shared/styles.css` (indigo/violet, Manrope, dark).
- **Joby długie**: flow engine — `flow_invoke_v1(flow_id, input, wait_ms)` → `flow_status_v1(invocation_id)` (status, operators_completed/total, result_toml). To jest gotowy runner treningów.
- **Dane addonu**: `sql_*` (SQLite per-addon + migracje), `vector_*` (HNSW embeddings), `storage_*` (kv).
- **Inne modele**: `llm_generate*` (LLM sync+stream), `service_request(service_id, ...)` (QUIC proxy do dowolnego kontenera/serwisu: vLLM, vision, whisper...).
- **Wzór bundla Python/Docker**: `tentaflow-containers/<kategoria>/` z manifestem TOML (`_services/*.toml`), warianty `docker`/`python-bundle`/`native`, sidecar QUIC. `build.rs` generuje Rust+JS. To jest wzór, jak odpalać ciężki ML w Pythonie sterowany z core.

## 3. Co trzeba DOROBIĆ (uczciwie, pofazowo)

| Element | Warstwa | Priorytet |
|---|---|---|
| Kategoria `tentaflow-containers/ml-training/` (bundle Python) | Python bundle | 1 |
| Konektory danych: xlsx/csv/parquet czytane w Rust (`calamine`,`csv`,`parquet`/`arrow`), zewn. bazy (`sqlx`) | Rust core/host-fn | 1 |
| Job/run tracking dla treningów (rozszerzenie flow engine o metryki czasowe) | Rust | 1 |
| Studio anotacji — backend (zbiory anotacji, eksport COCO/JSONL) + edytowalny canvas | Rust + JS komponent | 2 |
| Rejestr modeli (wersje, porównanie, deploy) | Rust | 2 |
| Komponenty UI: `tf-annotate-canvas` (edytowalny), `tf-leaderboard`, `tf-metrics-live` | JS web components | 2 |
| Metrics collector (time-series strat/metryk do wykresów live) | Rust (SQLite/ring) | 2 |

---

## 4. Przestrzeń po przestrzeni — UI + realny backend

### M2. Dane i połączenia
- **UI**: `tf-file-input`/`FileUpload` (drag-drop xlsx/csv/parquet), `tf-table` (podgląd), kreator mapowania pól (`Wizard` + `tf-select` per kolumna → rola: feature/label/id/text/prompt/response/timestamp), `tf-combobox` do połączeń DB.
- **Backend (Rust, realne)**: parsowanie plików w core — `calamine` (xlsx), `csv`, `parquet`/`arrow`. Zewn. bazy przez `sqlx` (Postgres/MySQL/SQLite) — host-fn `data_connect_v1` + `data_query_v1` (z mapowaniem). JSON/zagnieżdżone → mapowanie ścieżek (jsonpath) na role. Mały podgląd trzymany w `sql_*` addonu.
- **Mapowanie „co jest czym"**: UI zapisuje schemat mapowania (rola→kolumna/ścieżka) do `storage_*`; bundle treningowy dostaje znormalizowany dataset (parquet/jsonl) wyprodukowany przez konektor.
- **Status**: parsery Rust = do dorobienia (proste, crate'y istnieją); UI = gotowe komponenty.

### M3. Studio anotacji (jak Label Studio, ale zintegrowane)
- **UI**: `tf-canvas`/`tf-stage` rozszerzony o edycję (rysowanie/edycja boxów, poligonów, klas, tagów tekstu, segmentów audio) — **nowy komponent `tf-annotate-canvas`**. Lista zadań `tf-table`, klasy jako `tf-chip`/`tf-tag-input`, skróty klawiszowe.
- **WSTĘPNE oznaczanie innym modelem (killer-feature)**: przycisk „Pre-oznacz modelem" → addon woła `service_request("vision-detection", ...)` albo `llm_generate(...)` (np. nasz RF-DETR/OWLv2/Qwen z serwisu) → wynik wstawiany jako edytowalne anotacje (dokładnie to, co robiliśmy w Pythonie: OWLv2/Qwen pre-label, człowiek poprawia). Wybór modelu pre-labelującego z rejestru.
- **Backend (Rust)**: zbiory anotacji w `sql_*`; eksport do COCO/JSONL/CSV (do treningu) host-fn. Obrazy przez istniejące `frame`/`/frames` + `image_resize_rgb_v1` (cropy).
- **Status**: `tf-annotate-canvas` = do dorobienia (rozszerzenie `tf-canvas`, bazujemy na overlay detekcji który już zrobiliśmy); pre-label = gotowe host-fn (llm/service).

### M4. Trenuj — silniki ML (AutoML, anomalie, klasyczne)
- **Koncepcja „silnika"** = wybieralny backend treningu (jak silniki w `tentaflow-containers`). Przestrzeń pokazuje karty silników:
  - **AutoML** (jak IBM Watson AutoAI): wskazujesz zbiór + kolumnę-cel + typ zadania (klasyfikacja/regresja/anomalie) → silnik sam próbuje wielu algorytmów/pipeline'ów, zwraca **leaderboard** (`tf-leaderboard` — nowy, albo `tf-table` posortowana) z metrykami, wybierasz najlepszy. **Backend: bundle Python** wrapujący `AutoGluon`/`FLAML` (tabular AutoML).
  - **Wykrywanie anomalii** (Twój główny wymóg): IsolationForest / `PyOD` / autoenkoder / anomalie szeregów czasowych. Wskazujesz dane → silnik uczy detektor → próg + scoring + podgląd wykrytych anomalii (`tf-line-chart` z zaznaczeniami). **Backend: bundle Python** (`PyOD`, sklearn).
  - **Klasyczne ML**: ręczny wybór algorytmu (XGBoost/RandomForest/LogReg...) + hiperparametry. **Backend: bundle Python** (sklearn/xgboost).
- **UI**: `Wizard` (dane→zadanie→silnik→konfiguracja→trenuj), `tf-line-chart` (krzywe), `tf-table`/leaderboard, `tf-progress-bar`.
- **Uruchomienie**: `flow_invoke_v1("automl_run", {dataset, target, task})` → flow engine odpala bundle Python → polling `flow_status_v1` → live metryki.
- **Status**: bundle Python = do dorobienia (wrap istniejących bibliotek, realne); UI + orkiestracja = gotowe.

### M5. Fine-tuning (LLM / vision / audio / obraz)
- **Metody**: pełny fine-tune, **LoRA / DoRA / QLoRA**, **DPO**, od zera, **destylacja** (model-nauczyciel z serwisu → mniejszy uczeń).
- **UI**: wybór modelu bazowego (z rejestru/containers), `RadioCardGroup` metody (LoRA/DoRA/QLoRA/DPO/full/distill), zbiór (z M2/M3), hiperparametry (`SliderRow`/`tf-input`: lr, epoki, rank LoRA, alpha...), podgląd configu (`CodeBlock` YAML), żywe wykresy strat (`tf-line-chart`), GPU/VRAM (`tf-gauge`).
- **„Użyj innego modelu z serwisu"**: do destylacji (nauczyciel przez `service_request`/`llm_generate`), do generowania danych syntetycznych (jak nasz Qwen-Edit), do pre-labelu. Podpowiadane kontekstowo.
- **Backend: bundle Python `ml-training`** wrapujący HF `transformers` + `peft` (LoRA/DoRA/QLoRA) + `trl` (DPO/SFT) + akceleratory (Unsloth/Axolotl opcjonalnie). Vision/audio/image-gen: analogiczne bundle (diffusers, itp.). Sterowane flow engine, postęp i metryki przez `flow_status` + metrics collector.
- **Status**: bundle Python = główna praca do dorobienia (ale to znane biblioteki, realne); UI + orkiestracja = gotowe.

### M6. Joby i treningi
- **UI**: `tf-table` jobów (status/postęp/czas), szczegół joba = `tf-line-chart` (loss/metryki live), logi (`MonoBlock`/`CodeBlock`), `tf-progress-bar`, przyciski Anuluj/Wznów.
- **Backend**: flow engine (`flow_invoke/status/cancel`) + **metrics collector** (time-series strat → wykresy). Logi streamowane przez `event_publish`/WS (jak nasz overlay).
- **Status**: flow engine gotowy; metrics collector = do dorobienia (lekki, SQLite/ring).

### M7. Model: szczegóły
- **UI**: nagłówek `tf-detail-header`, metryki (`tf-stat-card`), porównanie wersji (`tf-table`), krzywe (`tf-line-chart`), „Deploy" (do `tentaflow-containers`/serwisu), „Użyj" (test inline przez `llm_generate`/`service_request`).
- **Backend**: rejestr modeli (wersje/metryki w `sql_*`), deploy przez istniejący mechanizm containers (manifest → docker/bundle).
- **Status**: rejestr = do dorobienia; deploy = istnieje (containers).

---

## 5. Podział Rust / Python (mapa realności)

- **Rust (tentaflow-core / addon ML Studio)**: całe UI (schema), orkiestracja jobów (flow engine), konektory danych (calamine/csv/parquet/sqlx), rejestr modeli, backend anotacji + eksport, metrics collector, deploy. = „lekkie i sterujące".
- **Python (bundle w `tentaflow-containers/ml-training/`)**: faktyczny trening/AutoML/fine-tune/destylacja/anomalie — HF transformers, peft, trl, AutoGluon, FLAML, PyOD, sklearn, xgboost, diffusers. Odpalane i monitorowane przez core jak inne bundle (sidecar QUIC, manifest TOML). = „ciężkie ML".
- **Granica**: core przygotowuje znormalizowany dataset + config → uruchamia bundle Python jako job → bundle raportuje postęp/metryki/artefakt (ścieżka modelu) → core zapisuje w rejestrze i pokazuje w UI.

## 6. Fazy realizacji (po akceptacji mockupów)

1. **Faza 0 (mockupy)**: ten katalog — wszystkie ekrany M1–M7, klikalne statycznie, spójne z TentaVision/CRM.
2. **Faza 1**: kategoria `ml-training` w containers (bundle Python: SFT+LoRA na transformers/peft) + konektory danych Rust + job tracking. Pierwszy realny przepływ: dane→fine-tune LLM→model.
3. **Faza 2**: Studio anotacji (canvas + pre-label) + eksport + trening vision na tym (zamknięcie pętli „oznacz→trenuj" jak nasz PoC, ale wyklikiwany).
4. **Faza 3**: AutoML + anomalie (bundle PyOD/AutoGluon) + leaderboard + metrics collector.
5. **Faza 4**: rejestr modeli, destylacja, DPO/DoRA/QLoRA, audio/image-gen fine-tune.

## 7. Czego świadomie NIE obiecujemy (poza zakresem na teraz)

- Rozproszony trening multi-node (na teraz pojedynczy host/sidecar; multi-GPU na jednym hoście przez bundle).
- Real-time collaborative annotation (jedno-użytkownikowo na start).
- Wizualizacja architektury sieci / tensorów (miłe, nie krytyczne).
- Pełny MLOps (lineage/DVC) — minimalne wersjonowanie zbiorów/modeli, bez pełnego systemu.
</content>
