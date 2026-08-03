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

### Etap 2 — kwantyzacja jako trait
`trait Quant { fn pack(..); fn permute_rope_rows(..); fn gemm_for(&Problem) -> KernelChoice; }`
z implementacjami: `nvfp4_ct`, `nvfp4_gguf`, `fp8`, `q4k`, `q8_0`, …

Likwiduje klasę błędów, której przykładem była permutacja RoPE: format sam
odpowiada, czy i jak go permutować, zamiast być wyliczany w cudzym `match`.

Warunek dodatkowy: test regresyjny na permutację per format.

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
