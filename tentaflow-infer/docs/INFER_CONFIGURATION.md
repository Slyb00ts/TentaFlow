# FORGE — Konfiguracja (INFER_CONFIGURATION.md)

Kompletny opis wszystkich parametrów silnika: flagi CLI, parametry API,
zmienne środowiskowe. **Reguła utrzymania: każda zmiana/nowy parametr MUSI być
dopisany do tego pliku w tym samym commicie, który go wprowadza.** Źródłem
prawdy jest `forge <cmd> --help` — ten plik dodaje kontekst, zakresy i zalecenia.

Ostatnia aktualizacja: 2026-07-18.

---

## Komendy

```
forge serve       # serwer HTTP z OpenAI-compatible API
forge run         # jednorazowa generacja do stdout (streaming)
forge bench       # pomiar prefill/decode tok/s
forge embed       # wektor embeddingu dla tekstu
forge transcribe  # transkrypcja WAV (Whisper)
```

Wspólny wzorzec: pierwszy argument pozycyjny to ścieżka modelu — plik `.gguf`
LUB katalog snapshotu HF (safetensors + config.json + tokenizer.json).

---

## Model i pamięć (serve / run / bench)

| Flaga | Domyślnie | Opis |
|---|---|---|
| `--ctx <N>` | `0` | Maksymalna długość kontekstu w tokenach. `0` = maksimum modelu (`max_position_embeddings`). Dowolna wartość jest honorowana (256, 200000, …) do limitu modelu; KV cache jest sizowany pod tę wartość — o tym, czy się zmieści, decyduje VRAM. |
| `--weights-pool-gb <F>` | serve: `16`; run/bench/embed: `0` | Rozmiar puli VRAM na wagi w GiB. `0` = automatyczny podział wolnego VRAM (KV dostaje swój wyliczony budżet najpierw, reszta minus margines idzie na wagi). W serve wartość jest przycinana, żeby wagi+KV+aktywacje zawsze mieściły się w wolnym VRAM. |
| `--kv-pages <N>` (serve) | `512` | Liczba stron KV cache (32 tokeny/strona) współdzielonych przez wszystkie sekwencje. Automatycznie podnoszona do co najmniej jednego pełnego okna `--ctx`. Budżet ponad okno = współbieżne sekwencje. |

Wewnętrzne (ModelConfig, niewystawione jako flagi): `kv_page_size=32` tokeny/strona.

---

## KV cache — drabinka kwantyzacji (serve / run / bench)

| Flaga | Domyślnie | Opis |
|---|---|---|
| `--kv-cache <MODE>` | `f16` | Tryb przechowywania KV: `f16` \| `fp8` \| `rot4` \| `rot3`. |
| `--kv-residual-window <N>` | `128` | Tryby rot: liczba najświeższych tokenów trzymanych w f16 (okno rezydualne). |
| `--kv-activate-at <N>` | `4096` | Tryby rot: długość kontekstu, od której sekwencja przechodzi na magazyn rotacyjny (poniżej — czysty f16; narzut rotacji przegrywa na krótkim kontekście). |

Charakterystyka trybów (Bielik-7B, RTX 4090, zmierzone):

| Tryb | B/element KV | Max ctx na 24 GB | Decode @4k | Jakość |
|---|---:|---:|---:|---|
| `f16` | 2.0 | ~118k | 130 tok/s | referencja (bit-exact) |
| `fp8` | 1.0 | ~236k | 119 tok/s | identyczne tokeny greedy; drift logitów raportowany |
| `rot4` | ~0.52 | ~458k | 87 tok/s | **zalecany low-bit**; needle-recall OK, cosine 0.984 |
| `rot3` | ~0.39 | ~604k | 87 tok/s | dostępny-ale-stratniejszy (cosine 0.937) |

Zasada wyboru: `f16` domyślnie; `fp8` gdy potrzeba 2× kontekstu/sekwencji;
`rot4` gdy liczy się bardzo długi kontekst (≈4× f16) za ~1.5× kosztu decode.
Ograniczenie: tryby rot są obecnie single-stream (batched decode z rot zwraca
jawny błąd Unsupported — tracked follow-up).

---

## Batching / scheduler (serve)

| Flaga | Domyślnie | Opis |
|---|---|---|
| `--max-active <N>` | `8` | Maksimum jednocześnie dekodujących sekwencji (górny rozmiar batcha; ≥1). Kwoty KV i admission control liczą się względem tej wartości. |
| `--batch-min <N>` | `12` | Próg włączenia batched forward: poniżej N jednocześnie dekodujących sekwencji działa strojona ścieżka pojedynczej sekwencji (szybsza przy małej współbieżności — crossover z GEMM-ów tensor-core zmierzony ~12). |
| `--prefill-chunk <N>` | `16` | Ile tokenów promptu jedna sekwencja może prefillować w jednej iteracji schedulera (chroni ITL pozostałych sekwencji). Wewnętrzny sufit chunka: 1024. |

Admission control: żądanie, którego prompt+max_tokens nie mieści się w stronach
KV, dostaje natychmiast 429 (przejściowy brak) albo 400 `context_length_exceeded`
(trwałe przekroczenie `--ctx`), nigdy OOM w połowie generacji.

---

## Serwer HTTP (serve)

| Flaga | Domyślnie | Opis |
|---|---|---|
| `--bind <ADDR>` | `0.0.0.0:8080` | Adres nasłuchu. |
| `--model-id <ID>` | nazwa pliku/katalogu | Id modelu zwracany w `/v1/models` i wymagany w żądaniach. |
| `--api-key <KEY>` | brak | Gdy ustawione: wymagane `Authorization: Bearer <key>` na `/v1/*` (porównanie constant-time; `/healthz` bez auth). |
| `--tool-call-parser <P>` | auto | Parser tool-calli z wyjścia modelu: `hermes` \| `llama3` \| `none`. Auto-detekcja: szablon zawierający `<tool_call>` → hermes; llama-arch z tool-aware szablonem → llama3; szablon bez `tools` → none. |
| `--whisper-model <DIR>` | brak | Katalog HF Whispera — włącza `POST /v1/audio/transcriptions` (osobny device/mutex; 4 równoległe uploady, nadmiar → 429). |
| `--embed-model <PATH>` | brak | Model embeddingowy — włącza `POST /v1/embeddings`. Gdy pominięte, a serwowany model sam jest embeddingowy, jest reużywany. |

Endpointy: `/v1/chat/completions`, `/v1/completions`, `/v1/models`,
`/v1/embeddings`, `/v1/audio/transcriptions`, `/healthz`.

---

## Parametry żądań API (OpenAI-compatible, per request)

`POST /v1/chat/completions` / `/v1/completions`:

| Pole | Domyślnie | Uwagi |
|---|---|---|
| `temperature` | 0.7 | `0` = greedy (argmax, deterministyczny). |
| `top_k` | 40 | Sampling na GPU dla 1..64; większe → ścieżka CPU. |
| `top_p` | 0.95 | Nucleus. |
| `min_p` | 0.0 | Próg względem najlepszego kandydata. |
| `repetition_penalty` | 1.0 | Karane są UNIKALNE tokeny (bez składania wykładniczego). |
| `seed` | czas | Deterministyczny strumień per (seed, krok). |
| `max_tokens` / `max_completion_tokens` | — | Walidowane względem `--ctx`: prompt+max_tokens ≤ ctx, inaczej 400 `context_length_exceeded`. |
| `stop` | — | String lub tablica; holdback bez wycieków częściowych dopasowań. |
| `stream` | false | SSE; `stream_options.include_usage` → osobny finalny chunk z usage. |
| `tools` / `tool_choice` | — | `tools` musi być tablicą; `tool_choice`: `auto`/`none` (`required`/named → 400 not_implemented). Odpowiedź: `tool_calls[]`, `finish_reason:"tool_calls"`. |
| `n` | 1 | `n>1` → 400 (na razie). |

Myślenie (`<think>…</think>`, np. Qwen3) jest ekstrahowane do
`reasoning_content` (nie liczy się jako content).

`POST /v1/embeddings`: `input` (string/tablica/tokeny), `encoding_format`
(`float`|`base64`), `dimensions` (trunkacja Matryoshka + renormalizacja).

`POST /v1/audio/transcriptions`: multipart `file` (WAV, auto-resample do 16 kHz
mono), `language` (np. `pl`; ignorowane dla modeli `.en`).

---

## forge run

| Flaga | Domyślnie | Opis |
|---|---|---|
| `-n, --max-tokens <N>` | `256` | Limit generacji. |
| `--temp <F>` | `0.7` | Temperatura (`0` = greedy). |
| `--chat` | off | Opakowuje prompt w szablon czatu modelu (user message, add_generation_prompt). |
| + `--ctx`, `--weights-pool-gb`, `--kv-cache`, `--kv-residual-window`, `--kv-activate-at` | jw. | Jak w sekcjach wyżej. |

## forge bench

| Flaga | Domyślnie | Opis |
|---|---|---|
| `--tokens <N>` | `128` | Tokeny decode do zmierzenia. |
| `--prompt-tokens <N>` | `512` | Długość promptu (prefill). |
| + `--kv-cache`, `--kv-residual-window`, `--kv-activate-at` | jw. | |

Uwaga metodyczna: prefill mierzony do pierwszego WIDOCZNEGO tokenu (zawiera
≥1 krok decode); bezczynna 4090 siedzi na ~210 MHz — porównuj rozgrzane przebiegi.

## forge embed

| Flaga | Domyślnie | Opis |
|---|---|---|
| `--pooling <P>` | metadane modelu | `mean` \| `cls` \| `last`. |
| `--dimensions <N>` | pełny wymiar | Trunkacja Matryoshka + renormalizacja L2. |
| `--weights-pool-gb <F>` | `0` (auto) | |

## forge transcribe

| Flaga | Domyślnie | Opis |
|---|---|---|
| `--language <L>` | `en` | Kod języka (`pl`, `de`, …); modele `.en` ignorują z ostrzeżeniem. |

---

## Zmienne środowiskowe

| Zmienna | Opis |
|---|---|
| `FORGE_KERNEL_DIR` | Ścieżka do `kernels/mojo/build` — nadpisuje wbudowane artefakty PTX (iteracja nad kernelami bez rebuildu Rusta). |
| `FORGE_BATCH_MIN` | Nadpisuje próg `--batch-min` (diagnostyka). |
| `FORGE_PREFILL_TRACE=1` | Wypisuje czasy faz prefill_chunk (profilowanie). |
| `RUST_LOG` | Standardowy filtr tracing (`info` domyślnie, na stderr). |

---

## Formaty modeli i kwantyzacje (co silnik przyjmuje)

- **GGUF v2/v3** — wszystkie kwantyzacje GGML natywnie w VRAM (bez
  materializacji do f16): F16/BF16, Q4_0/Q4_1/Q5_0/Q5_1/Q8_0,
  Q2_K…Q6_K/Q8_K, IQ1_S/M, IQ2_XXS/XS/S, IQ3_XXS/S, IQ4_NL/XS, MXFP4.
  Tokenizer i szablon czatu czytane z metadanych GGUF.
- **safetensors + HF config** — BF16/F16/F32; compressed-tensors
  `nvfp4-pack-quantized` (NVFP4, programowy FP4) i `float-quantized`
  (FP8 → f16 przy ładowaniu); sharding przez `model.safetensors.index.json`.
- Architektury: qwen3, llama, mistral (rejestr deklaratywny w forge-formats);
  Whisper (osobny silnik); modele embeddingowe (pooling z metadanych).
- head_dim: 64 i 128 (specjalizacje attention); inne → jasny błąd.

## Ograniczenia znane (uczciwie)

- Tryby rot (`rot4`/`rot3`) KV: tylko single-stream decode (batched → jawny
  Unsupported); decode ~1.5× wolniejszy od f16 — wartość to pamięć/max-ctx.
- Kontekst ~1M: konfiguracja go nie blokuje, ale fizycznie wymaga KV
  offloadu do RAM/SSD (TierManager ze SPEC §5.4 — nie zbudowany).
- ONNX: niewspierany (planowany). TTS: brak silnika (planowany).
- `logprobs`/`n>1` w API: jeszcze nie.
