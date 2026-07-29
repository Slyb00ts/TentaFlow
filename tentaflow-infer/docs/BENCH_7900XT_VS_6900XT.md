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
