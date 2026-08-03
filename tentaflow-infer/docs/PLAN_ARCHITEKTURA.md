# Plan uporządkowania architektury FORGE

Cel: **jak najwięcej wspólnego kodu tam, gdzie rzecz jest algorytmiczna, i czysty
podział tam, gdzie jest sprzętowa** — tak, żeby dodanie karty, modelu albo
kwantyzacji było dopisaniem jednego modułu, a nie poprawką w dwudziestu
miejscach.

To NIE jest dążenie do jednego kernela na wszystko. Projekt istnieje po to, by
optymalizować pod konkretny sprzęt; chodzi o to, żeby te optymalizacje dało się
dokładać bez przepisywania reszty.

## Stan wyjściowy (2026-08-03)

| plik | linie |
|---|---|
| `forge-engine/src/model.rs` | 21 457 |
| `forge-kernels/src/launchers.rs` | 20 515 |
| pozostałe ~100 plików Rusta | 64 500 |

Dwa pliki to 40% kodu Rusta. Apple ma osobną implementację (`msl.rs`, 1454
linie) niepowiązaną z resztą; NVIDIA i AMD dzielą jedno źródło Mojo bez
rozgałęzień sprzętowych.

### Dowód, że to kosztuje — trzy usterki z jednego dnia

- **Permutacja RoPE**: jedna funkcja wyliczała wszystkie 20+ formatów wag w
  `match`; NVFP4 compressed-tensors wpadł w gałąź „nieobsługiwane", a po
  dopisaniu go — permutował wagi, które HF już spermutował. Model ładował się,
  liczył szybko i generował śmieci.
- **Podział szerokiego N**: łatka w jednym z dwóch punktów wejścia GEMM nie
  zmieniła pomiaru. Drugi punkt był 230 linii dalej w tym samym pliku.
- **K/V w paczkach FP8**: wymagało zmian w trzech miejscach; czwarte
  (kwantyzacja aktywacji) przeoczone, wykryte dopiero profilem `nsys`.

## Punkt odniesienia — warunek zaliczenia KAŻDEGO etapu

Bielik-PL-Minitron-7B-NVFP4, jeden DGX Spark, `--weights-pool-gb 24`:

| miara | mediana | zakres z 3 przebiegów |
|---|---|---|
| prefill @2048 | **4 784 tok/s** | 4 763 – 4 805 |
| prefill @4096 | **4 092 tok/s** | — |
| decode | **38,3 tok/s** | — |
| generacja | `Stolicą Polski jest Warszawa. Warszawa jest największym miastem…` | |

Po każdym etapie: **mediana nie spada poza zakres szumu** i **generacja pozostaje
poprawna**. Bramka MUSI być medianą z kilku przebiegów — pierwotnie ustawiłem ją
na pojedynczym odczycie 4 811, czyli na górnej krawędzi rozrzutu, co kazałoby
odrzucić zmianę neutralną wydajnościowo.
Sam benchmark nie wystarcza — identyczne SHA tokenów dowodzą powtarzalności, a
nie poprawności. Tego nauczył nas błąd z RoPE.

## Etapy

### Etap 1 — projekcje sprowadzone do jednego typu ✅ ZROBIONE
Dziś iloczyn: format wag (W4A8 / pełny FP8 / hybryda FP8 / natywny NVFP4) ×
układ (`Fused` / `FusedQk` / `Split`) = 12 kombinacji rozpisanych ręcznie w
kilku miejscach wywołania.

Docelowo `Projection { Fp8 | W4A8 | Nvfp4Rows{w,off,rows} }` rozstrzygane RAZ przy
ładowaniu, plus jedna metoda `project(out, proj, x, rows, stream)`. Wykonanie
warstwy to trzy wywołania bez rozgałęzień.

Zysk: dodanie formatu przestaje wymagać dopisywania go w każdym miejscu wywołania.

### Zasada: kwantyzację liczymy WPROST, konwersja jest wyjątkiem

Format kwantyzacji istnieje po to, żeby liczyć na nim bezpośrednio — mniej
bajtów przez HBM i jedna kopia wag. Konwersja jest dopuszczalna TYLKO gdy sprzęt
albo kernel nie obsługuje formatu, i wtedy musi mieć zapisane uzasadnienie
pomiarowe.

FORGE stosuje tę zasadę niemal wszędzie: `q2_k`, `q3_k`, `q4_0`, `q4_1`, `q4_k`,
`q5_0`, `q5_1`, `q6_k`, `q8_0`, `iq1_m`, `iq1_s`, `iq2_s`, `iq2_xs`, `iq2_xxs`,
`iq3_s`, `iq3_xxs`, `iq4_nl`, `iq4_xs`, `mxfp4`, `nvfp4` — każdy ma własne
kernele.

**Wyjątkiem jest NVFP4 compressed-tensors w prefillu.** Ma 22 własne kernele, a
mimo to przepakowujemy go do FP8 przy ładowaniu. Zmierzone na Bieliku 7B:

| ścieżka | prefill | decode | dodatkowa pamięć |
|---|---|---|---|
| paczki FP8 | 4 899 tok/s | 38,4 | **+7,35 GB** |
| NVFP4 wprost | 2 064 tok/s | 38,2 | 0 |

Czyli konwersja kupuje 2,4x w prefillu kosztem 7,35 GB zdublowanych wag, a
dekodowaniu nie daje nic (obie ścieżki czytają NVFP4). To NIE jest brak
wsparcia — to słabszy kernel, i naprawa należy do kernela, nie do formatu.

Dla porównania vLLM nie konwertuje wcale: ma rodzinę kerneli NVFP4
(`CutlassNvFp4`, `MarlinNvFp4`, `FlashInferCuteDslNvFp4`, `FbgemmNvFp4`,
`HummingNvFp4`, `EmulationNvFp4`) wybieranych po zdolnościach karty.

Wniosek dla `trait Quant`: `gemm_for(&Problem)` musi domyślnie wskazywać kernel
NA FORMACIE, a konwersja ma być jawnym, uzasadnionym wariantem — nie domyślnym
zachowaniem, które łatwo przeoczyć.

### Etap 2 — kwantyzacja jako trait ✅ ZROBIONE (pierwszy wycinek)
`trait Quant { fn pack(..); fn permute_rope_rows(..); fn gemm_for(&Problem) -> KernelChoice; }`
z implementacjami: `nvfp4_ct`, `nvfp4_gguf`, `fp8`, `q4k`, `q8_0`, …

Likwiduje klasę błędów, której przykładem była permutacja RoPE: format sam
odpowiada, czy i jak go permutować, zamiast być wyliczany w cudzym `match`.

Warunek dodatkowy: test regresyjny na permutację per format.

Zrobione: `HostWeight::row_views_mut` — każdy format deklaruje własny układ
wierszowy (bufory + krok), a `permute_rope_pairs` przechodzi po widokach zamiast
wyliczać dwadzieścia wariantów. NVFP4 CT przestaje być przypadkiem szczególnym:
dwa bufory z krokami `cols/2` i `cols/16` to zwykły wpis. Dwa testy regresyjne
pilnują, że oba bufory są permutowane KAŻDY SWOIM krokiem — pomylenie ich było
realnym ryzykiem przy ręcznym dopisywaniu.

Do zrobienia w tym etapie: `gemm_for(&Problem)` i `pack` przeniesione na ten sam
kontrakt.

### Etap 3 — podział `launchers.rs` i wspólny rejestr wariantów
20 515 linii → `launchers/{gemm, attention, norm, quant, sample}.rs`.

Rejestr wariantów uogólniony z tego, co drugi agent zrobił dla Apple:
```rust
Variant { name, applies: fn(&Problem) -> bool, because: "zmierzone uzasadnienie" }
```
Dziś istnieje tylko pod `cfg(metal, macos)`; ma objąć CUDA i ROCm. Wybór kernela
staje się deklaratywny i **udokumentowany pomiarem**, zamiast rozsypanych `if`-ów.

### Etap 4 — podział `model.rs` po architekturach
21 457 linii → `model/{loader, prefill, decode}.rs` +
`model/arch/{llama, qwen35, deepseek_v4}.rs`.

Dziś wszystkie architektury żyją w jednym pliku, rozróżniane przez
`is_hybrid()`, `is_moe()` i podobne. Dodanie modelu ma być nowym plikiem.

### Etap 5 — HAL: zdolności zamiast domysłów
`Backend` z jawnym odpytaniem o zdolności (`has_fp4_block_scale`, `has_wgmma`,
`has_tmem`, `warp_size`, …), tak by rejestr wariantów wybierał po FAKTACH.

Podstawa jest zweryfikowana sondowaniem `ptxas` na GB10:

| funkcja | sm_121a |
|---|---|
| `mma.sync`, `cp.async` | jest |
| TMA (`cp.async.bulk.tensor`) | jest |
| `mma.kind::mxf4.block_scale.m16n8k64` (FP4 2×) | jest — skale UE8M0 |
| `mma...nvf4` (skale E4M3) | **brak** |
| `wgmma` (rdzeń FA3) | **brak** |
| `tcgen05` (rdzeń FA4) | **brak** |

Dlatego FA4 w całości jest tu niewykonalne, a jego dwie techniki algorytmiczne
(warunkowe przeskalowanie, wykładnicza wielomianowa) — owszem.

## Co ten plan odblokowuje

- **MXFP4** — natywne FP4 z `k64` to dwukrotna przepustowość instrukcji wobec
  FP8. Po Etapie 2 jest to jedna implementacja `Quant`, a nie zmiany w
  kilkunastu miejscach. Wymaga zmierzenia jakości (`forge ppl`), bo skale
  potęgowe są zgrubniejsze niż E4M3.
- **Hopper i Blackwell datacenter** — dziś nie budujemy `sm_90` ani `sm_100`, więc
  FORGE tam nie ruszy. Po Etapach 3 i 5 dołożenie ścieżki `wgmma`/`tcgen05` to
  nowy wariant w rejestrze.
- **Apple** — praca drugiego agenta przestaje być wyspą: ten sam rejestr, te same
  techniki algorytmiczne, osobne tylko kernele.
