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
| decode | **38,0 tok/s** | 37,9 – 38,2 |
| generacja | `Stolicą Polski jest Warszawa. Warszawa jest największym miastem…` | |

Wpisane wcześniej 38,3 tok/s dekodowania było odczytem z innego stanu maszyny.
Zmierzone 2026-08-03 na commicie sprzed refaktoru daje 37,9–38,0, a po nim
38,0–38,1 — wartość progu poprawiona na to, co ten sam kod daje dziś. Bramkę
zdaje się TYLKO porównaniem z baseline zmierzonym w tej samej sesji; liczba
przepisana z dokumentu sprzed tygodnia nie jest baseline'em.

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

Zrobione: `DevWeight::row_offset_bytes` — geometrię wiersza podaje format, a nie
miejsce wywołania. Dwadzieścia jeden wyrażeń `row_off * (cols / 256) * 176` i
podobnych zniknęło z `gemm_rows`; każde było kopią tego, co `QuantKind` już
wie. `block_quant` nie ma gałęzi `_ =>`, więc nowy wariant `DevWeight` nie
skompiluje się bez podania geometrii.

Zrobione: wszystkie sześć rodzin GEMV/GEMM czyta z JEDNEJ tabeli formatów
(`model/quant_dispatch.rs`). Osiemnaście formatów blokowych miało w każdej
rodzinie po jednym kernelu i było rozpisane sześć razy; teraz to sześć kolumn
jednego wiersza. Dodanie kwantyzacji do wszystkich ścieżek dekodowania i
batcha to jeden wiersz zamiast sześciu `match`-y, z których łatwo trafić pięć.

| miara | przed | po |
|---|---|---|
| `model/gemm.rs` | 1 976 linii | **613** |
| `model/quant_dispatch.rs` | — | 702 |
| wystąpień `DevWeight::` w `model/` | 283 | **173** |

Formaty różniące się TREŚCIĄ zostają wypisane osobno i zostały przeniesione
dosłownie: Q8_0 wybiera w `gemm_rows` między dp4a, małym batchem i i8mma, Q4_K
i Q6_K mają tam ścieżkę batch dp4a, a w `gemv_out_f32` próg, `NvFp4` rozgałęzia
się po układzie pamięci.

`gemm_rows` dało się zwinąć DOPIERO po `row_offset_bytes` — wcześniej jego
osiemnaście ramion miało szesnaście różnych kształtów, bo każde niosło własną
arytmetykę bajtów.

#### Zmierzony kształt iloczynu (stan przed zwinięciem)

Dyspozycja w `model/gemm.rs` to 24 warianty `DevWeight` × 6 rodzin operacji,
rozpisane w 165 miejscach. Wyciąg z kodu pokazuje, że iloczyn jest niemal
całkowicie regularny:

| rodzina | ramion | różnych list argumentów |
|---|---|---|
| `gemv_norm` | 21 | **1** |
| `gemv_norm_silu` | 21 | **1** |
| `gemm_rows` | 18 | 16 → **1** po `row_offset_bytes` |
| `gemv`, `gemv_residual`, `gemv_out_f32` | 18–20 | do zbadania |

Nazwy kerneli układają się mechanicznie (`gemv_norm_<fmt>_f16`,
`gemv_norm_silu_<fmt>_f16`, `gemm_<fmt>_f16_at`), z jedynym wyjątkiem w postaci
wariantów DP4A dla Q4K, Q6K i Q8_0.

Skoro w dwóch rodzinach format wpływa WYŁĄCZNIE na nazwę kernela, 42 ramiona
sprowadzają się do jednej tabeli 21 wierszy, a dodanie kwantyzacji do tych
ścieżek staje się dopisaniem linii. Trzy formaty o niejednorodnej ścieżce
(`Fp8Row`, `NvFp4`, `NvFp4Gguf`) zostają wypisane osobno, bo naprawdę różnią
się treścią, a nie tylko nazwą.

Do zrobienia w tym etapie: zwinięcie tych dwóch rodzin w tabelę oraz `pack`
przeniesiony na ten sam kontrakt.

### Etap 3 — podział `launchers.rs` i wspólny rejestr wariantów ✅ ZROBIONE
20 563 linie i jeden `impl Kernels` z 414 metodami → 19 modułów. Kwantyzacje
GGUF dostały po module na RODZINĘ formatu, bo to realizuje „dodanie kwantyzacji
= dodanie pliku":

```
attention 2163  deltanet 1963  gemm/nvfp4 1712  gemm/dense 1160
gemm/fp8   726  quant     732  norm        458  sample       438
gemm/quantized/{k_quants, i_quants, legacy, q8_0, mxfp4, mixed}
```

Rejestr wariantów uogólniony z tego, co drugi agent zrobił dla Apple:
```rust
Variant { name, applies: fn(&Problem) -> bool, because: "zmierzone uzasadnienie" }
```
Dziś istnieje tylko pod `cfg(metal, macos)`; ma objąć CUDA i ROCm. Wybór kernela
staje się deklaratywny i **udokumentowany pomiarem**, zamiast rozsypanych `if`-ów.

Zrobione: część wspólna (`Problem`, `Variant`, `Registry`, predykat końcowy)
kompiluje się na każdej platformie, a listy form zostają per platforma — bo to
platforma decyduje, które formy w ogóle istnieją, a nie który kształt problemu
je wybiera. Doszedł rejestr `NVFP4_MATMUL` z dwiema formami (przepakowanie do
FP8, rozpakowywanie wprost), każda z zapisanym pomiarem, oraz trzy testy: na
totalność po zamiatanym zbiorze kształtów i na to, że dekodowanie wybiera
ścieżkę bez drugiej kopii wag.

### Etap 4 — podział `model.rs` po ścieżkach wykonania ✅ ZROBIONE
21 530 linii i jeden `impl Model` z 288 metodami → 16 modułów:

```
mtp 3157  arch/dense 2822  arch/hybrid/prefill 2034  gemm 1976
arch/hybrid/core 1061  loader 897  arch/hybrid/verify 799  debug 784
arch/moe 741  arch/hybrid/decode 573  graph 530  sample 514
tp 431  kv 430  scratch 117
```

Osią podziału NIE okazała się nazwa architektury (`llama`, `qwen35`,
`deepseek_v4`), jak zakładał pierwotny plan, tylko ścieżka wykonania: gęsta,
hybrydowa (DeltaNet) i MoE. Kod nigdy nie rozróżniał modeli po nazwie — robi to
przez `is_hybrid()` i obecność ekspertów — więc podział po nazwach byłby
wymyśloną warstwą. Dodanie modelu to dziś nowy moduł w `arch/`.

#### Czego nauczył podział: bajtowa zgodność metod to za mało

Pierwsze podejście wstawiło pustą linię WEWNĄTRZ wieloliniowego literału
`W4A8_CALIB_TEXT` — skaner zerował stan literału na końcu każdej linii, a tekst
kalibracyjny zawiera przykładowy kod z nawiasami klamrowymi. Porównanie ciał
metod tego nie widziało (stała leży poza `impl`), złapał to dopiero warning
kompilatora o zawieszonym `\` na końcu linii.

Dlatego dowodem przeniesienia jest teraz: **każdy element oryginału występuje w
wyniku dosłownie i jako ciągły fragment**, przy jedynej dozwolonej różnicy w
postaci `pub(crate)`. 452 z 452 dla `model.rs`, 414 z 414 metod dla
`launchers.rs`.

### Etap 5 — HAL: zdolności zamiast domysłów ⚠ CZĘŚCIOWO

**Zrobione.** `DeviceCaps` niesie po jednym polu na INSTRUKCJĘ, a nie na
pokoleniowe hasło marketingowe: `fp4_block_scale_ue8m0`,
`fp4_block_scale_e4m3`, `wgmma`, `tcgen05`, `tma`. `forge caps` je wypisuje,
więc odpowiedź nie wymaga już ręcznego składania sondy.

Przy okazji wyszedł błąd. Pole `fp4_native = sm >= 100` na GB10 (sm_121) dawało
prawdę, a natywnego NVFP4 tam nie ma — Blackwell dzieli się na dwie linie o
RÓŻNYM ISA rdzeni tensorowych, czego numer zdolności obliczeniowej sam nie
rozstrzyga. Pole nie miało ani jednego czytelnika, więc nic się nie psuło;
czekało tylko na pierwszego, który by mu uwierzył.

Zrobione też: dwa pytania o falę 32 mają nazwy zamiast czterech zapisów.
`matrix_warp32` pyta o jednostkę macierzową i falę 32 i ŚWIADOMIE nie pyta o
producenta; `nvidia_warp32` jest węższe.

**Korekta wcześniejszego opisu tego etapu.** Napisałem, że „63 miejsca pytają
`Vendor::Nvidia`, a 31 o `warp_size == 32`", sugerując jeden sweep. Po
przeliczeniu: oba warunki łączy tylko **dziewięć** miejsc, a reszta to
niezależne pytania. Skala była więc podana błędnie.

**Do zrobienia.** Rejestr wariantów nadal nie wybiera po tych faktach — wybór
kernela siedzi w `if`-ach, choć wiele z nich niesie już pomiar w komentarzu
(np. 111 GB/s wobec 597 GB/s dla wariantu falowego). Przeniesienie ich do
rejestru to zmiana PER MIEJSCE z własnym uzasadnieniem pomiarowym, nie sweep.

Osobno: każde użycie `nvidia_warp32` jest kandydatem na usterkę, którą już raz
złapaliśmy — `vendor == Nvidia` w bramce chunków prefillu kazał modelom qwen35
na Radeonie czytać komplet wag 64 razy na prompt 1024. Rozstrzygnięcie wymaga
Radeona, którego nie mamy, więc te miejsca zostają oznaczone, a nie zmienione
w ciemno.

Świadomie nie dopisałem też gałęzi „gdy karta ma natywne NVFP4", bo nie mamy
kernela, który by ją obsłużył; byłby to stub udający wybór.

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

  **Korekta wcześniejszego wniosku.** Odrzuciłem MXFP4 jako „drugą konwersję, w
  dodatku stratną". Obie części były nieścisłe. Kernel NVFP4 wprost rozpakowuje
  do f16 (`wv = (_e2m1x8(codes) * sc[wp]).cast[DType.float16]()`), więc liczy
  na MMA `k16` — połowie przepustowości FP8. Skal per-grupa nie da się nałożyć
  wewnątrz zwykłego MMA FP8, i właśnie dlatego istnieje przepakowanie: wtapia
  je w wartości przy ładowaniu. Ale iloczyn wartości 4-bitowej i skali E4M3
  trzeba zaokrąglić do e4m3, więc **konwersja, którą już robimy, też jest
  stratna** — kosztuje 7,35 GB i daje `k32`.

  | ścieżka | MMA | skale per-grupa | dodatkowa pamięć | stratna |
  |---|---|---|---|---|
  | NVFP4 natywnie (E4M3) | — | — | — | sprzęt nie wspiera |
  | wprost → f16 | `k16` | w kernelu | 0 | nie |
  | przepakowanie do FP8 (dziś) | `k32` | wtopione | +7,35 GB | tak |
  | MXFP4 | `k64` | natywnie w MMA | 0 | tak (skale) |

  Zasada „licz wprost" zostaje słuszna, ale na tym sprzęcie nie da się jej
  spełnić w pełni: natywnego NVFP4 z E4M3 nie ma. Wybór jest więc między trzema
  kompromisami, a nie między czystością a konwersją. Rozstrzygnie pomiar
  jakości (`forge ppl`) obu konwersji — również tej, którą stosujemy dziś i
  której nikt nie zmierzył.
- **Hopper i Blackwell datacenter** — dziś nie budujemy `sm_90` ani `sm_100`, więc
  FORGE tam nie ruszy. Po Etapach 3 i 5 dołożenie ścieżki `wgmma`/`tcgen05` to
  nowy wariant w rejestrze.
- **Apple** — praca drugiego agenta przestaje być wyspą: ten sam rejestr, te same
  techniki algorytmiczne, osobne tylko kernele.
