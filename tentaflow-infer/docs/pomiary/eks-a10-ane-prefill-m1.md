# EKS-A10 — ile warte jest trzecie ramię prefillu na Neural Engine (Apple M1)

EKS-A9 zmierzył, że ANE na M1 liczy wycinek FFN z 8–9 TFLOPS i sumuje się
z GPU; faza 0 (EKS-A10) ustaliła kodowanie, które ANE w ogóle wykonuje; EKS-A11
znalazł, że przy pełnym zestawie kształtów ramię jest pułapką pamięciową, a przy
T256 daje zysk. Ten dokument jest pomiarem końcowym: **cztery ramiona × trzy
długości promptu, trzy niezależne przebiegi**, liczniki ANE, pamięć, bramki
numeryczne i dekodowania — na wdrożonym kodzie, bez zmian w silniku.

Maszyna: Apple M1 (4 P + 4 E, GPU 8 rdzeni, pasywnie chłodzony), **16 GB**,
macOS 26.6.2 (25G83). Model: Bielik-Minitron-7B-v3.0-Instruct MLX 4-bit
(affine, grupa 64), 40 warstw, d_model 4096, inter 11264. Modele ANE:
`.runtime/ane/bielik-minitron-7b-s060-int8/` (80 mlmodelc, multifunction
T256/T512/T1024, int8 per-channel, share 0,6). Nikt inny nie pracował na
maszynie; otwarte aplikacje użytkownika nie były zamykane (Claude, Chrome,
Terminal — łącznie ~30% jednego rdzenia, load average 2,0–2,2 przed startem).
`pmset -g therm`: **brak ostrzeżeń** przed, między i po każdym przebiegu.

Protokół: `crates/forge-model/tests/cpu_share_prefill.rs::what_the_three_units_are_worth_in_prefill`,
`FORGE_ANE_DIR=… FORGE_ANE_SHAPES=256 FORGE_BENCH_REPS=3 FORGE_BENCH_PROMPTS=256,512,1024`,
release, cecha `ane`. Ramiona przeplatane w jednej długości: GPU → GPU+CPU →
GPU+CPU+ANE → GPU+ANE → GPU (odniesienie GPU = minimum z pierwszego i
ostatniego), każde ramię = 1 rozgrzewka + 3 przebiegi, mediana. Trzy osobne
procesy z przerwą 60 s (trzeci, bo między pierwszymi dwoma różnica przekroczyła
5% w ramionach z ANE). Test nie drukuje IQR; „rozstęp" niżej to
(max − min)/mediana z trzech przebiegów, a za ważne uznaję ramię z rozstępem
≤ 5%. Surowe wyjścia: `eks-a10-final-run{1,2,3}.txt`, `eks-a10-final-gates.txt`
w katalogu scratch sesji.

## Krótko

| prompt | GPU | GPU+CPU | **GPU+CPU+ANE** | GPU+ANE | 3 ramiona wobec GPU+CPU | wobec GPU |
|---:|---:|---:|---:|---:|---:|---:|
| 256 | 101,1 tok/s | 102,5 | **65,4** (rozstęp 11,6%, NIE) | 79,3 (7,4%, NIE) | **−36,2%** | −35,3% |
| 512 | 101,0 | 122,9 | **119,1** (22,5%, NIE) | 102,1 (5,5%, NIE) | **−3,1%** | +17,9% |
| 1024 | 99,8 | 129,7 | **168,7** (9,0%, NIE) | 113,8 (4,4%) | **+30,1%** | **+69,0%** |

Mediany z trzech przebiegów. Ramiona bez ANE mają rozstęp 0–2,2% (GPU+CPU
1024: 5,9%); ramiona z ANE 4,4–22,5%. **Zysk jest tylko przy 1024 tokenach**
i tam jest duży (+30% nad dzisiejszą ścieżką GPU+CPU, +69% nad samym GPU);
przy 512 ramię ANE jest w granicach błędu wobec GPU+CPU (i w jednym z trzech
przebiegów wyraźnie lepsze: 143,4), a przy 256 **szkodzi** w każdym przebiegu.
Dekodowanie: 11,0 / 11,0 tok/s (bez zmian). Logity: ten sam argmax, RMS 0,151%
rozpiętości.

## Punkt wyjścia

| | skąd | wartość |
|---|---|---:|
| ANE na M1, FFN int4 per-channel, T=1024, izolacja | EKS-A9 §2 | 8,1–8,4 TFLOPS |
| narzut jednego predict | EKS-A9 §1 | ~0,27 ms |
| GPU + ANE naraz: strata GPU / ANE | EKS-A9 §5 | 11% / 15–22% |
| kodowanie wykonywane w 100% na ANE | EKS-A10 faza 0 §0.1 | tylko per-channel symetryczne (int4/int8) |
| int8 per-channel wobec int4 na ANE | faza 0 §0.1b | 88–99% prędkości |
| błąd wyjścia int8 per-channel wobec MLX g64 na prawdziwych wagach | `tools/ane-export/README.md` | 0,85% (gate/up), 1,0–1,2% (down) |
| 80 modeli T1024: wired | EKS-A10 (wiele modeli) | +6,0 GB → thrashing |
| T256 w silniku, `FORGE_BENCH_REPS=1`, 1024 tokeny | EKS-A11 §3 | GPU+CPU 130,0 → **179,8 tok/s (+38%)** |
| T256, 512 tokenów | EKS-A11 §3 | 121,9 → 199,2 (+63%) |

EKS-A7 (M4) zamknął ANE liczbą z literatury (20–24 ms na predict) i wdrożył
parę CPU+GPU; na M1 CPU jest warte 0,65 TFLOPS, nie 1,5 (EKS-A9), i para daje
tu +21–30% przy 512–1024 zamiast +19–22% jak na M4 — ale przy 256 tylko +1–2%.

## Co zostało zbudowane

- **`tools/ane-export/ane_export.py`** — z checkpointu MLX wycina ogon wierszy
  gate/up (6720 z 11264) i down (2432 z 4096) każdej warstwy, re-kwantyzuje do
  int8 per-channel i pakuje w jeden multifunction `.mlmodelc` na grupę
  (T256/T512/T1024, wagi raz), z `manifest.json` i kontrolą `MLComputePlan`
  (80/80 `linear` na ANE).
- **`crates/forge-hal/coreml/forge_coreml_shim.m` + `src/coreml.rs`** (cecha
  `coreml`) — cienki shim nad CoreML: ładowanie `.mlmodelc` z nazwą funkcji,
  odczyt kształtów, `predict` na buforach wołającego (`initWithDataPointer`
  + `outputBackings`), także z krokiem wiersza; kod 1 sygnalizuje, że CoreML
  jednak skopiował.
- **`crates/forge-kernels/src/ane_matmul.rs`** — `AneMatmul`: czyta manifest,
  wiąże części grup z `WeightId` wykonawcy (odmawia przy złym kształcie), trzyma
  jeden wątek roboczy z `start`/`join` (role First/Middle/Last/Solo, bo
  gate+up to jeden predict), budżet `MAX_RESIDENT` = 120 funkcji z LRU,
  `FORGE_ANE_SHAPES` i sub-predicty, gdy T funkcji < tokeny kafla; liczniki
  `AneStats`.
- **`crates/forge-kernels/src/variant.rs`** — trzecia forma
  `MatrixUnitsSharedWithCpuAndAne` i `RowSplit { gpu, cpu, ane }`: ogon ANE
  odejmowany najpierw, progi CPU liczone na reszcie, poniżej
  `MIN_SPLIT_TOKENS` = 256 ogon zerowany (dekodowanie nigdy nie idzie na ANE).
- **`crates/forge-kernels/src/msl/scatter.rs`** — kernel `scatter_cols_f16_{f16,f32}`
  przenoszący okno kolumn z ciągłego bufora ANE `[T', width]` do slotu
  projekcji o kroku `rows`, w typie slotu.
- **`crates/forge-kernels/src/dense_exec.rs`** — `attach_ane` /
  `set_ane_share` / `ane_stats`, `FORGE_ANE_LAYERS` do bisekcji; ogon ANE
  startuje przed GPU, a `join` + scatter siedzi w otwartym buforze poleceń
  przed wszystkim, co slot potem czyta.
- Testy: `forge-hal/tests/coreml_backend.rs`, `forge-kernels/tests/metal_scatter.rs`,
  `forge-model/tests/ane_bindings.rs`, trzy testy ANE w `cpu_share_prefill.rs`.

## Pomiar: cztery ramiona, trzy przebiegi

tok/s per ramię (mediana z 3 powtórzeń w procesie), trzy procesy:

| prompt | przebieg | GPU | GPU+CPU | GPU+CPU+ANE | GPU+ANE |
|---:|---|---:|---:|---:|---:|
| 256 | 1 | 101,2 | 104,3 | 65,4 | 75,2 |
| 256 | 2 | 101,1 | 102,5 | 71,4 | 81,1 |
| 256 | 3 | 100,2 | 102,0 | 63,8 | 79,3 |
| 256 | **mediana** | **101,1** | **102,5** | **65,4** | **79,3** |
| 512 | 1 | 101,1 | 123,2 | 116,6 | 99,5 |
| 512 | 2 | 101,0 | 122,9 | 119,1 | 102,1 |
| 512 | 3 | 100,2 | 121,7 | 143,4 | 105,1 |
| 512 | **mediana** | **101,0** | **122,9** | **119,1** | **102,1** |
| 1024 | 1 | 99,8 | 130,0 | 181,8 | 116,9 |
| 1024 | 2 | 99,8 | 129,7 | 168,7 | 113,8 |
| 1024 | 3 | 99,8 | 122,4 | 166,7 | 111,9 |
| 1024 | **mediana** | **99,8** | **129,7** | **168,7** | **113,8** |

Zyski z median:

| prompt | GPU+CPU wobec GPU | GPU+CPU+ANE wobec GPU+CPU | GPU+CPU+ANE wobec GPU | GPU+ANE wobec GPU | GPU+ANE wobec GPU+CPU |
|---:|---:|---:|---:|---:|---:|
| 256 | +1,4% | **−36,2%** | −35,3% | −21,6% | −22,6% |
| 512 | +21,7% | **−3,1%** | +17,9% | +1,1% | −16,9% |
| 1024 | +30,0% | **+30,1%** | **+69,0%** | +14,0% | −12,3% |

Czasy prefillu (ms, mediana w procesie) z przebiegu 1 dla skali: GPU 2531 /
5066 / 10258; GPU+CPU 2453 / 4155 / 7878; GPU+CPU+ANE 3916 / 4392 / 5632;
GPU+ANE 3405 / 5148 / 8760.

Wobec EKS-A11 (`FORGE_BENCH_REPS=1`, drugi przebieg): 1024 zgadza się
(179,8 tam, 166,7–181,8 tu), **512 nie** (199,2 tam, 116,6–143,4 tu) — patrz
nierozstrzygnięte.

## Liczniki ANE

Na jeden prefill: 80 zleceń (40 warstw × gate_up + down), predictów 80 / 160 /
320 (T256 liczy 1024 tokeny czterema sub-predictami), doładowań 0, wypchnięć 0,
kopii 0 (CoreML pisał w nasz bufor za każdym razem). „Zajętość" =
predict_ms / prefill_ms; TFLOPS efektywne = FLOP ogona (2·T·(4096·13440 +
11264·2432)·40) / predict_ms.

| prompt | ramię | przebieg | predict [ms] | na predict [ms] | czekanie w `join` [ms] | prefill [ms] | zajętość ANE | TFLOPS ef. |
|---:|---|---|---:|---:|---:|---:|---:|---:|
| 256 | GPU+CPU+ANE | 1 / 2 / 3 | 851 / 947 / 849 | 10,6 / 11,8 / 10,6 | 16 / 34 / 11 | 3916 / 3587 / 4013 | 22 / 26 / 21% | 1,98 / 1,78 / 1,99 |
| 256 | GPU+ANE | 1 / 2 / 3 | 716 / 762 / 714 | 8,9 / 9,5 / 8,9 | 2 / 31 / 0 | 3405 / 3158 / 3230 | 21 / 24 / 22% | 2,36 / 2,22 / 2,36 |
| 512 | GPU+CPU+ANE | 1 / 2 / 3 | 1278 / 1313 / 1159 | 8,0 / 8,2 / 7,2 | 41 / 55 / 46 | 4392 / 4298 / 3571 | 29 / 31 / 33% | 2,64 / 2,57 / 2,91 |
| 512 | GPU+ANE | 1 / 2 / 3 | 1462 / 1423 / 1523 | 9,1 / 8,9 / 9,5 | 8 / 5 / 28 | 5148 / 5014 / 4870 | 28 / 28 / 31% | 2,31 / 2,37 / 2,22 |
| 1024 | GPU+CPU+ANE | 1 / 2 / 3 | 2022 / 2162 / 2273 | 6,3 / 6,8 / 7,1 | 46 / 33 / 101 | 5632 / 6069 / 6143 | 36 / 36 / 37% | 3,34 / 3,12 / 2,97 |
| 1024 | GPU+ANE | 1 / 2 / 3 | 2944 / 2996 / 2940 | 9,2 / 9,4 / 9,2 | 33 / 1 / 11 | 8760 / 9000 / 9151 | 34 / 33 / 32% | 2,29 / 2,25 / 2,30 |

Trzy rzeczy, które widać wprost z liczników:

- **Czekanie na ANE jest pomijalne** (0–1,6% prefillu, maksymalnie 101 ms).
  Host nigdy nie stoi na `join`; ANE zawsze kończy przed tym, jak wynik jest
  potrzebny.
- **ANE jest zajęte 21–37% czasu prefillu**, i tym mniej, im krótszy prompt.
  Przy 256 tokenach ANE liczy 0,85 s z 3,9 s.
- **Predict jest 2–4× wolniejszy niż w izolacji.** Ta sama funkcja T256 w
  harnessie liczyła 4,4 ms (EKS-A10 wiele modeli, 6,4 TFLOPS); tu 6,3–11,8 ms
  = 1,8–3,3 TFLOPS. Przy 1024 tokenach z CPU obok predict trwa 6,3–7,1 ms,
  bez CPU 9,2–9,4 ms — ten sam kierunek, co „CPU obok pomaga ANE" w EKS-A9 §5,
  ale różnica jest tu 1,4×, nie 9%. Przy 256 jest odwrotnie (10,6 z CPU, 8,9
  bez). Przyczyna nie została zmierzona.

## Pamięć

`vm_stat` (MiB, strona 16 KiB); „w procesie" = wiersze drukowane przez test.

| moment | free | wired |
|---|---:|---:|
| przed startem (maszyna) | 3784 | 2427 |
| w procesie, po wczytaniu checkpointu, przed ANE (p.1 / 2 / 3) | 63 / 63 / 60 | 2430 / 2398 / 2539 |
| po załadowaniu 80 × T256 (p.1 / 2 / 3) | 62 / 83 / 68 | **6404 / 6349 / 6575** (+3974 / +3951 / +4036) |
| w trakcie ramion z ANE, wszystkie przebiegi | 45–72 | 7166–8440 |
| po zakończeniu procesu (p.1 / 2 / 3) | 5949 / 5562 / 5793 | 2392 / 2544 / 2397 |
| po bramkach, koniec sesji (maszyna) | 5696 | 2980 |

Ładowanie 80 modeli: 1283 / 1194 / 1054 ms (ciepły cache `aned`, jak w
EKS-A11). Wired po załadowaniu rośnie o **~4,0 GB** — wagi int8 3,07 GiB plus
bufory we/wy programów T256 (EKS-A10: ~0,6 GB) — i wraca do bazy po
zakończeniu procesu. W trakcie ramion z ANE wired sięga 7,2–8,4 GB przy 16 GB
i wolnych 45–72 MiB.

Błędy stron procesu na jeden prefill (drobne / twarde), przebieg 1: GPU 93/7,
4/0, 2/0; GPU+CPU 16 537/3, 21 133/0, 21 021/0; GPU+CPU+ANE 68 569/4,
80 293/0, 58 865/0; GPU+ANE 36 078/0, 37 322/0, 34 974/0 (256 / 512 / 1024).
Twardych błędów stron praktycznie nie ma (0–8 na prefill, tylko przy 256);
drobne rosną z ANE 2–4× wobec GPU+CPU. Liczniki jądra za całą sesję (trzy
przebiegi + bramki, cztery procesy po 4,2 GB checkpointu): swapins +208 tys.
stron (3,2 GB), swapouts +257 tys. (3,9 GB), dekompresje +26,0 mln stron.
Wolne po sesji 5,7 GB wobec 3,8 przed.

## Bramki

**Logity** (`the_ane_share_keeps_the_logits`, prompt 512, GPU+CPU+ANE wobec
samego GPU): pierwszy token **842 / 842**, RMS **0,151%** rozpiętości, max
|Δ| 0,1738 = **0,62%** rozpiętości 28,2; próg 0,4% RMS — **PASS**. Te same
liczby co w EKS-A11, co jest spodziewane: ta sama ścieżka, te same wagi.

**Dekodowanie** (`the_ane_share_does_not_reach_decode`, 24 kroki po prompcie
256, `FORGE_BENCH_REPS=3`): samo GPU **11,0 tok/s**, z trzema ramionami
włączonymi **11,0 tok/s (−0,0%)**; próg dryfu 10% — **PASS**. Ogon jest
zerowany poniżej `MIN_SPLIT_TOKENS`, dekodowanie nie dotyka ANE.

## Koszt

- **Druga kopia wag: 3,07 GiB int8 na dysku** (80 `.mlmodelc`, 52,6 + 26,1 MiB
  na warstwę) i tyle samo w wired po załadowaniu, plus ~0,6 GB buforów T256 —
  razem +4,0 GB. Wycinek liczony przez ANE nadal siedzi w checkpoincie MLX
  4-bit rezydentnym na GPU (~1,6 GB dla share 0,6), więc te wiersze są w
  pamięci dwa razy; usunięcie duplikatu nie zostało zrobione.
- **Dlaczego int8 per-channel, a nie blockwise (bit-exact z MLX):** kompilator
  ANE wykonuje `linear` tylko z jedną skalą na kanał wyjściowy bez przesunięcia
  (faza 0 §0.1d: każda grupa, każdy offset, LUT per grupa i int8 z grupą spadają
  na CPU z 13–26% prędkości i 2× gorszą numeryką). Int4 per-channel liczy się
  w 100% na ANE, ale kosztuje 17–25% błędu wyjścia na prawdziwych wagach; int8
  per-channel 0,85–1,2% przy 88–99% prędkości int4. Cena: 2× dysk i wired
  wobec int4.
- **Dlaczego T256:** przy T1024 bufory we/wy 80 programów to 2,4 GB wired
  (razem +6,0 GB), co na 16 GB dawało 36 tok/s przy 1024 tokenach przez
  thrashing (EKS-A11 §3). T256 zmniejsza je do 0,6 GB kosztem 4 sub-predictów
  na kafel 1024 i narzutu CoreML na każdy (~0,27 ms w izolacji).
- Wywołanie CoreML per grupa per warstwa: 80 na prefill, bez doładowań przy
  jednym kształcie (wszystkie 80 funkcji mieszczą się w budżecie 120).

## Co zjada czas teraz

Z liczników, nie z profilu (tu profilu nie zbierano):

- **Prefill 1024 z trzema ramionami trwa 5,6–6,1 s, z czego ANE liczy
  2,0–2,3 s (36%) i host czeka na nie 33–101 ms.** Pozostałe ~64% to GPU+CPU na
  swoich wierszach (gate/up 4544 z 11264, down 1664 z 4096), uwaga, normy, k/v
  — czyli praca, której ANE dziś nie dostaje. ANE ma ~64% wolnego czasu; to
  ten sam wniosek, co w EKS-A11, tylko na trzech przebiegach.
- **Przy 256 tokenach ramię z ANE jest wolniejsze niż samo GPU o 1,0–1,5 s na
  prefill** (3,6–4,0 s wobec 2,5 s), choć ANE liczy tylko 0,85–0,95 s i czekanie
  na nie jest ≤ 34 ms. Straty nie ma gdzie przypisać w licznikach ramienia —
  nie jest w predict ani w `join`. Drobne błędy stron rosną (68 tys. wobec
  16,5 tys. w GPU+CPU), wired w trakcie to 8,0–8,4 GB przy 45–65 MiB wolnego.
  Skąd bierze się ta sekunda, nie zostało zmierzone.
- **Efektywne TFLOPS ANE w silniku: 1,8–3,3**, wobec 6,4 dla tej samej funkcji
  w harnessie na tej samej maszynie i 8–9 dla int4 w EKS-A9. Predict w silniku
  jest 1,4–2,7× dłuższy niż w izolacji; EKS-A11 przypisał to współdzielonej
  magistrali z GPU/CPU, ale to nie zostało zmierzone osobno (bez
  `powermetrics`).
- Rozrzut między przebiegami dla ramion z ANE (4–23%) jest o rząd większy niż
  bez ANE (0–2%, wyjątek GPU+CPU 1024: 6%). Jeden przebieg nie rozstrzyga nic
  poniżej ~20% w tych ramionach.

## Nierozstrzygnięte i następne kroki

- **512 tokenów: 116,6 / 119,1 / 143,4 tu wobec 199,2 w EKS-A11.** Ten sam
  kod, te same modele, ta sama maszyna; różnice protokołu: A11 miało
  `FORGE_BENCH_REPS=1`, prompt 512 jako pierwszy w procesie (tu po 256) i inny
  stan pamięci maszyny (wolne 5,8 GB + speculative 2,1 przed startem, tu 3,8
  GB). Które z tego odpowiada za 40–70% różnicy, nie wiadomo; do rozstrzygnięcia
  osobnym przebiegiem z `FORGE_BENCH_PROMPTS=512` samym i z odwróconą
  kolejnością.
- **Strata przy 256 tokenach** (−36%) nie ma zmierzonej przyczyny (patrz wyżej).
  Zanim ramię wejdzie do produkcji, próg włączenia ANE powinien być osobny od
  `MIN_SPLIT_TOKENS` i wyżej niż 256 — na tej maszynie zmierzony zysk jest
  dopiero od 1024.
- **Większy udział ANE** (share > 0,6 albo dołożenie q/k/v/o — `ane_export.py`
  już przyjmuje `--groups qkv,o`): ANE stoi 63–79% czasu, a każdy wiersz
  przeniesiony z GPU+CPU skraca ich część. Kosztuje wired (int8: 132 MiB na
  warstwę pełnego FFN), którego przy 16 GB jest mało; sensowniej najpierw
  zdjąć duplikat wag z checkpointu MLX.
- **Strided `outputBackings` zamiast scatter**: faza 0 §0.3 pokazała, że CoreML
  pisze in-place z krokiem 11264 bez kosztu; shim ma `fc_model_predict_strided`,
  ale ścieżka silnika oddaje ciągły bufor i rozrzuca kernelem. Kernel scatter
  siedzi w buforze poleceń GPU, więc jego koszt nie jest w licznikach ANE i nie
  został zmierzony osobno.
- **Sub-predicty asynchronicznie**: 4 predicty T256 na kafel 1024 idą kolejno w
  jednym wątku; narzut CoreML na każdy nie jest tu ukryty.
- **M4**: cały pomiar jest na M1, gdzie CPU daje 0,65 TFLOPS, a ANE 16-rdzeniowe
  M4 ma inne proporcje do GPU. Liczby z tego dokumentu nie przenoszą się.
- `powermetrics` (sudo) nadal niezebrane — zajętość ANE jest liczona z czasu
  predict, nie z licznika sprzętowego.

## Źródła

- EKS-A7 — `docs/pomiary/eks-a7-cpu-gpu-wspolbieznie-m4.md` (para CPU+GPU, protokół przeplatany)
- EKS-A9 — `docs/pomiary/eks-a9-ane-wspolbieznie-m1.md` (narzut predict, TFLOPS ANE, współbieżność)
- EKS-A10 faza 0 — `docs/pomiary/eks-a10-faza0-ane-m1.md` (kodowania, multifunction, strided output)
- EKS-A10 wiele modeli — `docs/pomiary/eks-a10-wiele-modeli-ane-m1.md` (wired, thrashing)
- EKS-A11 — `docs/pomiary/eks-a11-ane-t256-cache-m1.md` (cache `aned`, T256)
- `tools/ane-export/README.md` (kodowanie, koszty, weryfikacja na prawdziwych wagach)
- test: `crates/forge-model/tests/cpu_share_prefill.rs`
