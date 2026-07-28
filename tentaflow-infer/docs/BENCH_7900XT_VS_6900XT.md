# Dwie karty AMD, dwa modele, dwa silniki — pomiar

Data: 2026-07-28. Karty: RX 6900 XT (gfx1030) i RX 7900 XT (gfx1100) w jednej
maszynie. llama.cpp `ff067f76` zbudowany na OBIE architektury
(`-DGPU_TARGETS=gfx1030;gfx1100`), FORGE z tego drzewa.

Oba silniki czytają **ten sam plik GGUF**, więc różnice są różnicami silnika,
nie kwantyzacji.

## Gemma 4 12B QAT (`gemma-4-12B-it-qat-UD-Q4_K_XL`, 6,24 GiB)

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
