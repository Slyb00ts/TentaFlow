# Moduł „ML Studio" — kompletny plan systemu (v2)

Jedno źródło prawdy. v2 powstała po pełnym audycie v1 (trzy niezależne przeglądy: PM, agent Explore, codex). Naprawia trzy fundamentalne problemy v1:

1. **Dwa równoległe światy** — stare ekrany „przestrzeni" (`m01`–`m07`, breadcrumb `Addons > ML Studio`, sidebar `Addons (WASM)`) i nowe projektowe (`p/r/f`, breadcrumb `ML Studio > Projekty`). Wyglądały jak dwa produkty.
2. **Fałszywy router** — `p01` niezależnie od wybranego typu projektu zawsze prowadził do `r01` (rozpoznawanie). 4 z 6 typów projektu nie miały żadnego wnętrza.
3. **Magiczne wartości** — UI pokazywało gotowe kolumny, klasy („ryzyko, 3 klasy"), listy modeli, leaderboardy i liczniki jako fakt, bez ekranu który tłumaczy SKĄD się biorą i jak user tworzy je od zera. To był główny zgłoszony ból.

Zasada nadrzędna (bez zmian, ale teraz egzekwowana): **nie obiecujemy nic, czego nie da się zbudować.** Każdy element UI ma przypisany realny mechanizm. Każda wartość pokazana w UI ma ekran-źródło.

---

## 0. Decyzja architektoniczna: JEDEN model — projekt-centryczny

Koniec dwóch światów. Cały moduł jest projekt-centryczny.

- **Sidebar**: pozycja **`ML Studio`** w sekcji Integrations (NIE `Addons (WASM)` jako active). Spójnie na każdym ekranie.
- **Wejście**: zawsze `Projekty` (`p00`). Nie ma pracy „poza projektem".
- **Breadcrumb wnętrza**: `ML Studio › Projekty › [nazwa projektu] › [zakładka]`. Zawsze.
- **Zakładki wnętrza projektu** (taby) zależą od typu projektu — patrz §3.
- **Globalne (cross-project)** są TYLKO dwa, i są jawnie globalne (breadcrumb `ML Studio › Rejestr modeli` / `ML Studio › Zasoby`):
  - **Rejestr modeli** — wszystkie wytrenowane modele wszystkich projektów (był `m07`, zostaje, ale każdy model linkuje do projektu-źródła).
  - **Biblioteka zasobów** — współdzielone między projektami: źródła danych, modele usług/pre-label/OCR, słowniki lookup, dostępy zewnętrzne. To rejestr, który ELIMINUJE magiczne wartości (każda lista modeli/słowników w kreatorach pochodzi stąd).

### Mapa starych ekranów (m01–m07)

| Stary | Decyzja | Dlaczego |
|---|---|---|
| `m01` Przegląd | **USUŃ** — zastąpiony przez `p00` (Projekty) + per-projekt zakładka „Przegląd" | globalny pulpit łamie narrację projektową |
| `m02` Dane | **PRZERÓB → `d-dane` (zakładka projektu)** | mapowanie pól musi być w kontekście projektu |
| `m03` Anotacja | **USUŃ** — zastąpiony przez `r02` | inny model danych, `r02` jest projektowy |
| `m04` Trenuj/silniki | **PRZERÓB → flow `t01–t04`** (wnętrze projektu Tabular/Anomaly) | źródło bólu: wrzucał w gotowy stan bez pochodzenia danych |
| `m05` Fine-tuning | **USUŃ** — zastąpiony przez `f01–f03` + ścieżki vision/audio/destylacja | redundantny, nieprojektowy |
| `m06` Joby | **PRZERÓB → projekt-aware** (zakładka „Joby" w projekcie + opcjonalny widok globalny z kolumną „projekt") | job bez projektu jest sierotą |
| `m07` Model | **PRZERÓB → globalny Rejestr modeli** z linkiem do projektu-źródła | jeden rejestr, ale osadzony w nawigacji projektowej |

Stare pliki `m01`–`m07` zostają w katalogu **tylko jako referencja wizualna** (oznaczone w `index.html` jako „archiwum v1, NIE wzorzec"). Docelowy system ich nie zawiera.

---

## 1. Zasada „zero magii": każda wartość ma pochodzenie

Reguła projektowa: jeśli ekran pokazuje wartość (kolumnę, klasę, liczbę klas, model na liście, metrykę, licznik, rozmiar pliku), to albo (a) istnieje wcześniejszy krok/ekran w którym user ją utworzył/wybrał, albo (b) jest jawnie oznaczona jako **wykryta automatycznie** z widocznym źródłem („wykryto z kolumny `ryzyko`: 3 unikalne wartości → niskie/średnie/wysokie").

Tabela pochodzenia (egzekwowana w mockupach v2):

| Wartość w UI | Skąd pochodzi (ekran-źródło) |
|---|---|
| Lista kolumn zbioru | profil danych po imporcie (`d-dane`, krok „Podgląd + profilowanie") — czytane z nagłówków pliku/tabeli |
| „kolumna-cel: ryzyko" | user **wybiera** kolumnę na ekranie celu (`t01`) z listy wykrytych kolumn |
| „(kategoria, 3 klasy)" | **auto-wykryte** z zawartości kolumny: typ + liczba unikalnych wartości; pokazane z rozwinięciem listy klas i ich liczności |
| Lista modeli bazowych (Qwen3.5-0.8B…) | Biblioteka zasobów → modele dostępne w serwisie (`service_request` capability check) |
| Lista modeli pre-label / OCR (RF-DETR, OWLv2, trocr…) | Biblioteka zasobów → modele usług, filtrowane po zdolności (detekcja/OCR/VL) |
| Słownik lookup `un_kody` (218 wierszy) | Biblioteka zasobów → słowniki; tworzony/importowany przez usera (ekran słownika) |
| `18 240 próbek`, split 90/10 | wynik kroku „Dane" kreatora FT — po imporcie/generacji + podziale, policzony realnie |
| `~8 GB VRAM`, `~52 min` | estymator: funkcja(metoda, rozmiar modelu, batch, długość, liczba próbek) — pokazany jako **estymata** z formułą, nie fakt |
| Leaderboard AutoML | wynik realnego joba (`flow_status_v1`), z job ID, splitem, seedem, czasem, artefaktem |
| Liczniki `3 projekty / 11 modeli / 9 zbiorów` | zliczenie z rejestrów (`sql_*`): projekty, rejestr modeli, biblioteka źródeł |
| Metryki modelu `eval acc 0.91` | zapisane w rejestrze modeli przy zakończeniu joba ewaluacji |
| Rozmiary eksportu (MB) | policzone po realnej konwersji wariantu, nie hardcode (przed konwersją: „szacowany ~") |

---

## 2. Model danych projektu (wspólne pojęcia)

Każdy projekt = samodzielna jednostka (`sql_*` addonu ML Studio):

```
Projekt { id, nazwa, opis, typ, utworzony, status }
  ├─ Źródła danych      → odwołania do Biblioteki zasobów (współdzielone) + lokalne
  ├─ Konfiguracja typu  → zależna od typu (schemat klas | model+metoda | mapowanie kolumn | korpus | nauczyciel/uczeń)
  ├─ Anotacje           → (tylko typy z anotacją) zbiory ramek/etykiet, eksport COCO/JSONL
  ├─ Joby               → treningi/AutoML/ewaluacje (flow engine + metrics collector)
  └─ Modele             → wersje wyprodukowane w projekcie (→ Rejestr modeli)
```

Wspólne dla wszystkich typów: zakładki **Przegląd · Dane · Joby · Modele**. Reszta zakładek zależy od typu (§3).

---

## 3. Sześć typów projektu — kompletne ścieżki (od „Nowy projekt" do „wdrożony model")

`p01` (kreator) jest **prawdziwym routerem**: wybór typu w kroku 2 determinuje kroki 3–N kreatora ORAZ zakładki wnętrza. „Dalej" prowadzi do właściwego pierwszego ekranu typu (nie zawsze `r01`).

### 3.0 Fine-tuning: DWIE prostopadłe osie (poprawka v1)

v1 mieszał DPO i destylację w jednej liście obok LoRA/QLoRA/DoRA — to konceptualny błąd. Fine-tuning ma dwie NIEZALEŻNE osie, które się **mnożą**:

- **Oś 1 — Algorytm/cel treningu (CO optymalizujemy):** **SFT** (pary wejście→wyjście), **DPO**/ORPO/KTO (preferencje: prompt + chosen + rejected), **Destylacja/KD** (uczeń naśladuje nauczyciela).
- **Oś 2 — Parametryzacja (ILE wag ruszamy):** **Full** / **LoRA** / **QLoRA** / **DoRA**.

Kombinacje są legalne i częste: `QLoRA+DPO`, `SFT+LoRA`, `Destylacja+LoRA`. Użycie dużego modelu (nauczyciela) NIE blokuje DPO — to inna oś.

**Sprzężenie z danymi (realne):** wybór celu zmienia WYMAGANY format danych — SFT: tekst/etykieta lub instrukcja→odpowiedź; DPO: pary `chosen/rejected`; KD: odpowiedzi nauczyciela. Ekran „Dane" dostosowuje format do wybranego celu.

**UI:** ekran „Metoda" = dwa selektory (Algorytm × Parametryzacja, kombinowalne) z estymatą VRAM zależną od obu. Destylacja zostaje też **osobnym typem projektu** (typ F) — bo ma własny workflow nauczyciel→generacja — ale w środku pozwala wybrać parametryzację (LoRA/QLoRA) dla ucznia. Dotyczy ekranów: `f01` (FT LLM), `c-metoda` (FT vision/audio), `d03` (destylacja).

### A. Rozpoznawanie obrazu (vision detection)
Zakładki: Przegląd · **Dane (zdjęcia)** · **Schemat** · **Anotacja** · **Trening** · **Ewaluacja** · Modele.

Ścieżka:
1. `p01` typ → 2. **`a-dane`** import zdjęć (upload/folder/kamera TentaFlow), podgląd galerii, podział train/val/test → 3. **`r01`** schemat klas+atrybutów (istnieje, dobry) → 4. **`r02`** anotacja + pre-label modelem (istnieje, dobry) → 5. **`a-trening`** wybór modelu (RF-DETR/OWLv2-bootstrap), hiperparametry, start → 6. **`a-ewaluacja`** mAP@50, confusion matrix, per-klasa P/R, podgląd predykcji na val → 7. **`a-eksport`** eksport vision (ONNX, TensorRT, CoreML) + deploy do serwisu → karta modelu w Rejestrze.

Backend: detekcja = bundle Python vision w `tentaflow-containers/ml-training/` (RF-DETR trening), pre-label = `service_request`/`llm_generate`, eksport ONNX/TensorRT = krok bundla.

**Braki do dodania: `a-dane`, `a-trening`, `a-ewaluacja`, `a-eksport`.** (`r01`,`r02` są.)

### B. Fine-tuning LLM (wzór: Guard)
Zakładki: Przegląd · **Model bazowy** · **Dane** · **Metoda** · **Trening** · **Eksport** · Modele.

Ścieżka:
1. `p01` typ → 2. **`f00-model`** wybór modelu bazowego (z Biblioteki, capability check) → 3. **`f-dane`** dane (format zależny od celu, §3.0): SFT → tekst/label (upload/generacja); DPO → pary chosen/rejected; + augmentacja + split → 4. **`f01`** metoda = **dwie osie** (Algorytm: SFT/DPO × Parametryzacja: Full/LoRA/QLoRA/DoRA, §3.0) + hiperparametry + estymata VRAM → 5. **`f02`** trening live (istnieje) → 6. **`f03`** eksport+benchmark+deploy (istnieje, ale dodać ONNX i naprawić niespójność klas, §6).

**Braki do dodania: `f00-model`, `f-dane`** (dziś tylko streszczenie w `f01`).

### C. Fine-tuning vision / audio / obraz-gen
Zakładki: Przegląd · **Model bazowy** · **Dane (modalność)** · **Metoda** · **Trening** · **Eksport** · Modele.

Trzy pod-ścieżki (segmented w kroku model): **Whisper** (audio→tekst, dane = pary audio/transkrypt, metryka WER), **Vision backbone** (klasyfikacja/embedding, dane = obrazy+etykiety), **Diffusers** (generacja obrazu, LoRA na promptach+obrazach). Wspólny szkielet kreatora jak B, inny dataset i metryki.

Backend: bundle Python (transformers/peft dla audio/vision, diffusers dla obrazu).

**Braki: cały flow `c01-model` → `c-dane` → `c-metoda` → `c-trening` → `c-eksport`** (może współdzielić komponenty z B/A).

### D. ML tabelaryczne + anomalie ← NAPRAWA BÓLU z m04
Zakładki: Przegląd · **Dane** · **Cel i silnik** · **Trening** · **Ewaluacja** · Modele.

Ścieżka (każdy krok pokazuje POCHODZENIE):
1. `p01` typ → 2. **`t-dane`** = przerobiony `m02`: import xlsx/csv/parquet/DB → **profil danych**: tabela z wykrytymi kolumnami, typem każdej (auto), liczbą unikalnych, brakami → 3. **`t01-cel`**: user **wybiera kolumnę-cel** z listy; system pokazuje „wykryto: kategoria, 3 klasy → [niskie, średnie, wysokie] (liczności 4120/6890/1470)"; user wybiera typ zadania (auto-sugerowany) → 4. **`t02-silnik`**: AutoML / anomalie / klasyczne ML; konfiguracja (budżet, metryka, walidacja — z objaśnieniem) → 5. **`t03-leaderboard`**: realny job, leaderboard z job ID/split/seed/czas, wybór modelu → 6. **`t04-ewaluacja`**: confusion/ROC, feature importance (explainability), próg (dla anomalii), deploy.

To jest dokładnie ekran, którego brakowało — pokazuje że kolumny i „3 klasy" pochodzą z DANYCH USERA, a nie są predefiniowane.

Backend: bundle Python AutoGluon/FLAML/PyOD/sklearn/xgboost.

**Braki: cały flow `t-dane`(przeróbka m02) → `t01-cel` → `t02-silnik` → `t03-leaderboard` → `t04-ewaluacja`.** Stary `m04` rozbity na te ekrany.

### E. RAG
Zakładki: Przegląd · **Korpus** · **Chunking + Embedding** · **Indeks** · **Ewaluacja + Playground** · Deploy.

Ścieżka:
1. `p01` typ → 2. **`rag01-korpus`**: źródła dokumentów (upload PDF/MD/HTML, konektory) + parsing → 3. **`rag02-chunk`**: strategia chunkowania (rozmiar/overlap), model embeddingów (z Biblioteki), reranker (opcjonalny) → 4. **`rag03-indeks`**: budowa indeksu HNSW (`vector_*`), postęp → 5. **`rag04-eval`**: zestaw pytań testowych, metryki retrieval (recall@k, MRR), playground z cytowaniem → deploy endpoint RAG.

Backend: `vector_*` (HNSW istnieje), `embeddings-chunker` (addon istnieje), reranker = serwis. To jest najbardziej „gotowe" infrastrukturalnie.

**Braki: cały flow `rag01`–`rag04`.** (`p00` ma dziś martwy link `#`.)

### F. Destylacja
Zakładki: Przegląd · **Nauczyciel + uczeń** · **Zbiór promptów** · **Generacja + trening** · **Porównanie** · Modele.

Ścieżka:
1. `p01` typ → 2. **`d01-para`**: wybór nauczyciela (większy model z serwisu) + ucznia (mniejszy bazowy) → 3. **`d02-prompty`**: zbiór promptów (upload/generacja) → 4. **`d03-generacja`**: nauczyciel generuje odpowiedzi (`service_request`/`llm_generate`), potem trening ucznia (SFT/KD) → 5. **`d04-porownanie`**: teacher vs student (jakość, tok/s, rozmiar), eksport.

Backend: `llm_generate`/`service_request` (nauczyciel) + bundle FT (uczeń).

**Braki: cały flow `d01`–`d04`.**

---

## 4. Rejestry pomocnicze (eliminują magię) — Biblioteka zasobów

Globalna zakładka `ML Studio › Zasoby`, dostępna też kontekstowo w kreatorach (każdy `<select>` modelu/słownika czyta stąd):

- **Modele usług** — modele dostępne przez `service_request`/`llm_generate`, z **capability matrix** (detekcja / OCR / VL / embedding / generacja / klasyfikacja). Kreatory filtrują po zdolności (np. pole „Model OCR" pokazuje tylko OCR-capable). Źródło list w `r01`/`r02`/`f00`.
- **Słowniki lookup** — tabele kod→wartości (np. `un_kody`). Ekran tworzenia/importu/edycji. Źródło lookup w `r01`.
- **Źródła danych** — współdzielone zbiory (xlsx/parquet/DB/jsonl) z profilem. Źródło danych w `t-dane`/`f-dane`/`a-dane`.
- **Dostępy zewnętrzne** — re-używa istniejący `Settings → Dostępy zewnętrzne` (vault `is_secret`). Źródło credentiali dla DB i zewnętrznych endpointów/baseline Claude.

---

## 5. Eksport — matryca bramkowana możliwościami (capability-gated)

Naprawia `f03`. Eksport NIE jest globalną siatką. Dostępność formatu zależy od (typ modelu, runtime, OS, hardware):

| Format | Dotyczy | Kwantyzacje | Warunek |
|---|---|---|---|
| GGUF | LLM (llama.cpp) | Q2_K, Q3_K_M, Q4_K_S, Q4_K_M, Q5_K_M, Q6_K, Q8_0, F16 | architektura wspierana przez llama.cpp |
| **ONNX** (DODAĆ) | LLM, vision, audio | FP32, FP16, INT8 (dynamic/static), opset wybór | eksporter dla architektury |
| NVFP4 | LLM (vLLM) | FP4 | GPU Blackwell+ / vLLM |
| MLX | LLM, vision (macOS) | 4-bit, 8-bit, FP16 | macOS + wspierana architektura |
| TensorRT | vision, LLM | FP16, INT8 | NVIDIA + kalibracja |
| CoreML | vision (iOS/macOS) | FP16, INT8 (palettize) | Apple |
| HF safetensors | wszystko | FP16, BF16, FP32 | zawsze |

UI: zakładka formatu → siatka kwantyzacji **właściwa dla tego formatu** (nie jedna globalna). Formaty niedostępne dla danego modelu/HW: wyszarzone z tooltipem „dlaczego". **Multi-select**: można zaznaczyć wiele wariantów (format×kwant) i wyeksportować paczkę. Rozmiary MB: „szacowany ~X" przed konwersją, realny po. Benchmark: baseline z jawnym źródłem (endpoint, model, wersja, koszt, zgoda na wysłanie danych).

---

## 6. Spójność i poprawki istniejących ekranów

- **`f03`**: dodać ONNX (§5); naprawić niespójność — nagłówek mówi „1820 promptów, 4 klasy" ale tabela ma 3 kolumny klas (`jailbreak/pii/safe`). Ujednolicić liczbę klas ze schematem ewaluacji.
- **`p01`**: router — „Dalej" zależny od typu; panel „Co dalej" generowany z typu (już jest, ale link zaślepiony).
- **`p00`**: martwy link RAG `#` → `rag01-korpus`. Liczniki z rejestrów.
- **Nawigacja globalna**: sidebar `ML Studio` active wszędzie; breadcrumb `ML Studio › Projekty › […]`. Usunąć `Addons (WASM) active` ze wszystkich ekranów ML Studio.

---

## 7. Fundament techniczny — co istnieje vs co dorobić (uczciwa ocena trudności)

### Istnieje (używamy wprost)
- UI schema-driven (`tentaflow-ui-schema`, PanelTree→`tf-*`). Komponenty: `tf-table`, `tf-*-chart`, `tf-gauge`, `tf-file-input`+`FileUpload`, `tf-progress-bar`, `tf-stepper/Wizard`, `tf-modal`, `tf-tabs`, `tf-toast`, `tf-select`, `tf-combobox`.
- Flow engine (`flow_invoke_v1`/`flow_status_v1`) — runner jobów.
- `sql_*`, `vector_*` (HNSW), `storage_*`.
- `llm_generate*`, `service_request` (QUIC proxy do serwisów).
- Wzór bundla Python/Docker (`tentaflow-containers/<kat>/`, manifest TOML, sidecar QUIC).
- `embeddings-chunker` (addon) — fundament RAG.
- `Settings → Dostępy zewnętrzne` (vault).

### Do dorobienia — z realnym mechanizmem i UCZCIWĄ oceną trudności

| Element | Mechanizm | Trudność | Uwaga |
|---|---|---|---|
| Kategoria `ml-training` (bundle Python: SFT/LoRA/QLoRA/DoRA, AutoGluon, PyOD, diffusers) | bundle w `tentaflow-containers` | Średnia | znane biblioteki, ale dużo wariantów |
| Konektory danych (calamine/csv/parquet/arrow + sqlx) | host-fn Rust | **Wysoka** | NIE „proste UI": walidacja schematu/typów, limity pliku, vault credentiali, sandboxing zapytań, ochrona SSRF/SQL-abuse |
| **Metrics collector** (time-series strat/metryk → krzywe live, tok/s, VRAM, checkpointy, log stream) | Rust (SQLite/ring) + `event_publish`/WS | **Wysoka** | `flow_status_v1` sam NIE wystarczy; to osobny podsystem (jak nasz overlay detekcji) |
| **`tf-annotate-canvas`** (edytowalne boxy/poligony/punkty, skróty) | nowy komponent JS (rozszerzenie `tf-canvas`/overlay detekcji) | **Wysoka** | mockupy używają raw `<svg>`/`<button>` — docelowo MUSI być `tf-*` |
| Rejestr modeli (wersje, metryki, porównanie, deploy, rollback) | Rust + `sql_*` | Średnia | deploy re-używa containers |
| Backend anotacji (zbiory, eksport COCO/JSONL) | Rust + `sql_*` | Średnia | cropy przez `image_resize_rgb_v1` (istnieje) |
| Eksport multi-format (ONNX/TensorRT/CoreML/GGUF/MLX/NVFP4) | krok bundla per format | **Wysoka** | każdy format = osobny eksporter, opsety/kalibracja/HW-gating |
| `tf-leaderboard`, `tf-metrics-live` | nowe komponenty JS | Niska | nad `tf-table`/`tf-*-chart` |
| Capability matrix modeli usług | Rust + `sql_*` | Średnia | zasila filtrowanie list w kreatorach |

### Zgodność `tf-*` (MANDATORY)
Mockupy używają surowych `<button>/<input>/<select>` i własnego canvasu — to **dozwolone tylko w mockupach**. Docelowa implementacja zwraca PanelTree i renderuje `tf-*`. Nowe komponenty do dodania: `tf-annotate-canvas`, `tf-leaderboard`, `tf-metrics-live`, `tf-quant-matrix` (wybór format×kwant). Brakującą funkcję istniejącego komponentu — rozszerzyć, nie forkować.

---

## 8. Inwentarz ekranów (mockupy v2)

### Zostają (dobre, ewent. drobne poprawki nawigacji)
`p00`, `p01` (poprawić router), `r01`, `r02`, `f01`, `f02`, `f03` (dodać ONNX + fix klas).

### DODAĆ (priorytet P0 — domknięcie podstawowych ścieżek)
- `t-dane` (profil danych) · `t01-cel` · `t02-silnik` · `t03-leaderboard` · `t04-ewaluacja` — **typ Tabular/Anomalie** (naprawa bólu m04).
- `a-dane` · `a-trening` · `a-ewaluacja` · `a-eksport` — domknięcie **Rozpoznawania**.
- `f00-model` · `f-dane` — realne kroki 1–2 **FT LLM**.

### DODAĆ (P1)
- `rag01`–`rag04` — **RAG**.
- `d01`–`d04` — **Destylacja**.
- `c01-model` · `c-dane` · `c-metoda` · `c-trening` · `c-eksport` — **FT vision/audio/obraz**.
- `zasoby` (Biblioteka: modele usług + capability matrix, słowniki, źródła, dostępy).
- `przeglad-projektu` (zakładka Przegląd wewnątrz projektu — zastępuje rolę `m01`).

### PRZEROBIĆ
- `m06` → projekt-aware joby. `m07` → globalny Rejestr modeli z linkiem do projektu.

### USUNĄĆ z docelowego systemu (zostają jako archiwum referencyjne w `index.html`)
`m01`, `m03`, `m05` (zastąpione). `m04` rozbity na `t01–t04`.

---

## 9. Fazy realizacji (po akceptacji mockupów v2)

0. **Mockupy v2** (ten katalog): domknąć wszystkie ścieżki (§8), spójna nawigacja, zero magii.
1. **Faza 1**: `ml-training` (bundle Python SFT+LoRA) + konektory danych Rust + metrics collector + job tracking. Pierwszy realny przepływ: FT LLM (B) end-to-end.
2. **Faza 2**: Tabular/Anomalie (D) — AutoGluon/PyOD bundle + profil danych + leaderboard. Domyka ból m04 realnie.
3. **Faza 3**: Studio anotacji (`tf-annotate-canvas` + pre-label + eksport) + trening vision (A) — pętla „oznacz→trenuj".
4. **Faza 4**: RAG (E) — najwięcej infrastruktury gotowej.
5. **Faza 5**: Rejestr modeli + Destylacja (F) + FT vision/audio/obraz (C) + multi-format eksport.

---

## 10. Świadomie POZA zakresem (na teraz)

- Real-time collaborative annotation (jedno-użytkownikowo na start).
- Wizualizacja architektury sieci/tensorów.
- Pełny MLOps (lineage/DVC) — minimalne wersjonowanie zbiorów/modeli.

(Rozproszony trening multi-node oraz sync bazy między nodami — PRZENIESIONE DO ZAKRESU, patrz §11.)

---

## 11. WYMAGANIA v2 (2026-06-14) — uprawnienia, miejsce w Aplikacjach, zasoby mesh, sync + trening rozproszony

Cztery twarde wymagania, które rozszerzają architekturę i wymuszają rework S0/S1.

### 11.1 Uprawnienia per-projekt + zaproszenia
- Projekt ma **właściciela** (twórcę). Domyślnie **każdy widzi TYLKO swoje projekty** (gdzie jest owner lub zaproszonym członkiem). NIE org-shared (to unieważnia model z plastra S0, gdzie `list_projects` filtrował po `org_id`).
- Właściciel może **zaprosić** innych użytkowników do projektu (rola: owner / editor / viewer). Zaproszeni widzą projekt i działają wg roli.
- **Backend:** tabela `project_members(project_id, user_id, role, invited_by, created_at)` (owner wpisany przy tworzeniu). `list_projects(user_id)` = `WHERE owner_user_id = ? OR EXISTS member`. Handlery: `project_members_list`, `project_invite`, `project_member_remove`, `project_role_set`. Autoryzacja akcji wg roli (np. tylko owner zaprasza/usuwa).
- **Mockupy:** `p00` → sekcje „Moje projekty" + „Udostępnione mi" (+ badge właściciela/roli); per-karta akcja „Udostępnij". NOWY ekran `p02-udostepnianie` (członkowie + zaproszenie po użytkowniku/e-mailu + role). `przeglad-projektu` → zakładka/sekcja „Członkowie".

### 11.2 Miejsce w „Aplikacjach", widoczność Power User
- ML Studio (mimo że moduł rdzenia) ma być w sekcji **Aplikacje** dashboardu, jak `chat.js` — NIE w Integrations/Addons, NIE w Admin-nav. (Rework rejestracji nawigacji z plastra S1.)
- Widoczny **tylko dla Power Userów i Adminów** (rola/uprawnienie Power User istnieje w core). Gate po roli przy renderowaniu pozycji + na handlerach (`#[policy(...)]` Power User/Admin).
- **Mockupy:** sidebar pokazuje ML Studio pod nagłówkiem „Aplikacje"; nota o widoczności Power User. (Wzór: jak `chat` jest w Aplikacjach.)

### 11.3 Alokacja zasobów przez Admina — per osoba / grupa / projekt, mesh-wide
- **Domyślnie NIKT nie ma zasobów** (GPU/CPU/RAM). Można założyć projekt, ale trening wymaga przydzielonych zasobów.
- Admin przydziela zasoby z **puli WSZYSTKICH nodów mesh**: będąc na node A można przydzielić GPU z node B → subjectowi (user / grupa / projekt). Projekt pokazuje „jakie zasoby przydzielono i z których nodów".
- **Backend:** rejestr zasobów per-node (z mesh: `node_resources_get` + mesh registry → karty GPU/VRAM/CPU per node). Tabela grantów `resource_grants(grant_id, subject_kind[user|group|project], subject_id, node_id, resource_kind[gpu|cpu|ram], resource_ref, quota, granted_by, created_at)`. Rozstrzyganie efektywnych zasobów usera/projektu = suma grantów (user + jego grupy + projekt). Scheduler treningu wybiera node wg dostępnych grantów.
- **Mockupy:** NOWY ekran admina `admin-zasoby` (pula nodów mesh z kartami GPU; przydział do user/grupa/projekt; domyślnie pusto). `przeglad-projektu` → sekcja „Zasoby przydzielone" (które GPU/nody). Kreatory treningu (f02/a-trening/c-trening/t02/d03) → wybór z PRZYDZIELONYCH zasobów (jeśli brak → komunikat „brak przydzielonych zasobów, poproś admina").

### 11.4 Sync bazy + trening rozproszony po mesh
- **`ml_studio.db` synchronizuje się między nodami** (Sync Ledger / Fjall, `sync/core_registry.rs`) — projekty/schematy/modele/zadania widoczne na każdym node usera. Nawet telefon tworzy projekty/uruchamia uczenie; job idzie na node z zasobami. Tabele ML Studio rejestrowane w sync runtime (z Permission/Sync Policy gate — synchronizują się tylko do nodów uprawnionego usera).
- **Trening rozproszony:** dane przenoszone po mesh na node z zasobami; możliwość podziału (część pipeline'u na jednym node, część na innym). Scheduler mesh-aware (wybór node wg zasobów §11.3). Na laptopie/telefonie część treningów lokalnie.
- **Backend:** rejestracja tabel ml_studio w `sync/core_registry.rs` (z politykami `replicated_by_permission`); job dispatcher świadomy mesh (mapuje run → node z grantem; przerzut datasetu/artefaktu przez mesh stream, jak frame pickup). To duże, fazowane (patrz nowa Faza 6+).
- **Mockupy:** ekrany treningu pokazują „node wykonawczy" / „rozproszony na N nodów" + wskaźnik sync; `przeglad-projektu` wskaźnik „zsynchronizowano z mesh".

### 11.5 Wpływ na plan
- **Rework S0/S1:** (a) `list_projects` per-user + `project_members` + zaproszenia; (b) nawigacja → Aplikacje + gate Power User. To pierwsze do zrobienia po mockupach.
- **Nowe fazy:** Faza 6 — zasoby mesh (rejestr + granty + UI admina + widok w projekcie). Faza 7 — sync `ml_studio.db` + trening rozproszony mesh-aware.
- **Nowe mockupy:** `p02-udostepnianie`, `admin-zasoby`; przeróbki: `p00` (moje/udostępnione), `przeglad-projektu` (członkowie + zasoby + sync), ekrany treningu (node/rozproszenie), nawigacja (Aplikacje/Power User), `index.html`.
