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
| `--kv-pages <N>` (serve) | `512` | Liczba stron KV cache (32 tokeny/strona) współdzielonych przez wszystkie sekwencje. Bez tieringu automatycznie podnoszona do co najmniej jednego pełnego okna `--ctx` (budżet ponad okno = współbieżne sekwencje). Z `--kv-tier` przeciwnie: to jest GORĄCY budżet VRAM (przycinany do okna), a reszta kontekstu spilluje do RAM/NVMe. |
| `--kv-pages <N>` (run / bench) | `0` | `0` = pula na pełne okno `--ctx` (dzisiejsze zachowanie). Jawna wartość ogranicza gorący working set VRAM — użyteczne z `--kv-tier`. |

Wewnętrzne (ModelConfig, niewystawione jako flagi): `kv_page_size=32` tokeny/strona.

---

## KV tiering: VRAM → pinned RAM → NVMe (serve / run / bench)

`TierManager` (SPEC §5.4B, `forge-engine/src/tier.rs`): zimne strony KV są
spillowane z VRAM w **chunkach 4–16 MB** per (sekwencja × wszystkie warstwy)
do przypiętego RAM-u, a po przekroczeniu budżetu RAM — do append-only pliku
NVMe (FIFO demote, zapis sekwencyjny; żadnych małych losowych I/O na dysk).
Sekwencja, której spilled KV nie mieści się z powrotem w VRAM, dekoduje przez
ścieżkę STRUMIENIOWANĄ: per warstwa staging slab dostaje PEŁNY kontekst tej
warstwy (chunki z RAM/pliku + strony rezydentne D2D) i attention działa na
identity page table. Wynik jest **bitowo identyczny** z przebiegiem bez
tieringu (bajty KV są przenoszone, nie transformowane; te same kernele w tej
samej kolejności) — udowodnione testami `forge-engine/tests/kv_tier.rs` i na
Bielik-7B-NVFP4 (8k kontekstu na budżecie VRAM 2k tokenów, needle-recall przez
granicę spillu, identyczne id tokenów greedy).

| Flaga | Domyślnie | Opis |
|---|---|---|
| `--kv-tier <M>` | `off` | `off` \| `ram` \| `nvme`. `off` = dzisiejsze zachowanie (zero zmian). `ram` = spill do pinned RAM (twardy błąd po wyczerpaniu budżetu). `nvme` = RAM jako warm cache + append-only plik jako cold tier. |
| `--kv-tier-dir <PATH>` | temp dir | Katalog pliku spillu (`nvme`). Domyślnie `$TMPDIR/forge-kv-tier-<pid>`, usuwany przy zamknięciu. |
| `--kv-tier-ram-gb <F>` | `8` | Budżet pinned RAM na warm chunki (GiB). |
| `--kv-tier-watermark <F>` | `0.10` | Rezerwa wolnych stron VRAM: spill proaktywny gdy free/total spada poniżej; restore tylko gdy zmieści się BEZ naruszenia rezerwy (anty-thrash). |

Zasady i zmierzone charakterystyki (Bielik-7B-NVFP4, RTX 4090, PCIe 4.0):

- **Hot path bez spillu: zerowa kara.** Decode 154.2 tok/s z tieringiem ON
  (bez presji) i OFF — identycznie; graf CUDA decode nietknięty.
- **Streamed decode** (kontekst 8k na 2k-tokenowym VRAM): ~8.6 tok/s (ram) /
  ~8.1 tok/s (nvme) vs 113 tok/s bez tieringu — koszt to ~0.7 GB transferu KV
  na token przez PCIe. Prefill 8k: 1984 tok/s vs 2321 bez tieringu.
- **Pasma (EMA z realnych transferów):** spill D2H ~11 GB/s, restore ~4.5-4.8
  GB/s, zapis pliku ~5 GB/s (buforowany; odczyty wspiera page cache OS).
- **Transfer-vs-recompute:** każdy restore loguje decyzję (`kv tier
  decision:`) z estymatami z mierzonych pasm; recompute (pełny re-prefill
  z zachowanych tokenów) wybierany tylko gdy wygrywa czasowo I historia jest
  czysto prefillowa (KV pisane przez decode nie jest bitowo odtwarzalne
  re-prefillem — wtedy zawsze transfer).
- **Hot tail:** ostatnie 4 strony (128 tokenów) sekwencji nigdy nie są
  spillowane (decode zawsze czyta ogon; append idzie do strony rezydentnej).

Zakres v2 (dawne ograniczenia v1 — usunięte):
- Tryby `rot4`/`rot3` współpracują z `--kv-tier`: spillowane są paged regiony
  packed + skale (ring rezydualny zostaje w VRAM), a streamed attention czyta
  packed store przez staging za identity page table. `f16`/`fp8` bez zmian.
- Eviction jest cross-sequence: scheduler co iterację balansuje pulę i
  spilluje globalnie najzimniejsze prefiksy WSZYSTKICH aktywnych sekwencji
  (największy zimny prefiks pierwszy), więc długi request nie czeka na zimną
  historię sąsiadów.
- Sekwencja strumieniowana WCHODZI do batched decode: streamed lanes idą na
  koniec batcha, GEMM-y liczą pełny batch, attention rezydentnych lane'ów to
  jeden launch, a streamed lane attenduje po stagingu swojego pełnego
  kontekstu. Mixed batch nie jest graf-capturowany; czysto rezydentny batch
  zostaje na grafach per bucket.
- Restore strumieniowy fused decode overlapuje warstwa-po-warstwie: staging
  warstwy l+1 jedzie osobnym streamem transferowym (ping-pong 2 sloty
  stagingu + eventy) podczas attention warstwy l; pełny restore overlapuje
  odczyt pliku z H2D (2 pinned scratche). Plik spillu reużywa zwolnione
  extenty (exact-size free list) — rośnie do szczytowego working setu, nie
  monotonicznie; znika przy wyjściu.

### KVFlash: stały mały gorący budżet VRAM (serve / run / bench)

Tryb Luce-KVFlash: **ślad KV w VRAM jest stałą, małą gorącą pulą niezależnie
od długości kontekstu**, a wszystko poza nią żyje w RAM/NVMe. Kontekst
pojedynczej sekwencji jest ograniczony przez RAM+dysk, nie przez VRAM — pula
VRAM nie rośnie z kontekstem. To nadbudówka konfiguracyjna nad tieringiem: gdy
`--kv-hot-pages` jest ustawione, **nadpisuje** domyślne sizowanie
`kv_pages = pełne okno kontekstu`, przez co VRAM pozostaje stały gdy kontekst
rośnie.

| Flaga | Domyślnie | Opis |
|---|---|---|
| `--kv-hot-pages <N>` | `0` | Ustala gorącą pulę KV w VRAM na dokładnie N stron (32 tokeny/strona) niezależnie od `--ctx`; reszta okna strumieniuje przez tier. `0` = dzisiejsze zachowanie (pula VRAM na pełne okno). Wymaga `--kv-tier ram\|nvme` — bez tieru nie ma dokąd wypchnąć nadmiaru (jasny błąd). Musi pokryć co najmniej minimalną rezydencję silnika (jeden chunk prefillu + gorący ogon, `min_resident_pages` = 37 stron przy page_size=32); niższa wartość to jasny błąd zamiast zakleszczenia. |
| `--kvflash` | `false` | Skrót: włącza tier `nvme` + małą domyślną gorącą pulę (`256` stron = 8k tokenów gorące), jeśli użytkownik nie ustawił `--kv-tier` / `--kv-hot-pages` jawnie. Komponuje się z `--kv-tier-dir` / `--kv-tier-ram-gb`. |

Przykłady:
```bash
# 2k-tokenowa gorąca pula VRAM (64 strony), reszta na NVMe; VRAM KV stały
forge run <bielik> "<prompt>" -n 400 --kvflash --kv-hot-pages 64
# serve z 2k gorącym budżetem admituje request > 2k tokenów (streamed decode)
forge serve <bielik> --kvflash --kv-hot-pages 64
```

Zweryfikowane na Bielik-7B-NVFP4 (RTX 4090): przy `--kv-hot-pages 64` (pula
VRAM 2k tokenów) kontekst rosnący ponad 2k tokenów kończy się poprawnie, pula
KV w VRAM pozostaje stała (log sizowania puli / region KV `nvidia-smi`), a
needle zasadzony wcześnie jest odtworzony po tym, jak gorąca pula wykręciła
się w całości (spill przez granicę + restore).

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
| + `--ctx`, `--weights-pool-gb`, `--kv-pages`, `--kv-cache`, `--kv-residual-window`, `--kv-activate-at`, `--kv-tier*`, `--kv-hot-pages`, `--kvflash` | jw. | Jak w sekcjach wyżej. |

## forge bench

| Flaga | Domyślnie | Opis |
|---|---|---|
| `--tokens <N>` | `128` | Tokeny decode do zmierzenia. |
| `--prompt-tokens <N>` | `512` | Długość promptu (prefill). |
| + `--ctx`, `--kv-pages`, `--kv-cache`, `--kv-residual-window`, `--kv-activate-at`, `--kv-tier*` | jw. | |

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
- Architektury: qwen3, llama, mistral, **olmoe** (MoE), **qwen3moe** (MoE)
  (rejestr deklaratywny w forge-formats); Whisper (osobny silnik); modele
  embeddingowe (pooling z metadanych).
- head_dim: 64 i 128 (specjalizacje attention); inne → jasny błąd.

## MoE (Mixture-of-Experts)

- Wykrywane automatycznie z metadanych GGUF (`<arch>.expert_count > 0`):
  liczba ekspertów, top-k (`expert_used_count`), szerokość eksperta
  (`expert_feed_forward_length`), renormalizacja wag (`expert_weights_norm`),
  opcjonalny shared expert (`expert_shared_feed_forward_length`). Router to
  `ffn_gate_inp`, eksperci to stackowane `ffn_{gate,up,down}_exps`.
- Bez flag — działa jak zwykły model (`forge run olmoe.gguf "…" -n 24 --temp 0`).
- Routing zgodny z HF: softmax po WSZYSTKICH ekspertach → top-k → opcjonalny
  renorm. Obsługiwane QK-norm: pełny wektor (OLMoE) i per-head (qwen3moe).
- Ograniczenia MoE (v1, correctness-first): tylko KV `f16` (bez fp8/rot), bez
  KV-tieringu, tylko single-stream decode (batched/`--max-active`>1 → jawny
  Unsupported). Prefill przetwarza całą paczkę naraz; decode jeden token/krok
  (wybór ekspertów zależny od danych → brak CUDA-graph). Perf-follow-up:
  grouped-GEMM permute/unpermute i batched-MoE decode.
- Warstwy MTP/NextN (`nextn_predict_layers`) są pomijane w podstawowej generacji.
- Hybrydy SSM+MoE (np. `qwen35moe` = Qwen3.6-35B-A3B, z warstwami Gated-DeltaNet)
  są NIEWSPIERANE — wymagają kerneli skanu SSM (osobny projekt); ładowanie zwraca
  jasny błąd „unsupported arch", nie generuje śmieci.

## Ograniczenia znane (uczciwie)

- Tryby rot (`rot4`/`rot3`) KV: tylko single-stream decode (batched → jawny
  Unsupported); decode ~1.5× wolniejszy od f16 — wartość to pamięć/max-ctx.
- Kontekst ponad VRAM wymaga `--kv-tier ram|nvme` (sekcja KV tiering wyżej);
  prędkość streamed decode ogranicza PCIe (~0.7 GB KV na token przy 8k na
  modelu 7B f16-KV — rośnie liniowo z głębokością).
- Tokenizer HF (`tokenizer.json`) jest używany z wbudowanymi ustawieniami
  truncation modelu — np. snapshot Bielika przycina pojedyncze `encode` do
  2048 tokenów; długie prompty składaj z kawałków (patrz
  `examples/kv_tier_longctx.rs`).
- ONNX: niewspierany (planowany). TTS: brak silnika (planowany).
- MoE: tylko single-stream decode + KV f16 (patrz sekcja MoE); hybrydy SSM+MoE
  (Qwen3.6 `qwen35moe`) niewspierane.
- `logprobs`/`n>1` w API: jeszcze nie.
