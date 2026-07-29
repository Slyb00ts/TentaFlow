# Dwie karty AMD, dwa modele, dwa silniki — pomiar

> **UWAGA HISTORYCZNA.** Liczby FORGE w tabelach niżej powstały PRZED naprawą
> RoPE (commit `e7c4d7c7`) i opisują silnik, który dla rodziny Llama liczył złą
> rotację pozycyjną. Wydajność się przez to nie zmieniła (poprawka działa raz
> przy ładowaniu wag), ale wyniki modeli owszem. Aktualne pomiary Bielika są w
> sekcji „Po naprawie RoPE".

Data: 2026-07-28. Karty: RX 6900 XT (gfx1030) i RX 7900 XT (gfx1100) w jednej
maszynie. llama.cpp `ff067f76` zbudowany na OBIE architektury
(`-DGPU_TARGETS=gfx1030;gfx1100`), FORGE z tego drzewa.

Oba silniki czytają **ten sam plik GGUF**, więc różnice są różnicami silnika,
nie kwantyzacji.

## Gemma 4 12B QAT (`gemma-4-12B-it-qat-UD-Q4_K_XL`, 6,24 GiB)

Mimo nazwy pliku dominującym formatem tensorów jest **Q4_0**, nie Q4_K —
llama.cpp też raportuje ten model jako `Q4_0`. Ścieżka Q4_K go nie dotyczy.

| pomiar | 6900 XT | 7900 XT | 7900/6900 |
|---|--:|--:|--:|
| llama.cpp pp512 | 1490,2 tok/s | 2017,1 tok/s | **1,35x** |
| FORGE prefill | 1323,3 tok/s | 1260,8 tok/s | **0,95x** |
| llama.cpp tg128 | 53,95 tok/s | 72,21 tok/s | 1,34x |
| FORGE decode | 54,2 tok/s | 75,9 tok/s | 1,40x |

## Bielik Minitron 7B v3.0 (`Q4_K_M`, 4,19 GiB)

| pomiar | 6900 XT | 7900 XT | 7900/6900 |
|---|--:|--:|--:|
| llama.cpp pp512 | 1597,7 tok/s | 3002,6 tok/s | **1,88x** |
| FORGE prefill | 1647,8 tok/s | 1598,9 tok/s | **0,97x** |
| llama.cpp tg128 | 81,29 tok/s | 101,59 tok/s | 1,25x |
| FORGE decode | 75,6 tok/s | 92,8 tok/s | 1,23x |

## Co z tego wynika

**1. Stosunek mocy tych kart zależy od formatu wag, nie tylko od rodzaju pracy.**
Wcześniejsza kalibracja mierzyła zawsze NVFP4 i dawała 1 : 8,8 na korzyść
7900 XT. Dla Q4_K te SAME karty dają 0,95 : 1 — odwrotnie. Oba pomiary są
poprawne i opisują różne ścieżki: NVFP4 ma na 7900 XT kernel WMMA, a 6900 XT bez
jednostki macierzowej kończy się na wsadzie T=16; Q4_K nie ma kernela WMMA w
ogóle, więc obie karty liczą go na `dot4`, gdzie szybsza jest 6900 XT.

Kalibracja przyjmuje teraz format wag argumentem. Zmierzone tym samym probem:

| format | 6900 XT | 7900 XT | podział prefillu |
|---|--:|--:|---|
| NVFP4 | 1,1 TOPS | 8,8 TOPS | 11,3% / 88,7% |
| Q4_K | 19,4 TOPS | 8,5 TOPS | 69,5% / 30,5% |

Podział Q4_K (69,5% dla 6900 XT) zgadza się co do kierunku z pomiarem
end-to-end, gdzie 6900 XT robi prefill szybciej. Kalibracja przewiduje
rzeczywistość.

**2. FORGE przegrywa prefill na 7900 XT, bo Q4_K nie ma tam kernela WMMA.**
Jednostka macierzowa RDNA3 stoi bezczynnie: `gemm_q4_k` schodzi na `dot4`, a
llama.cpp liczy Q4_K na WMMA. Stąd 1261 wobec 2017 tok/s (Gemma) i 1599 wobec
3003 (Bielik). Na 6900 XT, gdzie WMMA nie ma i tak, FORGE jest równorzędny
(89% i 103% wyniku llama.cpp). Dekodowanie FORGE trzyma poziom: 100-105% na
Gemmie, 91-93% na Bieliku.

## Pokrycie kwantyzacji — audyt

Sprawdzone wykonawczo na `qwen3-0.6b` w jedenastu kwantyzacjach, obie karty.
Na AMD startuje **wyłącznie Q8_0 i Q4_K**. Reszta kończy się błędem
`kernel not loaded: gemm_<format>_f16_bm64` — IDENTYCZNIE na obu kartach, więc
to nie jest regresja 7900 XT.

Artefakty GEMM w katalogu (liczba plików):

| format | gfx1030 | gfx1100 | NVIDIA |
|---|--:|--:|--:|
| q8_0 | 19 | 24 | 56 |
| nvfp4 | 17 | 19 | 98 |
| q4_k | 4 | 4 | 14 |
| q6_k | 3 | 3 | 7 |
| q4_0 | 4 | 4 | 8 |
| q4_1, q5_0, q5_1, q2_k, q3_k, q5_k | 0 | 0 | 4 każdy |
| iq1_s, iq2_xxs, iq2_xs, iq3_xxs, iq4_nl, iq4_xs, mxfp4 | 0 | 0 | 4 każdy |

### Przyczyna, ustalona kompilacją a nie domysłem

Próba zbudowania `gemm_q5_k_f16` na gfx1100 kończy się:

```
constraint failed: no valid implementation of mma for
a=8xfloat16, b=4xfloat16, c=4xfloat32, and d=4xfloat32
```

Wszystkie 20 ciał `gemm_*_impl` w `src/gemm.mojo` woła `mma()` z fragmentem
NVIDIA `m16n8k16` (8 połówek na linię dla A, 4 dla B). WMMA na RDNA3 ma inny
fragment: `16x16x16`, 16 połówek na linię i akumulator 8xf32. To nie jest różnica
jednej instrukcji, tylko innego układu danych na linię — dotyczy ładowania
aktywacji, wag i zapisu wyniku.

Markery `# arch: nvidia` przy tych wpisach dodałem 2026-07-27 razem z
mechanizmem zakresowania katalogu. Nie one są przyczyną: artefaktów AMD dla tych
formatów nie było NIGDY (`git log` na plikach `build/gfx1100/gemm_q5_k_f16.hsaco`
— zero commitów). Markery ZAPISAŁY istniejącą lukę, nie stworzyły jej.

### Czego to wymaga

Q8_0 i NVFP4 mają już warianty WMMA (`src/gemm_wmma.mojo`,
`src/nvfp4_gguf_wmma.mojo`) zrobione dokładnie tą drogą: dekwantyzacja zostaje,
wymieniony jest fragment, mnożenie i zapis. Pozostałe formaty potrzebują tego
samego szkieletu — jednego wspólnego kafla WMMA sparametryzowanego funkcją
„rozpakuj 16 kolumn wiersza", do którego każdy format wnosi tylko własne
rozpakowanie. Rozpakowania te już istnieją w odpowiednich `gemm_*_impl`.

Odblokowuje to trzynaście formatów: q4_1, q5_0, q5_1, q2_k, q3_k, q5_k, iq1_s,
iq2_xxs, iq2_xs, iq3_xxs, iq4_nl, iq4_xs, mxfp4. Ten sam kafel daje przy okazji
Q4_K ścieżkę WMMA na 7900 XT, czyli zamyka lukę prefillu z punktu 2.

## Zastrzeżenia

- SHA wygenerowanych tokenów RÓŻNI się między kartami dla obu modeli Q4_K
  (Gemma `65f4387c` vs `74a1e0e4`, Bielik `2fd23100` vs `88430414`). Dla Qwen 27B
  NVFP4 SHA były identyczne. Q4_K liczy się innymi kernelami na obu
  architekturach, więc różnica ostatnich bitów jest oczekiwana, ale nie została
  zbadana pod kątem jakości wyjścia.
- FORGE mierzony z `--ctx 4096`; bez tego dobiera pule z pełnego kontekstu
  Gemmy i żąda 105 GB VRAM.
- llama.cpp `pp512`/`tg128` to jego własny protokół pomiaru; liczby FORGE to
  mediana z 5 powtórzeń po rozgrzewce. Zgodność protokołów nie była wymuszana.

## Poprawność — znaleziony i naprawiony błąd RoPE

Sprawdzenie wyjścia po pomiarach pokazało, że FORGE odpowiada inaczej niż
llama.cpp. Porównanie warstwa po warstwie z `llama-eval-callback` (ślad
`FORGE_LAYER_TRACE=1`) wskazało dokładne miejsce: embedding, norma i projekcja Q
zgadzały się co do elementu, ale wartości PO RoPE już nie.

Przyczyna: silnik wszędzie stosował RoPE w stylu NeoX (pary `(i, i + d/2)`),
podczas gdy rodzina Llama wymaga rotacji przeplatanej (pary `(2i, 2i+1)`).
Policzona ręcznie rotacja przeplatana z wartości sprzed RoPE dała dokładnie
liczbę z llama.cpp. Błąd nie ruszał pozycji zerowej (kąt = 0), więc model
odpowiadał poprawnie na prompt jednotokenowy i rozjeżdżał się od drugiego —
i dlatego długo wyglądał jak problem konkretnego modelu.

Poprawka przestawia raz przy ładowaniu wiersze Q i K w kolejność
`[0, 2, 4, …, 1, 3, 5, …]`; istniejący kernel NeoX liczy wtedy rotację
przeplataną, a `Q·K` jest niewrażliwe na wspólną permutację wymiarów.

| model | perplexity przed | po |
|---|--:|--:|
| Bielik Minitron 7B | 169,96 | **5,98** |
| Mistral 7B | 30,13 | **4,96** |

Dotyczyło to KAŻDEGO modelu rodziny Llama i Mistral, na NVIDII tak samo jak na
AMD. Qwen, Gemma i DeepSeek używają NeoX i były poprawne.

## Po naprawie RoPE — Bielik Q4_K, prefill 512 / decode 128

| | 6900 XT | 7900 XT |
|---|--:|--:|
| prefill | 1648,8 tok/s | **2408,9 tok/s** |
| decode | 75,6 tok/s | 92,1 tok/s |

Wydajność względem pomiarów sprzed poprawki jest niezmieniona — permutacja
wykonuje się raz przy ładowaniu wag.

## Kernel WMMA dla Q4_K — dodany i zweryfikowany

`kernels/mojo/src/gemm_q4_k_wmma.mojo`: kafelkowany GEMM czytający surowe
superbloki GGML Q4_K 144 B na jednostkach macierzowych RDNA3, zakres
`amd:gfx11+`. Wcześniej Q4_K schodził na AMD na `dot4` i jednostka macierzowa
7900 XT stała bezczynnie.

Weryfikacja:
- **Golden test** (`kernels/mojo/tests_amd_q4k_wmma.mojo`) wobec referencji
  liczonej na hoście tą samą formułą co `_dequant_q4k`, cztery kształty łącznie
  z niepełnymi kafelkami (`rows=70, cols=768, T=17`): największa różnica 0,48
  przy referencji rzędu 1000, czyli dokładnie ziarno zaokrąglenia wyjścia f16.
- **End-to-end**: Mistral 7B Q4_K, prompt 409 tokenów (prefill przez WMMA), tekst
  spójny i zgodny z wariantem `dot4` na drugiej karcie.

Zysk na Bieliku Q4_K, 7900 XT, prefill 512 tokenów:

| | przed | po | zmiana |
|---|--:|--:|--:|
| FORGE prefill | 1598,9 tok/s | **2374,4 tok/s** | **+48,5%** |
| llama.cpp prefill | 3002,6 tok/s | 3002,6 tok/s | — |
| udział llama.cpp | 53% | **79%** | |

6900 XT bez zmian (1647,8 → 1662,2), poprawnie: RDNA2 nie ma WMMA i zostaje na
`dot4`. Dekodowanie bez zmian (92,8 → 92,6), bo idzie przez GEMV.

Zastrzeżenie: ten zysk dotyczy szybkości ścieżki liczenia. Dla samego Bielika
wynik końcowy pozostaje błędny z powodu opisanego wyżej, niezależnego błędu.

## Audyt warunków „tylko NVIDIA" — 2026-07-29

Zestaw testów FORGE nigdy wcześniej nie wykonał się na Radeonach: `forge-engine`,
`forge-server`, `forge-whisper` i `forge-onnx` nie miały flagi wyboru backendu, a
32 pliki testów i przykładów tworzyły `CudaDevice` wprost. Skutek uboczny — dziewięć
plików testowych `forge-server` nie kompilowało się od czasu dodania pola
`layer_range`, bo nikt ich nie budował.

Po przestawieniu wszystkiego na `gpu::open` zestaw przechodzi w całości:
**724 testy, 0 porażek, 20 pominiętych**.

Odsłoniło to jeden powtarzający się błąd: kod pytał `vendor == Nvidia` tam, gdzie
chodziło o konkretną zdolność sprzętową. RDNA ma falę 32 i instrukcję `dot4` na
int8, więc warunek producenta wyłączał ścieżki, dla których kernele były
ZBUDOWANE i poprawne.

| miejsce | było | jest |
|---|---|---|
| `dense_prefill_backend_capable` | tylko NVIDIA | fala 32 + blok ≥256 |
| uwaga segmentowana HD128 | wyłącznie kernel FA (`mma`) | FA gdy jest, inaczej przenośny kafel |
| głowa logitów B16 Q8_0 | wyłącznie kernel NVIDII | dodatkowo kafle WMMA / `dot4` |
| `attn_verify_segmented_*_warp32` | tylko NVIDIA | każda karta z falą 32 |
| `gemm_qk_dp4a_batch_at` | tylko NVIDIA | fala 32 (`v_dot4_i32_i8`) |
| `dense_prefill_auto_backend_capable` (serwer) | tylko NVIDIA | fala 32 + blok ≥256 |

Batchowy dp4a Q4_K/Q6_K policzył na RX 7900 XT T=2/4/8/16 zgodnie z referencją
dekwantyzacji CPU; wcześniej test cicho się pomijał.

Osobno naprawiony rozjazd semantyki w HAL, zamaskowany komentarzem twierdzącym,
że jest tak samo jak w CUDA:

| | CUDA | HIP (było) |
|---|---|---|
| tryb przechwytywania grafu | THREAD_LOCAL | GLOBAL |
| strumienie | NonBlocking | blokujące |

Tryb globalny unieważnia przechwytywanie przy każdej ryzykownej operacji w CAŁYM
procesie, także w innym wątku, który o grafie nic nie wie — stąd błędy 906/901
tam, gdzie ta sama praca na NVIDII przechodziła.

### Warunki producenta, które zostają

- **Wybór zestawu wbudowanego** (`select_embedded_set`) — zgodność w przód PTX
  jest własnością CUDA; `hsaco` wymaga dokładnego ISA.
- **`fused_decode_available`** — decyzja z POMIARU, nie z ostrożności: kernele
  `gemv_norm_*` przeliczają normę w każdej grupie roboczej i na gfx1030 dają
  181 GB/s wobec 466 GB/s zwykłego GEMV. Rozdzielenie normy i GEMV dało tam
  67,2 → 78,6 tok/s.
- **NVFP4 CT** (repack/decode/prefill/TileN128K64) — ścieżka formatu
  compressed-tensors pisana pod instrukcje NVIDII.
- **Rollouty eksperymentalne** (hybrydowy prefill B2, MTP+n-gram) — bramki
  wdrożeniowe, nie zdolności sprzętu.

## Naprawiona usterka: host wyprzedzał GPU i nadpisywał przypięte bufory

Qwen3.6-27B (`qwen35`, DeltaNet) na RX 7900 XT wywalał się w prefillu:

```
Memory access fault by GPU node-2 ... Reason: Page not present or supervisor privilege.
```

### Jak to znaleziono

Kolejne obserwacje zawężały problem, aż wskazały jedno miejsce:

1. **Próg to 32 tokeny** — tyle mieści jedna strona KV. Krótszy prompt przechodził.
2. **Niedeterministycznie**, około 2 na 3 uruchomienia, i niezależnie od trasy
   prefillu (layer-major, batched, sekwencyjna — wszystkie padały tak samo).
3. **`AMD_SERIALIZE_KERNEL=3` usuwał błąd.** To wyklucza zły indeks w kernelu i
   wskazuje na kolejność.
4. **Adres błędu leżał 48 KiB przed początkiem puli KV** — czyli kernel policzył
   adres z indeksu strony `-1`, wartownika wpisów niewypełnionych.
5. **Zastąpienie wartownika `-1` powtórzoną ważną stroną usuwało błąd** — dowód,
   że kernel czyta wpis spoza ważnego zakresu.
6. Kernel czyta jednak `position < seq_lens[seq]`, więc zakres był poprawny —
   chyba że `seq_len` na urządzeniu WYPRZEDZAŁ tablicę stron.

### Przyczyna

Wejścia jednego tokenu (`token_id`, `pos`, `seq_len`), tablica stron i wiersz
embeddingu szły przez **pojedyncze** przypięte bufory pośrednie, z których kopie
na urządzenie są ASYNCHRONICZNE. Host nadpisywał te bufory dla kolejnego tokenu,
zanim poprzednia kopia zdążyła się wykonać, więc na urządzenie trafiały dane
z przyszłości: `seq_len` rósł szybciej niż tablica stron, a kernel sięgał po
stronę `-1`.

Dekodowanie tego nie ujawniało, bo synchronizuje się co token po logity —
host nie ma jak wyprzedzić GPU. Prefill nie synchronizuje się aż do ostatniego
tokenu i wyprzedza o setki kroków.

### Naprawa

Przypięte bufory pośrednie mają pierścień 64 slotów i zdarzenie na każdy z nich;
slot wolno nadpisać dopiero po potwierdzeniu, że jego kopia dotarła. Ten sam
wzorzec był już w kodzie dla prefillu layer-major
(`HYBRID_HOST_STAGING_SLOTS`) — brakowało go w ścieżce per token.

### Drugi wyścig: wiersz embeddingu

Po naprawie awaria zniknęła, ale model odpowiadał bełkotem (`" to to to to"`).
Ta sama przyczyna, inny bufor: `pinned_embed` też był pojedynczy, więc token
dostawał embedding następnego. Rozstrzygnął test porównawczy — z
`AMD_SERIALIZE_KERNEL=3` odpowiedź była poprawna, bez niego bełkot. Po objęciu
embeddingu tym samym pierścieniem oba wyjścia są identyczne.

### Wynik

```
$ forge run qwen36-27b-Q4_K_M.gguf "Stolica Polski to" --max-tokens 16 --temp 0 --no-chat
 miasto, które w ciągu ostatnich lat bardzo się rozwinęło. W
```

Prompt 170 tokenów przechodzi 5/5, 28 tok/s. Modele gęste bez zmian
(Bielik 7B 121,8 tok/s, Bielik 11B 83,9, Gemma 12B QAT 105,2, Nemo 12B 55,9).
Cały zestaw testów: 724 przechodzi, 0 porażek.

## Persistent scan DeltaNet — zweryfikowany na AMD, bez zysku

Po naprawie wyścigów dało się wreszcie sprawdzić bramkę
`supports_deltanet_gated_scan_persistent_d128_f16`, która wymagała NVIDII.
Qwen3.6-27B, prompt 582 tokeny, RX 7900 XT:

| tryb | czas | wyjście |
|---|---|---|
| `chunked` | 21,07 s | — |
| `persistent` | 20,98 s | **identyczne co do bajtu** |

Zgodność wyjścia potwierdza poprawność, więc bramka pyta teraz o falę 32, a nie
o producenta. Zysku prędkości na RDNA3 NIE MA (0,4%, w granicach szumu) — w
odróżnieniu od NVIDII, gdzie dokumentacja podaje 2,60-2,63x na samym skanie.

## Hybrydowy prefill na AMD: to format checkpointu, nie producent

Qwen3.6-27B na RX 7900 XT, `forge bench --prompt-tokens 2048 --tokens 32`,
mediana z 5 powtórzeń:

| checkpoint | prefill 2048 | decode |
|---|---|---|
| Q4_K_M | 72 514 ms → **28,2 tok/s** | 27,3 tok/s |
| NVFP4 (ThinkingCap …-NVFP4-MTP) | 2 657 ms → **770,5 tok/s** | 32,3 tok/s |

**27x różnicy na prefillu robi sam format wag**, nie karta. Q4_K schodzi na
prefill token po tokenie (prefill w tempie dekodowania), NVFP4 wchodzi na
ścieżkę layer-major.

Dlaczego — `hybrid_prefill_extended_structural_capable` wymaga, żeby KAŻDA waga
FFN była `DevWeight::NvFp4Gguf`. Dla Q4_K_M jest to fałsz na dowolnej karcie,
więc na NVIDII ten sam plik też zszedłby na prefill po tokenie. Backend AMD nie
jest tu blokowany: `hybrid_prefill_t128_backend_capable` przyjmuje
`Nvidia | Amd` z falą 32, a wszystkie artefakty `HYBRID_PREFILL_T128_SHARED`,
`HYBRID_PREFILL_T128_MATRIX_AMD` i triplet `gemm_q8_0_wmma_triplet_bm64` są
zbudowane dla gfx1100.

Odniesienie z `CLAUDE.md` dla tego samego checkpointu NVFP4 na RTX 4090 to
2498,5 tok/s prefillu — RX 7900 XT osiąga **31% tego wyniku**. To jest realny
stosunek tych kart, a nie awaria ścieżki.

Model odpowiada poprawnie:

```
$ forge run ThinkingCap-Qwen3.6-27B-NVFP4-MTP.gguf "Stolica Polski to" --no-chat
 Warszawa. To miasto, które jest największym ośrodkiem politycznym
```

Wniosek praktyczny: dla modeli hybrydowych na AMD używać checkpointów NVFP4.
Dorobienie wariantów Q4_K kerneli layer-major zamknęłoby lukę dla pozostałych
kwantyzacji, ale jest osobną, dużą pracą.

## Profil prefillu NVFP4 na RX 7900 XT

`rocprofv3 --kernel-trace` na prefillu 2048 tokenów (suma czasu kerneli 5278 ms):

| kernel | udział | wywołań |
|---|---|---|
| `nvfp4_gguf_wmma_gemm` | 66,1% | 608 |
| `attn_decode_batch_exact_f16_hd256` | 13,0% | 32 |
| `gemm_q8_0_wmma_triplet` + `i8mma` | 12,6% | 192 |
| `deltanet_value_key` | 4,6% | 96 |

Drugi wpis był błędem doboru: bez artefaktu flash-attention layer-major schodził
na `Exact`, czyli liczył CAŁY chunk kernelem dekodowania — 21 ms na wywołanie.
Wariant prefillowy `attn_prefill_device_pos_f16_hd256` jest zbudowany dla
gfx1100 i nikt do niego nie prowadził. Po zmianie zejścia awaryjnego:

| | prefill 2048 |
|---|---|
| zejście na `Exact` | 770,5 tok/s |
| zejście na `Prefill` | **836,3 tok/s** (+8,5%) |

Wyjście identyczne co do bajtu z obydwoma wariantami.

Następny cel to same GEMM-y NVFP4 — 66% czasu, około 40% szczytu WMMA tej karty.

## Q4_K na modelu hybrydowym: 28,4 → 724,9 tok/s

Pytanie „a co z Q4_K" odsłoniło dwie usterki bramkowania. Q4_K na modelach
GESTYCH byl caly czas w porzadku (Bielik 7B 1474,8 tok/s prefillu, Gemma 12B
1168,8) — zalamywal sie wylacznie na hybrydowym.

### 1. Batchowy prefill bramkowany formatem glowy verifiera

`prefill_hybrid` wybieral sciezke batchowa przez `validate_hybrid_speculation_target()`,
ktore wymaga glowy logitow F16 albo Q8_0. GGUF Q4_K_M ma z konwencji llama.cpp
glowe **Q6_K**, wiec KAZDY hybrydowy model Q4_K_M schodzil na prefill token po
tokenie — na dowolnej karcie, takze NVIDII.

Wymog F16/Q8_0 nalezy do verifiera, ktory liczy logity dla T pozycji naraz.
Prefill zwraca logity wylacznie ostatniego tokenu zwykla sciezka `logits_gemv`,
obslugujaca kazdy format. Rozdzielone: batchowy prefill ma wlasny predykat
`hybrid_batched_prefill_capable()`. Efekt: **28,4 → 198,3 tok/s**.

Przy okazji profilowanie w `bench` powielalo decyzje o trasie i pytalo starego
walidatora — rezerwowalo spany na inna sciezke, niz sie wykonywala, i przerywalo
pomiar bledem. Oba miejsca uzywaja teraz tego samego predykatu.

### 2. Chunk zabetonowany na 32

Dla modeli bez NVFP4 `resolve_hybrid_prefill_chunk_size` zwracalo SZTYWNE 32.
Wieksze chunki dzialaja i sa wyraznie szybsze — Qwen3.6-27B Q4_K_M, prefill 2048
na RX 7900 XT:

| chunk | prefill |
|---|---|
| 32 | 198,3 tok/s |
| 128 | 440,4 |
| 256 | 677,7 |
| 512 | 720,5 |
| 1024 | **726,3** |

Granice stawia budzet puli aktywacji, nie format wag, wiec chunk dobiera sie
teraz drabinka `[1024, 512, 256, 128, 32]` ograniczona ta sama miara scratcha co
sciezka NVFP4. Automat wybiera 1024.

### Wynik

| model | przed | po |
|---|---|---|
| Qwen3.6-27B Q4_K_M | 28,4 tok/s | **724,9 tok/s** (25,5x) |
| Qwen3.6-27B NVFP4 | 838,6 tok/s | 838,6 (bez zmian) |

Wyjscie identyczne co do bajtu z referencja token-po-tokenie. Q4_K jest teraz na
86% wyniku NVFP4 zamiast 3%.

## RDNA4 (Radeon AI PRO R9700, gfx1201) — jednostka macierzowa

Mojo nie umie wygenerowac WMMA dla gfx12: `Cannot select: intrinsic
llvm.amdgcn.wmma.f32.16x16x16.f16`. Powod jest konkretny — RDNA4 ma WLASNE
warianty tych intrinsikow, o polowe mniejsze fragmenty na linie:

| operand | RDNA3 (gfx11) | RDNA4 (gfx12) |
|---|---|---|
| A/B f16 | `v16f16` (caly wiersz, dublowany miedzy polowami fali) | `v8f16` |
| A/B iu8 | `v4i32` | `v2i32` |
| akumulator | `v8f32` | `v8f32`, ale INNY uklad |

Uklad akumulatora zmierzylem sonda na karcie, a nie przyjalem z dokumentacji:
RDNA3 przeplata wiersze co drugi (`i*2 + polowa fali`), RDNA4 daje kazdej
polowie fali osiem KOLEJNYCH wierszy (`8*polowa + i`); kolumna w obu to
`lane % 16`. Pierwsza wersja zakladala uklad RDNA3 i test zloty pokazal blad
wzgledny 42 — stad sonda zamiast kolejnego zgadywania. Roznica siedzi teraz w
`arch_wmma.mojo` (`wmma_acc_row`), a kernele jej nie widza.

Wszystkie testy zlote AMD przechodza na gfx1201 z bledem na poziomie
kwantyzacji (4,8e-4 dla Q8_0, ulamek jednostki dla Q2_K–Q5_K).

**Wynik jest MIESZANY i to jest istotne** (Bielik 7B, prompt 2048, jedna R9700):

| format | prefill bez WMMA | prefill z WMMA | zmiana |
|---|---|---|---|
| Q4_K_M | 1575 tok/s | **2279 tok/s** | **+45%** |
| Q8_0 | 2047 tok/s | 1838 tok/s | **-10%** |

Dla Q8_0 przenosna sciezka `dot4` na RDNA4 jest wiec SZYBSZA od jednostki
macierzowej. Czesc straty zabralo czytanie fragmentow: kernel ladowal 16 bajtow
na linie (wymog RDNA3), z czego RDNA4 potrzebuje 8 — po zejsciu do 8 bajtow
(`_row_frag_i8`, parametr `preselected` prymitywu) Q8_0 wrocilo z 1777 do 1838
tok/s, ale nie do 2047.

Nastepny krok jest wiec dispatch, nie kolejny kernel: wybor sciezki dla Q8_0 na
gfx12 musi isc za POMIAREM, tak jak juz idzie wybor ukladu NVFP4. Do tego czasu
Q8_0 prefill jest o 10% wolniejszy niz przed wlaczeniem WMMA — swiadomie
zapisane, bo Q4_K zyskalo w tym samym ruchu 45%.

Decode nie zmienil sie w zadnym z formatow (65,0 i 89,0 tok/s): to sciezka
GEMV, ktorej jednostka macierzowa nie dotyka.
