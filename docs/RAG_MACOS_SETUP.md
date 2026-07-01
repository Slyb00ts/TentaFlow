# Konfiguracja RAG na macOS

Przewodnik uruchomienia addona `rag` (kolekcje, ingest dokumentów, wyszukiwanie
semantyczne) na Apple Silicon. Zakłada gotowe repo i zbudowaną binarkę `tentaflow`.

## 1. Dlaczego RAG wymaga konfiguracji na macOS

Flow RAG jest **jeden i wspólny dla wszystkich platform** (linux, macos, windows,
ios, android). Pliki flow (`ingest.flow.json`, `query.flow.json`,
`retrieval_round.flow.json`) nie odwołują się do konkretnych modeli — wołają
**aliasy**:

| Alias | Rola | Wymagany |
|-------|------|----------|
| `rag-embeddings` | embeddingi chunków (ingest + retrieval) | tak |
| `rag-parse` | parsowanie stron PDF/skanów na markdown | tak |
| `rag-reranker` | re-ranking kandydatów w retrievalu | nie |
| `rag-llm` | generowanie odpowiedzi z cytatami | nie |

Różnice per-OS załatwia **podpięcie aliasu do modelu**, a nie osobny flow.

Od tej zmiany wszystkie aliasy RAG startują **odpięte** — w `manifest.toml`
`suggested_default = ""` dla każdego z nich. Świeża instalacja tworzy je jako
nieaktywne (`is_active=0`), więc admin **musi podpiąć modele ręcznie**.

Domyślne modele z rodziny nemotron (`nemotron-embed`, `nemotron-rerank`,
`nemotron-parse`) są zbudowane pod **CUDA/NVIDIA** i **nie działają na Apple** —
brak CUDA. Na macOS aliasy podpina się do modeli **MLX** (Apple Silicon) lub
przenośnych **GGUF** (llama.cpp).

## 2. Wymagania wstępne

Uruchom setup — wykryje macOS i zainstaluje zależności:

```bash
./scripts/setup.sh
```

Kluczowe elementy dla RAG na macOS:

- **Xcode + Metal Toolchain** — na macOS 26+ kompilator Metal jest osobnym
  komponentem. Bez niego `xcodebuild` buduje zepsuty `mlx.metallib` i **każdy model
  MLX zwraca bełkot** (złe logity GPU, bez błędu buildu). `setup.sh` pobiera go
  automatycznie, ręcznie:

  ```bash
  xcodebuild -downloadComponent MetalToolchain
  ```

- **zvec (`macos-arm64`)** — wbudowana baza wektorowa. Feature `vector` jest
  obowiązkowy, bez artefaktu `libzvec_c_api.dylib` binarka się nie zbuduje.
  `setup.sh` buduje go do `tentaflow-zvec-sys/vendor/lib/macos-arm64/`.

## 3. Wdrożenie serwisów modeli

`Settings → Services`. Wdróż po jednym serwisie w każdej potrzebnej kategorii
(zakładka listy, przycisk „Dodaj serwis"). Wszystkie poniższe warianty działają
natywnie na Apple Silicon.

### Embeddings (dla `rag-embeddings`)

- `jina-embed-mlx` — Jina Embeddings v5 (Qwen3-0.6B, 1024d) przez MLX, embedded na
  macOS/iOS. Preset zalecany: `jina-v5-text-small-retrieval-mlx`.
- `jina-embed-gguf` — przenośna alternatywa przez llama.cpp (GGUF, 1024d), embedded
  na wszystkich platformach. Preset zalecany: `jina-v5-small-retrieval-q5-k-m`.

> Wymiar przestrzeni `passages` w `manifest.toml` to **1024** — dobierz model
> embeddingów o tym samym wymiarze (oba warianty Jina v5 = 1024d).

### Vision / parsowanie dokumentów (dla `rag-parse`)

- `paddle-ocr-mlx` — PaddleOCR-VL przez MLX, embedded na macOS/iOS. Zwraca tekst z
  układem strony. Preset: `paddleocr-vl-mlx`.
- `apple-ocr` — natywny OCR przez Vision (`VNRecognizeTextRequest`), bez modelu na
  dysku i bez sieci. Preset: `apple-vision-ocr`.

### Reranker (dla `rag-reranker`, opcjonalny)

- `jina-rerank-mlx` — jina-reranker-v3 przez MLX (python-bundle, `/v1/rerank`).
  Tylko macOS (nie iOS). Licencja NC.
- `qwen3-rerank-mlx` — Qwen3 Reranker 0.6B przez mlx-lm (python-bundle,
  `/v1/rerank`). Tylko macOS. Apache-2.0.

### LLM (dla `rag-llm`, opcjonalny)

Dowolny lokalny LLM MLX wdrożony jako serwis kategorii LLM.

## 4. Podpięcie aliasów RAG

`Settings → Services → zakładka „Aliasy & Routing"`. Dla każdego aliasu RAG otwórz
edycję i w polu **Primary target** wskaż wdrożony serwis:

| Alias | Podepnij do |
|-------|-------------|
| `rag-embeddings` | `jina-embed-mlx` (lub `jina-embed-gguf`) |
| `rag-parse` | `paddle-ocr-mlx` (lub `apple-ocr`) |
| `rag-reranker` | `jina-rerank-mlx` (lub `qwen3-rerank-mlx`) |
| `rag-llm` | wdrożony model MLX LLM |

Zapisz („Zapisz"). Podpięcie primary target ustawia alias jako aktywny
(`is_active=1`).

> **Dopóki alias jest odpięty (`is_active=0`), flow się nie rozwiąże.** Dispatcher
> nie znajdzie modelu dla `rag-embeddings` / `rag-parse` (oba `required=true`) i
> ingest oraz zapytanie zakończą się błędem. Alias bez podpiętego modelu jest w
> UI oznaczony jako „Nieaktywny".

## 5. Odpinanie i zmiana modelu

W panelu edycji aliasu (zakładka „Aliasy & Routing"):

- **Zmiana modelu** — wybierz inny serwis w polu Primary target i zapisz.
- **Odepnij model** — przycisk „Odepnij model" (ikona `ban`) przy slocie primary
  czyści podpięcie. Po zapisie primary target jest pusty, a alias staje się
  nieaktywny (`is_active=0`).
- **Modele fallback** — dodawaj i usuwaj w tym samym panelu (lista pod primary,
  z zmianą kolejności przeciąganiem). Fallback zostaje użyty według strategii,
  gdy primary jest niedostępny. Odpięcie primary czyści też fallbacki.

## 6. Użycie

Po podpięciu aliasów addon RAG jest gotowy. Narzędzia (z panelu RAG lub przez
protokół):

1. **Utwórz kolekcję** — `create_collection` (parametr `name`).
2. **Ingestuj dokument** — `ingest_document` (`collection_id`, `doc_id_blob` z
   document store, `filename`, `mime` — np. `application/pdf`, `image/png`,
   `text/markdown`). Pipeline: parse → chunking → embedding → upsert wektorów.
   Status śledzisz przez `ingest_status` (`job_id`).
3. **Zapytaj** — `ask` (`collection_id`, `question`, opcjonalnie `top_k`,
   domyślnie 10). Zwraca odpowiedź z cytatami (multi-hop: retrieval → rerank →
   LLM).

## 7. Troubleshooting

- **Modele MLX zwracają bełkot** — brak Metal Toolchain (macOS 26+). Zainstaluj
  `xcodebuild -downloadComponent MetalToolchain` i usuń stary metallib
  (`rm -rf tentaflow-desktop/macos/swift/MLXBridge/build-xcode`), przebuduj.
- **Alias „Nieaktywny" / flow się nie rozwiązuje** — alias nie ma podpiętego
  modelu (`is_active=0`). Podepnij serwis w „Aliasy & Routing" (pkt 4). Sprawdź
  szczególnie `rag-embeddings` i `rag-parse` (oba wymagane).
- **Błąd „brak CUDA" / model nie startuje** — podpięto model nemotron/NVIDIA.
  Na macOS używaj wariantów MLX lub GGUF (pkt 3).
- **Deep-research / SearXNG** — na macOS SearXNG działa jako `python-bundle`
  (nie jako embedded silnik Rust). iOS nie uruchamia SearXNG natywnie
  (to aplikacja webowa w Pythonie).
