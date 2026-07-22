# FORGE — Konfiguracja (INFER_CONFIGURATION.md)

Kompletny opis wszystkich parametrów silnika: flagi CLI, parametry API,
zmienne środowiskowe. **Reguła utrzymania: każda zmiana/nowy parametr MUSI być
dopisany do tego pliku w tym samym commicie, który go wprowadza.** Źródłem
prawdy jest `forge <cmd> --help` — ten plik dodaje kontekst, zakresy i zalecenia.

Ostatnia aktualizacja: 2026-07-18.

---

## Komendy

```
forge pull        # pobranie modelu z HuggingFace Hub (GGUF lub snapshot)
forge serve       # serwer HTTP z OpenAI-compatible API
forge run         # jednorazowa generacja do stdout (streaming)
forge bench       # pomiar prefill/decode tok/s
forge embed       # wektor embeddingu dla tekstu
forge transcribe  # transkrypcja WAV (Whisper)
forge onnx-run    # załaduj model ONNX, wypisz pokrycie opów, uruchom na GPU (VAD)
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
spillowane z VRAM w **chunkach 4–16 MB** per (sekwencja × warstwy zarządzane
przez tier) do przypiętego RAM-u, a po przekroczeniu budżetu RAM — do append-only pliku
NVMe (FIFO demote, zapis sekwencyjny; żadnych małych losowych I/O na dysk).
Sekwencja, której spilled KV nie mieści się z powrotem w VRAM, dekoduje przez
ścieżkę STRUMIENIOWANĄ: per warstwa staging slab dostaje PEŁNY kontekst tej
warstwy (chunki z RAM/pliku + strony rezydentne D2D) i attention działa na
identity page table. Wynik jest **bitowo identyczny** z przebiegiem bez
tieringu (bajty KV są przenoszone, nie transformowane; te same kernele w tej
samej kolejności) — udowodnione testami `forge-engine/tests/kv_tier.rs` i na
Bielik-7B-NVFP4 (8k kontekstu na budżecie VRAM 2k tokenów, needle-recall przez
granicę spillu, identyczne id tokenów greedy).

`TierManager` zarządza tylko WYBRANYMI warstwami: dla modeli gęstych/rot to
wszystkie warstwy (`layer_kinds` jest w całości `Attention` → zero zmian
zachowania), a dla hybrydy `qwen35moe` — tylko ~10 warstw atencji (30 warstw
DeltaNet trzyma rezydentny stan SSM, nigdy nie paged). Chunki pakują te warstwy
po ich pozycji w liście (indeks „kompaktowy"), więc hybrydowy chunk niesie ~10
warstw atencji zamiast 41 — patrz sekcja qwen35moe.

Rezydentny target KV używa tego samego zwartego mapowania
`global_layer -> kv_layer`: slaby są alokowane wyłącznie dla warstw
`Attention`, a DeltaNet nie zajmuje pustych par K/V. Dla Qwen3.6-27B z 48
warstwami DeltaNet i 16 warstwami pełnej atencji zmniejsza to surowy koszt
F16 KV z 256 KiB do 64 KiB na token. Osobny, jednowarstwowy cache MTP nie
wchodzi do tej mapy.

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
(trwałe przekroczenie `--ctx`), nigdy OOM w połowie generacji. Trafienie w
prefix-cache (niżej) zmniejsza projekcję stron przy przyjęciu, a strony
odzyskiwalne z cache liczą się jako dostępne — pełny-ale-odzyskiwalny cache nigdy
nie blokuje przyjęcia żądania, które by się zmieściło.

---

## Radix prefix cache: dedup współdzielonych prefiksów KV (serve / run / bench)

`PrefixCache` (SPEC §5.2, `forge-engine/src/prefix.rs`): drzewo radix indeksowane
sekwencją token-id, które dedupuje współdzielone prefiksy KV (system-prompty,
few-shot, kolejne tury czatu). Nowe żądanie PRZED prefillem przechodzi drzewo,
dopasowując tokeny promptu do zapamiętanych prefiksów; strony najdłuższego
pasującego prefiksu są WSPÓŁDZIELONE (refcount, read-only), a prefillowany jest
tylko rozbieżny ogon. Po zakończeniu sekwencja DONUJE swoje własne, świeżo
zprefillowane pełne strony z powrotem do drzewa, przedłużając wspólny prefiks dla
kolejnych żądań.

**Poprawność (twarda):** bajty KV to deterministyczna funkcja prefiksu tokenów
ORAZ ścieżki kerneli prefillu. Cache trzyma WYŁĄCZNIE strony zbudowane przez
prefill, więc pożyczony prefiks jest bajt-w-bajt identyczny z tym, co
zprefillowałoby żądanie bez trafienia — drugie żądanie ze wspólnym prefiksem daje
DOKŁADNIE te same tokeny co bez cache. Współdzielenie jest na granularności
CAŁYCH stron (32 tokeny), więc borrower nigdy nie pisze do współdzielonej strony
(strony KV są append-only, a pierwszy zapis trafia w nową stronę na granicy) — nie
ma potrzeby copy-on-write częściowej strony granicznej.

**Eviction:** gdy wolnych stron brakuje, wyrzucane są liście drzewa z refcount 0
w kolejności LRU, a ich strony wracają na stos wolnych. Strony wskazywane przez
aktywne sekwencje (refcount > 0 lub przodek żywej pożyczki) nigdy nie są
wyrzucane. Reclaim jest wołany przed wzrostem prefillu/decode, więc trafienie w
cache nigdy nie zagłodzi puli.

| Flaga | Domyślnie | Opis |
|---|---|---|
| `--prefix-cache <M>` | `on` | `on` \| `off`. `on` = dedup współdzielonych prefiksów KV; **ścisła optymalizacja** — włącza się tylko gdy nic nie psuje. `off` = bajt-w-bajt dzisiejsze zachowanie (zero akwizycji/donacji). |

Zakres i komponowanie:

- Aktywny tylko dla **verbatim KV `f16`/`fp8`, bez tieringu, arch nie-hybrydowa**.
  Z `--kv-tier`/`--kvflash` (spill przepisuje/przenosi strony), trybami `rot4`/
  `rot3` (residual ring indeksowany pozycją, nie stroną) i arch hybrydową
  `qwen35moe` (rezydentny stan SSM nie żyje w stronach KV) cache jest CICHO
  nieaktywny — zachowanie bit-identyczne z dzisiejszym. MoE gęste (`f16`) i modele
  dense są wspierane.
- Usage API zwraca `prompt_tokens_details.cached_tokens` = długość trafionego
  prefiksu (pomijana gdy 0). To pole `cache_read_tokens` ze SPEC §8.1.2.

Zweryfikowane (RTX 4090, qwen3-0.6b-q8_0, `tests/prefix_cache.rs`):

- **Współdzielony prefiks bit-identyczny + pominięty prefill:** dwa żądania z tym
  samym prefiksem 2048 tokenów (greedy) — drugie raportuje `cache_read=2016`
  (63 pełne strony), a prefill spada z **68.8 ms (cold, ~29.8k tok/s) do 14.8 ms
  (hit) = 4.7× szybciej**; wygenerowane id są identyczne co do bitu z przebiegiem
  cold ORAZ z przebiegiem `--prefix-cache off`.
- **Multi-turn:** tura 2 (prompt = tura 1 + rozszerzenie) reużywa strony KV tury 1
  (`cache_read=128`), poprawnie i szybciej; wynik identyczny z przebiegiem od
  zera bez cache.
- Golden NVFP4 (Bielik-7B, `tests/batched_bielik.rs`, `--prefix-cache off`)
  reprodukuje dokładne id — ścieżka OFF jest bit-identyczna z dzisiejszą.

---

## Dekodowanie spekulatywne (SPEC §6)

Działają trzy tryby: hostowy proposer n-gram, natywna głowa MTP/NextN modelu oraz
priorytetowy router `mtp+ngram`.
W trybie n-gram na każdym
kroku greedy proposer szuka najdłuższego dopasowania sufiksu w WŁASNEJ historii
sekwencji (prompt + wygenerowane) i drafuje kontynuację; silnik weryfikuje cały
draft JEDNYM przebiegiem (mini-prefill nad pozycjami draftu), akceptuje najdłuższy
prefiks, którego token równa się argmaxowi modelu na poprzedniej pozycji, a
odrzucone pozycje KV wycofuje (`KvCache::rollback`). Jeden forward daje 1..=k+1
tokenów, **identycznych co do tokena** z dekodowaniem sekwencyjnym tam, gdzie
argmax jest jednoznaczny — akcelerator dokładnego dekodowania, nie przybliżenie.

Natywne MTP jest dostępne dla obsługiwanego, gęstego hybrydowego GGUF `qwen35`
z `nextn_predict_layers`. Model generuje draft K=2 lub K=3 na GPU, a target
weryfikuje cały blok jednym batched przebiegiem. Zaakceptowany prefiks zatwierdza
KV oraz retained checkpointy DeltaNet bez ponownego skanu warstw. `mtp` oznacza
budżet 3 z adaptacyjnym wyborem K=2/K=3; `mtp:2` i `mtp:3` wymuszają maksymalny
budżet, przy czym K=3 nadal może spaść do K=2 przy końcu dostępnego kontekstu.
Wagi i stan NextN są ładowane tylko po jawnym wybraniu trybu MTP; przy
`--speculative off` nie zwiększają zużycia VRAM ani czasu prefill.

Router `mtp+ngram:2|3` najpierw próbuje pełnego draftu n-gram o długości K. Przy
trafieniu target weryfikuje draft bez zależności od jego źródła, a stan MTP jest
doganiany po `fed + accepted` bez liczenia logitów. Przy braku pełnego draftu
router używa zwykłego natywnego MTP. Liczniki `ngram_forwards` i
`mtp_fallback_forwards` rozdzielają obie ścieżki w logu końcowym.

To opis aktualnie wykonywanych ścieżek, a nie pełnego docelowego interfejsu ze
`SPEC.md` §6. Fundament ma typowane `DraftTree`/`DraftNode`,
`SpeculationCoordinator`, kaskadową kompozycję liniową i konfiguracje wielu
proposerów. CLI odrzuca `draft-model`, `eagle`, `dflash` i `dspark` z
jawnym komunikatem o wymaganym loaderze i `forge-speculation.json`, dopóki nie ma
wykonawczego proposera i zgodnych wag. PARD i suffix nie są jeszcze wartościami CLI. N-gram docelowo
pozostanie opcjonalnym fallbackiem lub rozszerzeniem ich draftów, korzystającym z
tego samego lossless verifiera.

Typowane API `SpeculativeConfig::chain` zachowuje kolejność proposerów, odrzuca
pustą listę i duplikaty oraz dopuszcza n-gram wyłącznie jako ostatni element.
CLI nie udostępnia jeszcze składni łańcuchów neuralnych.

| Flaga | Domyślnie | Opis |
|---|---|---|
| `--speculative <M>` | `off` | Działające: `off` \| `on` \| `ngram` \| `ngram:<k>` \| `mtp` \| `mtp:2` \| `mtp:3` \| `mtp+ngram` \| `mtp+ngram:2` \| `mtp+ngram:3`. `on`/`ngram` = proposer n-gram z budżetem 16; jego sufiks `:<k>` wymaga 1..=16. `mtp` = natywna głowa modelu, maksymalny K=3 i wybór adaptacyjny. `mtp+ngram` = priorytet n-gram K=3 z fallbackiem MTP. |

Przykład dla modelu z natywną głową MTP:

```bash
cargo run -p forge-cli --release -- run MODEL.gguf "Przykładowy prompt" \
  --temp 0 --prefix-cache off --speculative mtp+ngram:3
```

Polecenie `serve` dobiera dla MTP domyślne `--max-active 1`; jawna większa
wartość uruchamia startup preflight puli stanów per sekwencja. Jeśli wymagane
sloty nie mieszczą się w pulach weights/activations, worker nie startuje, a błąd
podaje wymagane i dostępne bajty. `run` zawsze obsługuje jedną sekwencję.

### Manifest neuralnego proposera

Plik `forge-speculation.json` ma wersjonowany, zamknięty schemat; nieznane pola są
błędem. Najważniejsze pola:

| Pole | Znaczenie |
|---|---|
| `format_version`, `kind`, `composition` | Wersja formatu, `draft_model`/`mtp`/`eagle`/`dflash`/`dspark` oraz `standalone`/`cascade`/`tree`. |
| `source` | Adres źródła i repozytorium HTTP(S). |
| `license` | Osobne SPDX dla kodu i wag, zgoda na redystrybucję i wymagane attribution. `unknown` nie jest akceptowane. |
| `target`, `target_fingerprint`, `tokenizer_fingerprint` | Id/revision/architektura i rozmiary targetu oraz fingerprinty SHA-256 modelu i tokenizera. |
| `max_nodes`, `max_depth`, `block_size`, `diffusion_steps` | Granice propozycji; pola zależne od rodzaju proposera. |
| `artifacts`, `tensor_map`, `shared_tensors` | Role i ścieżki plików, SHA-256, logiczne mapowanie tensorów oraz tensory współdzielone z targetem. |
| `target_feature_layer_ids`, `feature_dimensions` | Warstwy i wymiary cech wymagane przez EAGLE/DFlash/DSpark. |
| `dtype`, `quantization`, `supported_sampling_modes` | Reprezentacja wag i jawnie obsługiwane tryby samplingu. |
| `confidence_calibration` | Metoda, próg, temperatura lub artefakt kalibracji; wymagana dla DFlash/DSpark. |

`SpeculationManifest::load` weryfikuje składnię i zależności pól, kanonikalizuje
ścieżki artefaktów wewnątrz katalogu manifestu, czyta pliki strumieniowo i porównuje
ich obliczony SHA-256 z `artifacts[].sha256`. Zwraca otwarte, zweryfikowane uchwyty,
aby runtime nie otwierał ponownie plików po kontroli integralności. Fingerprinty targetu i tokenizera są
na tym etapie sprawdzane jako 64-znakowe SHA-256; ich porównanie z uruchomionym
modelem będzie częścią integracji neuralnego runtime. Pole
`redistribution_allowed=false` nie blokuje lokalnego odczytu, ale zabrania
pakowania artefaktu w dystrybucji FORGE bez odrębnej podstawy licencyjnej.

Zakres i komponowanie:

- N-gram włącza się TYLKO dla żądań **greedy** (`temperature == 0`, bez repetition
  penalty i bez host-logit features: grammar / `logit_bias` / `min_tokens` /
  `logprobs`) na **gęstej ścieżce F16 paged-KV** (bez `--kv-tier`/`--kvflash`,
  bez `rot4`/`rot3`, nie-hybrid, nie-MoE, głowica `lm_head` f16/q8_0). Pozostałe
  żądania CICHO spadają do zwykłego dekodowania — wynik bez zmian.
- **Wymusza `--prefix-cache off`** (obie funkcje zarządzają własnością stron KV;
  weryfikacja dopisuje i wycofuje draftowe strony na gołej puli).
- Natywne MTP i `mtp+ngram` wymagają obsługiwanego modelowego runtime MTP oraz greedy z próbkowaniem
  GPU bez repetition penalty. Target DeltaNet i draft MTP
  mają osobny stan per sekwencja pod wspólnym lease; strony KV draftu pochodzą
  z jednej współdzielonej puli MTP. `max_active > 1` wymaga udanego startup
  preflightu i używa seryjnie przeplatanego forwardu per sekwencja. Nie ma cichego fallbacku: niezgodna
  konfiguracja kończy się błędem przed uruchomieniem workera.
- Spekulacja stochastyczna (`temp > 0`) NIE jest zaimplementowana w v1 — tylko
  greedy-exact. Statystyki akceptacji per-proposer i adaptive-disable (usypianie
  proposera, gdy szacowany zysk < 1.05 w oknie) są aktywne.
- Weryfikacja idzie NIEGRAFOWANĄ ścieżką prefill, więc na małym modelu opłaca się
  dopiero dla długich draftów (gate `MIN_VERIFY_DRAFT`): krótkie drafty (typowe dla
  zwykłej prozy) spadają na tani graf-krok pojedynczego tokena, więc ruch
  nie-powtarzalny nie regresuje. Zysk rośnie z długością akceptowanych serii i z
  rozmiarem modelu (decode compute/bandwidth-bound).
- Raport na końcu sekwencji (log `INFO`): liczba forwardów weryfikacji,
  akceptowane tokeny draftu, akceptacja/krok, tok/forward i migawka statystyk
  proposera.

Zweryfikowane (RTX 4090, qwen3-0.6b-q8_0, `tests/e2e_speculative.rs`):

- **Powtarzalny prompt (`--speculative on`) == `off` co do tokena**, ~1.45–1.6×
  szybciej; `forge run … --speculative on` na powtarzalnym ciągu: 64 tokeny w 4
  forwardach weryfikacji = **16 akceptowanych/krok, 17 tok/forward**.
- **Prompt zwykły**: akceptacja ≈ 0, wynik identyczny, brak regresji (fallback na
  pojedynczy graf-krok; ~0.88× to koszt taniego proposera per krok).
- Golden NVFP4 (Bielik-7B, `--speculative off`) reprodukuje dokładne id — ścieżka
  OFF jest bit-identyczna z dzisiejszą.
- Rollback stron KV: jednostkowy `tests/kv_rollback.rs`; `verify_greedy`
  (długość akceptowanego prefiksu): `speculation::tests`.

Zweryfikowane natywne MTP (RTX 4090,
`protoLabsAI/ThinkingCap-Qwen3.6-27B-MTP-GGUF`, lokalny wariant NVFP4):

- wielocyklowy wynik MTP jest porównywany token po tokenie z sekwencyjnym greedy;
- retained checkpointy DeltaNet są używane podczas commit zaakceptowanego prefiksu;
- catch-up routera przeszedł bitową zgodność target h/x/SSM i MTP hidden/K/V/len
  dla wymuszonych długości akceptacji 0..K;
- batchowy catch-up MTP normalizuje przesunięte pary embedding/target hidden,
  wykonuje wspólne `eh_proj` i zapisuje wyłącznie K/V;
- na raw128 catch-up spadł z około **52,8 ms do 2,814 ms**, a na raw512 z
  **227,696 ms do 11,262 ms**, z identycznymi SHA tokenów względem referencji;
- fair prose K=3: raw128 około **105,1 tok/s** przy akceptacji 96,0%, raw512
  około **71,2 tok/s** przy akceptacji 59,3%;
- `mtp+ngram:3` prose: raw128 **118,3-118,5 tok/s** przy akceptacji 97,0%,
  raw512 **78,0-78,3 tok/s** przy akceptacji 63,7%. Są to wyniki actual;
  `oracle_upper` pozostaje osobną górną granicą;
- K=3: raw128 około **86,9 tok/s**, raw512 około **83,8 tok/s** po włączeniu
  retained checkpointów, głowy Q8 B3/B4, scalonego przygotowania DeltaNet,
  szybkich projekcji NVFP4 B3/B4 i grafów verifiera T=3/T=4;
- pomiary dotyczą wyłącznie CUDA. Źródła kerneli Mojo są przenośnym punktem
  wyjścia dla AMDGPU/Metal, ale tych backendów nie uruchomiono ani nie
  zweryfikowano.

Pełny protokół i porównanie: `docs/BENCH_QWEN35_MTP_NVFP4.md`.

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
| `--speculative <M>` | `off` | Dekodowanie spekulatywne (patrz sekcja wyżej): `off` \| `on` \| `ngram` \| `ngram:<k>`. Wymusza `--prefix-cache off`. |

Endpointy: `/v1/chat/completions`, `/v1/completions`, `/v1/messages`
(Anthropic), `/v1/models`, `/v1/embeddings`, `/v1/audio/transcriptions`,
`/healthz`, `/metrics`.

`/healthz` i `/metrics` są POZA bramką `--api-key` (standardowy health-check +
scrape Prometheus). Reszta `/v1/*` jest za bramką gdy `--api-key` ustawione.

---

## Metryki Prometheus (`GET /metrics`, SPEC §8.3)

Format text exposition 0.0.4 (`Content-Type: text/plain; version=0.0.4`).
Wszystkie wartości pochodzą z realnego stanu silnika/HTTP (nic syntetycznego);
wątek workera aktualizuje atomiki in-place, handler tylko czyta.

| Metryka | Typ | Opis |
|---|---|---|
| `forge_http_requests_total{route,status}` | counter | Żądania HTTP wg szablonu trasy + kod statusu. |
| `forge_engine_requests_started_total` | counter | Requesty przyjęte do aktywnego slotu. |
| `forge_engine_requests_finished_total` | counter | Requesty zakończone (terminal Done). |
| `forge_engine_requests_errored_total` | counter | Błąd silnika / odrzucenie admission / rozłączenie klienta. |
| `forge_engine_prompt_tokens_total` | counter | Tokeny promptu (zakończone requesty). |
| `forge_engine_generated_tokens_total` | counter | Tokeny wygenerowane (completion). |
| `forge_engine_cache_read_tokens_total` | counter | Tokeny promptu z radix prefix-cache (§5.2). |
| `forge_engine_speculative_forwards_total` | counter | Forwardy weryfikacji spekulacji (§6; 0 gdy off). |
| `forge_engine_speculative_accepted_total` | counter | Zaakceptowane tokeny draftu. |
| `forge_engine_active_sequences` | gauge | Sekwencje aktualnie dekodujące/prefillujące. |
| `forge_engine_queued_sequences` | gauge | Głębokość kolejki admission (KV pressure). |
| `forge_engine_kv_pages_total` / `_used` | gauge | Strony KV w puli VRAM: łącznie / zajęte. |
| `forge_engine_ttft_seconds` | histogram | Time-to-first-token per request. |
| `forge_engine_inter_token_seconds` | histogram | Latencja między tokenami per krok decode. |
| `forge_engine_decode_tokens_per_second` | histogram | Przepustowość decode per request. |
| `forge_build_info{model}` | gauge | Tożsamość serwowanego modelu (wartość 1). |

Prefix-cache hit-rate liczysz jako `cache_read_tokens_total / prompt_tokens_total`;
acceptance spekulacji jako `speculative_accepted_total / speculative_forwards_total`.

---

## API Anthropic Messages (`POST /v1/messages`, SPEC §8.1)

Warstwa translacji nad tą samą ścieżką generacji co `/v1/chat/completions`.

| Pole | Uwagi |
|---|---|
| `model` | Musi zgadzać się z `--model-id` (inaczej 404). |
| `system` | String lub tablica bloków `{type:"text",text}` → prepend jako `system`. |
| `messages` | `role` ∈ `{user,assistant}`; `content` string lub tablica bloków (tekst konkatenowany; bloki nie-tekstowe pomijane). |
| `max_tokens` | WYMAGANE (budżet completion); brak/0 → 400. |
| `stop_sequences` | Tablica stringów (holdback jak `stop`). |
| `temperature` / `top_p` / `top_k` | Jak w OpenAI. |
| `stream` | SSE Anthropic (patrz niżej). |

Odpowiedź non-stream: `{id,type:"message",role:"assistant",model,
content:[{type:"text",text}],stop_reason,stop_sequence,usage:{input_tokens,
output_tokens}}`. Streaming: sekwencja eventów `message_start` →
`content_block_start` → `content_block_delta{delta:{type:"text_delta",text}}` →
`content_block_stop` → `message_delta{delta:{stop_reason}}` → `message_stop`.

`stop_reason`: EOS→`end_turn`, limit `max_tokens`→`max_tokens`, dopasowana
stop-sekwencja→`stop_sequence`. `<think>…</think>` zdejmowany (nie trafia do
bloku tekstowego). Braki: bloki `tool_use`/`tool_result`, `thinking`,
`/v1/messages/count_tokens`.

---

## Parametry żądań API (OpenAI-compatible, per request)

`POST /v1/chat/completions` / `/v1/completions`:

| Pole | Domyślnie | Uwagi |
|---|---|---|
| `temperature` | 0.7 | `0` = greedy (argmax, deterministyczny). |
| `top_k` | 40 | Sampling na GPU dla 1..64; większe → ścieżka CPU. |
| `top_p` | 0.95 | Nucleus. |
| `min_p` | 0.0 | Próg względem najlepszego kandydata. |
| `repetition_penalty` | 1.0 | Każdy token w oknie jest transformowany raz, niezależnie od liczby wystąpień. |
| `frequency_penalty` | 0.0 | OpenAI `[-2, 2]`; odejmuje wartość pomnożoną przez liczbę wystąpień tokenu. |
| `presence_penalty` | 0.0 | OpenAI `[-2, 2]`; odejmuje wartość raz dla każdego obecnego tokenu. |
| `repeat_last_n` | 0 | Okno historii dla wszystkich kar; `0` oznacza całą przekazaną historię. |
| `seed` | czas | Deterministyczny strumień per (seed, krok). |
| `max_tokens` / `max_completion_tokens` | — | Walidowane względem `--ctx`: prompt+max_tokens ≤ ctx, inaczej 400 `context_length_exceeded`. |
| `stop` | — | String lub tablica; holdback bez wycieków częściowych dopasowań. |
| `stream` | false | SSE; `stream_options.include_usage` → osobny finalny chunk z usage. |

Usage: `usage.prompt_tokens_details.cached_tokens` raportuje prefiks promptu
obsłużony z radix prefix-cache (SPEC §5.2; pomijane gdy 0) — patrz sekcja
„Radix prefix cache".
| `tools` / `tool_choice` | — | `tools` musi być tablicą; `tool_choice`: `auto`/`none`/`required`/named. `required`/named wymuszają poprawne wywołanie przez constrained decoding (patrz niżej). Odpowiedź: `tool_calls[]`, `finish_reason:"tool_calls"`. |
| `response_format` | — | Constrained decoding — patrz niżej. |
| `grammar` | — | Passthrough gramatyki GBNF/EBNF (root `root`). Constrained decoding. |
| `logit_bias` | — | `{token_id: bias}` (klucze-stringi), bias w [-100, 100]; ±100 ≈ twardy force/ban. Dodawany do logitów PRZED próbkowaniem. Wymusza sampler CPU. |
| `min_tokens` | 0 | Dolny próg wygenerowanych tokenów: wszystkie EOS tłumione (logit → -inf) aż do progu. Musi być ≤ `max_tokens`. Wymusza sampler CPU. |
| `logprobs` | — | Chat: `true` + `top_logprobs:N` (0..20) → `choices[].logprobs.content[]` (token, `logprob`, `bytes`, `top_logprobs[]`). Completions: `logprobs:N` (0..20) → legacy `{tokens, token_logprobs, top_logprobs, text_offset}`. Log-softmax na hoście; przy temp 0 top-1 = token próbkowany. Wymusza sampler CPU. Tylko non-streaming. |
| `echo` (completions) | false | Dokleja prompt do odpowiedzi; z `logprobs` tokeny promptu dostają `token_logprobs=null`. Tylko non-streaming. |

Aktywne kary korzystają z kompaktowego histogramu unikalnych tokenów na GPU.
Kernel Mojo `penalize_histogram_f32` nakłada repetition, frequency i presence
w jednym launchu, po którym działa istniejący równoległy argmax lub top-k.
Ścieżka batchowa wykonuje tę samą operację jednym uruchomieniem dla wszystkich
sekwencji. Historia obejmuje prompt i wygenerowaną odpowiedź oraz respektuje
`repeat_last_n`; bufory IDs i liczników są trwałe, a w trybie release nie ma
D2H histogramu. Domyślny greedy bez kar zachowuje dotychczasowy fast path bez
dodatkowego kernela, alokacji w kroku ani synchronizacji. Żądania `logprobs`
korzystają ze ścieżki CPU i raportują rozkład po nałożeniu kar.
| `n` | 1 | Liczba niezależnych completions (1..128). Osobne ziarna (`seed+i·φ`), dzielą prefiks promptu przez radix prefix-cache; `choices[0..n]`. Non-streaming (streaming przy `n>1` = 400). |

Usage przy `n>1`: `prompt_tokens` liczony raz, `completion_tokens` sumowany po
wszystkich completions.

`prompt` (completions, SPEC §8.5 batch): pojedynczy string, tablica stringów,
pojedyncza tablica token-id, albo tablica tablic token-id. Batch prompta ×`n`
completions submitowany JEST RAZEM (wszystkie `engine.submit` przed pierwszym
`await`), więc scheduler łączy je w jeden decode-batch zamiast serializować.
`choices[]` z bieżącym `index` prompt-major; `usage.prompt_tokens` liczy każdy
prompt raz. Streaming odrzucany gdy `prompts × n > 1` (400).

`logit_bias`/`min_tokens`/`logprobs` (oraz maska gramatyki) wymuszają sampler CPU
(pełne logity na hoście); żądanie bez żadnej z tych funkcji zostaje na samplerze
GPU — ścieżka bit-identyczna (golden Bielik NVFP4 bez zmian).

Myślenie (`<think>…</think>`, np. Qwen3) jest ekstrahowane do
`reasoning_content` (nie liczy się jako content).

### Constrained decoding (SPEC §8.1.2)

Silnik może fizycznie NIE wygenerować wyjścia łamiącego gramatykę: na każdym
kroku dekodowania maska logitów ustawia `-inf` każdemu tokenowi, którego bajty
nie utrzymują gramatyki spełnialną, PRZED próbkowaniem (działa dla greedy i
stochastycznego). Trzy front-endy kompilują się do jednego byte-level automatu
(`forge-grammar`):

- **`response_format: {"type":"json_object"}`** — dowolny poprawny JSON.
- **`response_format: {"type":"json_schema","json_schema":{"schema":{…}}}`** —
  JSON zgodny ze schematem.
- **`response_format: {"type":"regex","regex":"…"}`** — całe wyjście pasuje do
  regexa (rozszerzenie poza OpenAI).
- **`response_format: {"type":"grammar","grammar":"…"}`** lub top-level
  **`grammar`** — gramatyka GBNF/EBNF (kompatybilna z llama.cpp; root `root`).
- **`tool_choice: "required"`** / **`{"type":"function","function":{"name":…}}`**
  — model MUSI wyemitować poprawne wywołanie: parametry narzędzia (JSON Schema)
  są kompilowane do gramatyki owiniętej w składnię tool-call modelu (obecnie
  Hermes/Qwen; inne parsery → 400). Parser i tak dekoduje wynik do `tool_calls[]`.

Automaty są prekompilowane i cache'owane per napis gramatyki; maski dozwolonych
tokenów są cache'owane per stan parsera + prefiltr pierwszego bajtu.

Obsługiwany subset **JSON Schema**: `object` (`properties`, `required`,
zagnieżdżenia), `array` (`items` — bez formy krotkowej), `string`
(+ `pattern` przez konwerter regex — alfabet wzorca nie może wymagać
escapowania JSON), `integer`, `number`, `boolean`, `null`, `enum`, `const`.
Niewspierane (świadomie): `anyOf`/`oneOf`/`allOf`/`$ref`, krotkowe `items`,
`additionalProperties:false` NIE jest osobno egzekwowane (nienazwane klucze po
prostu nie są generowane), a własność bez jawnej listy `required` jest
traktowana jak wymagana (ograniczenie wymusza kanoniczną, zgodną ze schematem
postać, nie akceptując dowolnej kolejności). Wewnętrzny „insignificant
whitespace" to co najwyżej jeden opcjonalny znak (nieograniczone `*` pozwoliłoby
greedy modelowi zawiesić się na spacjach).

**Regex** (subset): literały, `.`, klasy `[...]` (zakresy/negacja +
`\d \w \s \D \W \S`), grupy `(...)`/`(?:...)`, alternacja `|`, kwantyfikatory
`* + ? {n} {n,} {n,m}` (leniwe `?`/`+` akceptowane i ignorowane), kotwice `^`/`$`
akceptowane i ignorowane. Bez backreferencji/lookaround/nazwanych grup.

**Wydajność**: ścieżka constrained wymusza CPU sampler (maska potrzebuje pełnych
logitów na hoście) i skan słownika per krok — v1 stawia na poprawność, nie
prędkość. Pomiar (RTX 4090, qwen3-0.6b Q8_0): ~48 tok/s constrained vs ~800 tok/s
unconstrained. Ścieżka bez ograniczeń jest bit-identyczna (golden Bielik NVFP4
bez zmian).

`POST /v1/embeddings`: `input` (string/tablica/tokeny), `encoding_format`
(`float`|`base64`), `dimensions` (trunkacja Matryoshka + renormalizacja).

`POST /v1/audio/transcriptions`: multipart `file` (WAV, auto-resample do 16 kHz
mono), `language` (np. `pl`; ignorowane dla modeli `.en`).

---

## forge pull

Pobiera model z HuggingFace Hub i wypisuje na stdout finalną ścieżkę do podania
do `forge run` / `forge serve` (reszta logów idzie na stderr, więc ścieżkę można
podstawić w skrypcie).

```
forge pull <repo> [--file <name>] [--revision <rev>] [--token <hf_token>] [--dir <dest>]
```

| Flaga | Domyślnie | Opis |
|---|---|---|
| `<repo>` (pozycyjny) | — | Repo HF, np. `Qwen/Qwen3-0.6B-GGUF` albo `bartowski/...`. |
| `--file <name>` | auto | Konkretny plik GGUF (nazwa lub pełna ścieżka w repo, case-insensitive). Wymagany, gdy repo ma wiele kwantyzacji i brak domyślnego `Q4_K_M`. Nieużywany dla snapshotów safetensors. |
| `--revision <rev>` | `main` | Gałąź, tag lub commit git. |
| `--token <hf_token>` | `HF_TOKEN` | Token do repo gated/prywatnych (`Authorization: Bearer`). Gdy pominięty, brany z env `HF_TOKEN`. |
| `--dir <dest>` | cache XDG | Katalog docelowy. Domyślnie `$XDG_CACHE_HOME/forge/hub/<repo>` (lub `~/.cache/...`). Dla snapshotu pliki układane jak checkout HF (config.json + tokenizer.json + `*.safetensors`), tak że loader czyta je wprost. |

Zachowanie:

- **Repo GGUF** → pobiera jeden plik. Jeden GGUF w repo = brany automatycznie;
  wiele = wybierany domyślny `Q4_K_M` (z komunikatem) albo wymagany `--file`
  (błąd listuje dostępne kwantyzacje z rozmiarami).
- **Snapshot safetensors** → pobiera `config.json`, `tokenizer.json`,
  `tokenizer_config.json`, `generation_config.json`, `*.safetensors` (+ index dla
  shardowanych), szablon czatu i pozostałe metadane tokenizera. Repo bez
  `.safetensors`/`.gguf` lub bez `config.json` jest odrzucane.
- **Resume**: przerwane pobranie wznawia się z `<plik>.part` przez HTTP Range
  (`bytes=N-`); serwer ignorujący Range (200 zamiast 206) → restart od zera.
  Postęp (MB / MB, %, MB/s) leci na stderr.
- **Integralność**: pliki LFS weryfikowane po sha256 (`lfs.oid`), pozostałe po
  długości bajtowej. Zapis atomowy: `.part` → `rename` dopiero po weryfikacji.
- **Auth**: `401` → podpowiedź o `--token`/`HF_TOKEN`; `403` → podpowiedź o
  akceptacji licencji gated na stronie repo.

Przykład (pobierz i uruchom):

```
forge pull Qwen/Qwen3-0.6B-GGUF --file Qwen3-0.6B-Q8_0.gguf --dir /tmp/qwen
forge run /tmp/qwen/Qwen3-0.6B-Q8_0.gguf "Hi" -n 4
```

## forge run

| Flaga | Domyślnie | Opis |
|---|---|---|
| `-n, --max-tokens <N>` | `256` | Limit generacji. |
| `--temp <F>` | `0.7` | Temperatura (`0` = greedy). |
| `--chat` | off | Opakowuje prompt w szablon czatu modelu (user message, add_generation_prompt). |
| + `--ctx`, `--weights-pool-gb`, `--kv-pages`, `--kv-cache`, `--kv-residual-window`, `--kv-activate-at`, `--kv-tier*`, `--kv-hot-pages`, `--kvflash`, `--prefix-cache`, `--speculative` | jw. | Jak w sekcjach wyżej. |

## forge bench

| Flaga | Domyślnie | Opis |
|---|---|---|
| `--tokens <N>` | `128` | Tokeny decode do zmierzenia. |
| `--prompt-tokens <N>` | `512` | Długość promptu (prefill). |
| + `--ctx`, `--kv-pages`, `--kv-cache`, `--kv-residual-window`, `--kv-activate-at`, `--kv-tier*`, `--prefix-cache`, `--speculative` | jw. | |

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

## forge onnx-run

Ładuje model ONNX (subset opset 17+, `forge-onnx`), wypisuje opset, wejścia/
wyjścia grafu oraz histogram typów operacji (łącznie z podgrafami `If`). Gdy
model ma sygnaturę Silero VAD (wejścia `input`/`state`/`sr`), syntetyzuje jeden
frame audio i uruchamia model na GPU (kernele Mojo f32), wypisując
prawdopodobieństwo mowy oraz czas. Inne modele są tylko parsowane (uruchomienie
wymaga wejść specyficznych dla modelu).

```
forge onnx-run .runtime/models/audio/silero_vad.onnx
forge onnx-run silero_vad.onnx --signal zero
forge onnx-run silero_vad.onnx --samples 512 --sr 16000 --signal sine
```

| Flaga | Domyślnie | Opis |
|---|---|---|
| `--samples <N>` | `512` | Liczba próbek w syntetycznym frame (16 kHz VAD oczekuje 512). |
| `--sr <HZ>` | `16000` | Wartość wejścia `sr` przekazana do modelu. |
| `--signal <S>` | `sine` | Sygnał testowy: `sine` (sin(i·0.1)·0.1), `zero`, `ones`. |

Ciężka arytmetyka (Conv1d, LSTM, Relu/Sigmoid/Sqrt, Add, Pow, ReduceMean) liczy
się na GPU; operacje kształtu/kontroli (Shape/Gather/Slice/Concat/Reshape/
Transpose/Pad-reflect/Cast/Equal/Not/If) na hoście. Weryfikacja numeryczna: test
`forge-onnx` `silero_vad_matches_onnxruntime` porównuje wynik z referencją
onnxruntime (|Δ| ~1e-6, tol 1e-3). Model VAD można wskazać przez
`FORGE_SILERO_VAD` (test integracyjny).

---

## Zmienne środowiskowe

| Zmienna | Opis |
|---|---|
| `FORGE_KERNEL_DIR` | Ścieżka do `kernels/mojo/build` — nadpisuje wbudowane artefakty PTX (iteracja nad kernelami bez rebuildu Rusta). |
| `FORGE_SILERO_VAD` | Ścieżka do `silero_vad.onnx` dla testu integracyjnego `forge-onnx` (domyślnie lokalizacja w `.runtime`). |
| `FORGE_BATCH_MIN` | Nadpisuje próg `--batch-min` (diagnostyka). |
| `FORGE_PREFILL_TRACE=1` | Wypisuje czasy faz prefill_chunk (profilowanie). |
| `HF_TOKEN` | Token HuggingFace dla `forge pull` (repo gated/prywatne), gdy brak `--token`. |
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
- Architektury: qwen3, llama, mistral, **olmoe** (MoE), **qwen3moe** (MoE),
  **qwen35** (gęsty hybrid SSM z natywnym MTP) oraz **qwen35moe**
  (hybrid SSM+MoE — działa E2E, patrz sekcja qwen35moe)
  (rejestr deklaratywny w forge-formats); Whisper (osobny silnik); modele
  embeddingowe (pooling z metadanych).
- head_dim: 64, 128 (specjalizacje attention generacji) i 256 (tylko f16 KV,
  warstwy atencji qwen35moe).

## MoE (Mixture-of-Experts)

- Wykrywane automatycznie z metadanych GGUF (`<arch>.expert_count > 0`):
  liczba ekspertów, top-k (`expert_used_count`), szerokość eksperta
  (`expert_feed_forward_length`), renormalizacja wag (`expert_weights_norm`),
  opcjonalny shared expert (`expert_shared_feed_forward_length`). Router to
  `ffn_gate_inp`, eksperci to stackowane `ffn_{gate,up,down}_exps`.
- Bez flag — działa jak zwykły model (`forge run olmoe.gguf "…" -n 24 --temp 0`).
- Routing zgodny z HF: softmax po WSZYSTKICH ekspertach → top-k → opcjonalny
  renorm. Obsługiwane QK-norm: pełny wektor (OLMoE) i per-head (qwen3moe).
- Ograniczenia MoE (v1, correctness-first): tylko KV `f16` (bez fp8/rot),
  KV-tiering tylko dla hybrydy `qwen35moe` (patrz niżej — nie-hybrydowe MoE:
  OLMoE/qwen3moe → jawny Unsupported, bo brak ścieżki staged-attention decode),
  tylko single-stream decode (batched/`--max-active`>1 → jawny Unsupported).
  Prefill przetwarza całą paczkę naraz; decode jeden token/krok (wybór ekspertów
  zależny od danych → brak CUDA-graph). Perf-follow-up: grouped-GEMM
  permute/unpermute i batched-MoE decode.
- Warstwy MTP/NextN (`nextn_predict_layers`) są pomijane w podstawowej generacji.

### qwen35moe (Qwen3.6-35B-A3B, hybrid SSM+MoE) — DZIAŁA E2E

`forge run test-models/gguf/qwen36-moe.gguf "The capital of France is" -n 32
--temp 0 --weights-pool-gb 20 --ctx 4096` → „The capital of France is Paris."
(spójny angielski). W trybie `--chat` (model myślący) strumień greedy jest
identyczny co do znaku z `llama-cli`. Decode ~17 tok/s na RTX 4090 (ścieżka
correctness-first — host round-tripy per warstwa, bez grafu/wsadu; llama.cpp
~194 tok/s — optymalizacja to follow-up).

- **Loader** (`weights.rs::load_hybrid`): per-`LayerKind`; warstwa DeltaNet
  ładuje in-proj (`attn_qkv`)/conv1d (f16)/`ssm_dt`+`ssm_a` (f16)/beta+alpha
  proj/`ssm_norm`/out-proj; atencja — bramkowane Q (split, bez fuzji) + QK-norm;
  MoE z bramką shared expert (`ffn_gate_inp_shexp`). Tabela embeddingów trzymana
  host-side (gather per token) — 22 GB kwantowanych wag mieści się w VRAM 24 GB.
- **Stan SSM** (`Model.hybrid_states`): pula rezydentnych slotów per sekwencja;
  każdy przechowuje `[n_v_heads, d_state, d_state]` f32 oraz okno conv
  `[conv_dim, d_conv-1]` f16 dla każdej warstwy DeltaNet. Lease zawiera numer
  slotu i generację, a event GPU porządkuje przełączenie oraz bezpieczny reuse.
  Ten sam lease wskazuje osobny stan draftu MTP i jego `SeqKv`, korzystający ze
  współdzielonego paged cache MTP. Preflight dwóch slotów, produkcyjny admission
  oraz audyt GPU pure MTP i MTP+n-gram A/B przechodzą dla `max_active=2`.
  Verifier utrzymuje osobne grafy T=3/4 według stabilnego identyfikatora slotu;
  pierwsze wykonanie danej pary slot/T przechwytuje graf, a kolejne go odtwarzają.
- **Forward** (`hybrid_forward_token`): dispatch per-`LayerKind`; bramkowana
  atencja hd256 (deinterleave q/gate → QK-norm → partial M-RoPE `n_rot=64` →
  paged decode → `attn ⊙ σ(gate)` → o-proj), DeltaNet (conv+SiLU → split →
  L2-norm → repeat 16→32 blokowo jak `ggml_repeat` → gated step → gated-RMSNorm →
  out-proj), MoE + bramkowany shared expert. Prefill = sekwencyjny scan
  rekurencyjny; decode = 1 token/krok. Ograniczenia jak MoE (KV f16, single-stream).
- **KV-tiering / KVFlash DZIAŁA** (jedyne MoE, które tieruje). Kluczowy fakt
  architektoniczny: z 41 warstw tylko ~10 to ATENCJA (paged KV) — 30 warstw
  DeltaNet trzyma rezydentny stan SSM, który NIGDY nie jest paged/spillowany.
  `TierManager` dostaje listę indeksów warstw atencji (`layer_kinds` filtrowane po
  `Attention`) i pakuje chunki tylko z tych ~10 warstw (kompaktowy indeks warstwy),
  więc ślad KV tieringu jest ~4× mniejszy niż dla modelu czysto-atencyjnego.
  `hybrid_attn_mixer` przyjmuje `AttnSrc`: `Paged` (rezydentny paged KV) albo
  `Staged` (pełny kontekst warstwy strumieniowany ze slabów tieru — te same
  kernele/kolejność, więc tokeny greedy są bit-identyczne z przebiegiem bez tieru).
  Prefill (`prefill_hybrid`) i decode (`step_streamed`) są tier-świadome:
  `tier_ensure_capacity` spilluje najzimniejsze strony atencji przed `kv.grow`, a
  gdy sekwencja ma spilnięte strony, atencja idzie przez staged path. Stan SSM
  advansuje niezmiennie (rezydentny). Dowód: prompt 8k z igłą, `--kv-tier nvme
  --kv-pages 64` (2048 tokenów gorące) → ~6k tokenów KV atencji spilnięte na NVMe,
  igła odzyskana, ids bit-identyczne z przebiegiem full-VRAM bez tieru, VRAM stały.
  Uruchomienie: `forge run qwen36-moe.gguf "…" -n 24 --temp 0 --weights-pool-gb 20
  --ctx 4096 --kvflash --kv-hot-pages 64`.

### qwen35moe — szczegóły rejestru/kerneli

- **Rejestr architektury** jest wpięty (`forge-formats`): detekcja z GGUF,
  reguła warstw hybrydowych (`(idx+1)%full_attention_interval==0` → pełna
  atencja, reszta → Gated-DeltaNet; interval 4 → atencja na warstwach 3,7,…,39
  z 40 warstw trunku), parsowanie `ssm.*` (`conv_kernel=4`, `inner_size=4096`,
  `state_size=128`, `time_step_rank=32`, `group_count=16`), sekcje M-RoPE
  `[11,11,10,0]` (dla pozycji tekstowych M-RoPE redukuje się do NEOX partial
  rotary po pierwszych 64 wymiarach), shared expert (256 ekspertów, top-8 +
  gated shared, `expert_feed_forward_length=512`) i pomijanie głowy MTP
  (warstwa 40). Atencja: `head_dim=256`, bramkowane wyjście Q (`wq` szerokości
  `head_dim*n_heads*2`), per-head QK-norm.
- **Referencja CPU Gated-DeltaNet** (`forge-formats::deltanet`): causal conv1d,
  reguła delta z bramkowaniem (krok autoregresyjny), gated-RMSNorm — numeryczne
  oracle dla kernela/silnika.
- **Kernele Mojo GOTOWE i zwalidowane** (`kernels/mojo/src/deltanet.mojo`,
  hd256 w `attention.mojo`/`prefill.mojo`, partial M-RoPE w `rope.mojo`):
  `deltanet_conv_silu_f16`, `l2norm_heads_f16`, `deltanet_gated_step_f16`,
  `deltanet_gated_rmsnorm_f16`, `deltanet_log_decay_f32`,
  `deltanet_beta_sigmoid_f32`, `attn_decode_f16_hd256`, `attn_prefill_f16_hd256`,
  `rope_neox_partial_f16`. PTX + manifest przebudowane, typowane launchery +
  registry w `forge-kernels` (build + clippy czyste). Test numeryczny vs
  `deltanet.rs` (`test_deltanet.mojo`) przechodzi w tolerancji f16.

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
- MoE: tylko single-stream decode + KV f16 (patrz sekcja MoE). Hybrydy SSM+MoE
  (Qwen3.6 `qwen35moe`) generują E2E, ale ścieżka jest correctness-first
  (~17 tok/s, host round-tripy per warstwa, bez grafu/wsadu — patrz sekcja
  qwen35moe). Target i MTP mają izolowane sloty per sekwencja oraz wspólny paged
  cache MTP. Dwie niespekulacyjne sekwencje targetu używają B2 ze wspólnymi
  batch GEMM FFN i głowy logits; mixery zachowują osobne sloty DeltaNet. Native
  MTP nadal wykonuje lane seryjnie do czasu verifiera `[B,T]` i batchowego draftu.
  B2 wymaga rezydentnego KV oraz obsługiwanych formatów dense FFN i lm_head;
  tiering lub inny niespełniony warunek wybiera seryjny fallback przed zmianą KV.
- `logprobs`/`echo`/`n>1`: obsługiwane tylko na ścieżce non-streaming (streaming
  przy `n>1` lub completions z `echo`/`logprobs` = 400). Streaming chat NIE dokłada
  `logprobs` do delt (parser reorganizuje tekst — token↔delta nie są 1:1). Prompt-token
  logprobs przy `echo` mają `null` (pozycje prefill nie są punktowane osobno).
