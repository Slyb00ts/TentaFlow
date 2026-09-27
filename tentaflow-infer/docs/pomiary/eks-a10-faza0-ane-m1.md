# EKS-A10 faza 0 — blockwise grupa 64, multifunction, wyjście z krokiem (Apple M1)

Trzy sondy rozstrzygające przed implementacją ścieżki ANE w Rust. Każda ma
jedno pytanie i jedno kryterium; odpowiedzi są w „Krótko", reszta to dowody.

Maszyna: Apple M1 (4P+4E, GPU 8 rdzeni, pasywnie chłodzony), 16 GB, macOS 26.6.2
(25G83), Swift 6.3.3, coremltools 9.0. Stan termiczny `ProcessInfo` +
`pmset -g therm`: **nominal, bez ostrzeżeń** przed, między i po każdej sekcji.
**Maszyna była obciążona** (inne agenty kompilowały Rusta; load average 2,9–5,1
przez cały czas pomiaru), dlatego wszystkie porównania są **przeplatane** — warianty
załadowane naraz w jednym procesie, 9 rund (A, B, C, D po 20 predict), pierwsza runda
odrzucona, mediana i IQR z 8. Bezwzględne liczby mają przez to szerszy IQR niż w
EKS-A9 (część wierszy „NIE"), ale stosunek wariantów w jednej rundzie nie zależy od
tego, co robi reszta maszyny. Sekcję 0.1b uruchomiono dwa razy (drugi raz przy
mniejszym obciążeniu) i obie serie są podane.

Kształt jak w A9: wycinek FFN 7B, gate/up [3072×4096], silu·up, down [4096×3072],
x [T×4096] f16, T = 256/512/1024, FLOP = 2·T·4096·3072·3, wagi N(0, 0,02) z tym
samym ziarnem co A9 (pliki `x_T*.f16.bin` zgodne). Narzędzia:
`tools/eks-apple/eks_a9_gen.py --a10`, `tools/eks-apple/eks_a10_ane.swift`
(`./run.sh a10 <modele> [sekcja]`), sondy dodatkowe z `tools/eks-apple/eks_a10_probe_gen.py`
(sekcja `probe` harnessu).

## Krótko

| sonda | pytanie | odpowiedź |
|---|---|---|
| **0.1 blockwise grupa 64** | czy `constexpr_blockwise_shift_scale` z grupą 64 wzdłuż K liczy się na ANE | **NIE.** MLComputePlan: `linear` z takimi wagami idzie na **CPU** (BNNS), tylko silu/mul zostają na ANE. Czas: 2,1–2,2 TFLOPS = **24–26% int4 per-channel**. K0 (100% ANE, ≥85%) — **niespełnione**, i to nie przez offset ani rozmiar grupy: na CPU spada każda grupa (64…2048), każdy offset (f16, uint4, także per-channel), LUT per-grupa, int8 z grupą, `matmul` zamiast `linear`. Na ANE zostaje wyłącznie **symetryczne per-channel** (int4 8,4–8,9 TFLOPS, int8 7,4–8,4) oraz per-channel z zero-pointem uint4 (5,0 TFLOPS, 60%). |
| **0.2 multifunction** | czy jeden mlpackage z T256/T512/T1024 dzieli wagi i liczy tak samo | **TAK.** mlpackage i mlmodelc: **18,04 MiB = jeden model** (suma trzech osobnych 54,11) — jeden blob, trzy funkcje wskazują te same offsety. Ładowanie z `functionName`: zimne 160–240 ms/funkcję, ciepłe 30–35 ms, ponowne w procesie 10–12 ms — tyle samo co osobny model. TFLOPS funkcji wobec osobnego modelu: −1,5…+0,9% (w rozrzucie). Wyjście bit w bit równe. Pamięć: w żadnym liczniku (footprint, RSS `aned`, wired) nie widać ani wag jednego, ani trzech modeli — różnica jest poniżej szumu maszyny (±100 MiB). |
| **0.3 wyjście z krokiem** | czy `outputBackings` przyjmie MLMultiArray [T,4096] ze stride [11264,1] i napisze in-place | **TAK.** Przyjęte bez błędu, wskaźnik zwróconej tablicy = nasz (CoreML nie podmienił bufora), bajty **identyczne** z wyjściem kontigualnym, komórki poza oknem **nietknięte** (wartownik), czas **w rozrzucie kontigualnego** (−1,1…+5,1%, IQR podobny) — i dla początku wiersza wyrównanego do strony (colOffset 0), i dla środka wiersza (colOffset 4096 = 8 KiB od strony). Ręczna kopia wierszy (memcpy T×8 KiB) kosztowałaby +4,6…+8,1%. |

**Rekomendacja:** kodowanie wycinka ANE = **int4 per-channel symetryczne, re-kwantyzowane
z wag MLX** *tylko jeśli* błąd re-kodowania jest do przyjęcia na prawdziwych wagach
(na wagach gaussowskich bez outlierów to 2,8e-1 względnej L2 wyjścia — za dużo);
w przeciwnym razie **int8 per-channel** (1,6e-2 na wyjściu, 88–94% prędkości int4,
2× wagi na dysku: 36 MiB wobec 18 na wycinek). Układ modeli = **jeden multifunction
na warstwę** (T256/T512/T1024, wagi raz). Rozrzut = **strided outputBackings** wprost
do bufora [T, 11264] (bez kernela scatter, bez kopii).

## 0.1 Blockwise grupa 64 na ANE

### 0.1a Plan (MLComputePlan, `cpuAndNeuralEngine`)

Wszystkie modele: 6 operacji liczących (`linear`×3, `silu`, `mul`, `identity`) plus
stałe. „brak" = operacje `constexpr_*`/`const` bez przydziału (dekompresja przy
ładowaniu).

| model (T=256/512/1024 identycznie) | wagi | linear ×3 | silu, mul | koszt na ANE |
|---|---|---|---|--:|
| ffn_T*_int4 (A9, `linear_quantize_weights` per_channel) | `constexpr_blockwise_shift_scale`, data int4 [N,K], scale f16 [N,1], bez offsetu | **ANE** | ANE | **100%** |
| ffn_T*_blockwise | data uint4 [N,K], scale f16 [N,K/64], **offset f16** [N,K/64] | **CPU** | ANE (T≥512) / CPU (T=256) | 0% |
| ffn_T*_blockwise_zp | jak wyżej, **offset uint4** (zero-point całkowity, q przeliczone) | **CPU** | ANE / CPU | 0% |
| ffn_T*_affine16 | `constexpr_affine_dequantize` (iOS16), **int8** per-channel, zero_point 0 | **ANE** | ANE | **100%** |
| ffn_multi_blockwise (T256/T512/T1024) | jak blockwise | **CPU** | ANE / CPU | 0% |

`constexpr_affine_dequantize` w coremltools 9.0 przyjmuje wyłącznie int8/uint8
(4 bity od iOS18), więc kontrola (ii) z planu jest 8-bitowa — i jest jedyną, poza
per-channel int4, która została na ANE. Skompilowany program dla wariantów spadających
na CPU to BNNS (`bnns_program.bnnsir` w cache, **75 MiB = wagi zdekwantyzowane do
fp16**) — CoreML rozpakowuje takie wagi przy ładowaniu i liczy je na CPU w f16.

### 0.1d Co jeszcze ANE odrzuca — sondy (T=512, ten sam FFN)

Żeby odpowiedzieć „dlaczego" i nie zostawić otwartych wariantów: 17 dodatkowych
kodowań, plan + czas. Czas modeli w 100% na ANE mierzony przeplatanie (dwa
niezależne przebiegi: A / B), modeli z CPU — jeden przebieg orientacyjny.

| kodowanie (`constexpr_*` → `linear`) | plan | mlmodelc | µs (A / B) | TFLOPS (A / B) | wobec int4 |
|---|---|--:|--:|--:|--:|
| **int4 per-channel symetryczne** (odniesienie) | 6/6 ANE | 18,0 | **4 470** (1,9%) / 4 653 (17,9% NIE) | **8,65 / 8,31** | 100% |
| per-channel + **offset f16** [N,1] | linear CPU | 18,1 | ~17 700 / 24 100 | 2,2 / 1,6 | 19–25% |
| per-channel + **zero-point uint4** [N,1] | **6/6 ANE** | 18,0 | **7 754** (6,0% NIE) | **4,99** | **60%** |
| grupa 64 wzdłuż K, **bez offsetu** (symetryczna) | linear CPU | 19,1 | ~17 700 / 25 800 | 2,2 / 1,5 | 18–25% |
| grupa 128 / 256 / 512 / 1024 / 2048 wzdłuż K, offset f16 | linear CPU (każda) | 18,1–19,1 | 18 000–31 000 | 1,2–2,2 | 14–25% |
| grupa 64 wzdłuż **N** (oś wyjściowa), offset f16 | linear CPU | 20,3 | 82 600 / 99 200 | 0,4–0,5 | 5% |
| grupa 64 wzdłuż K, `matmul(x, Wᵀ)` zamiast `linear` | matmul CPU | 20,3 | 232 000 / 274 000 | 0,14–0,17 | 2% |
| **LUT 4-bit per grupa 64** (`constexpr_lut_to_dense`, lut [N,K/64,16,1]) | linear CPU | 36,0 | 18 700 / 25 900 | 1,5–2,1 | 18–24% |
| LUT per grupa 64 wzdłuż N | linear CPU | 36,0 | 68 400 / 100 900 | 0,4–0,6 | 5–7% |
| **int8** grupa 64 wzdłuż K (symetryczna) | linear CPU | 37,1 | 27 500 | 1,4 | 17% |
| **int8 per-channel** z wag zdekwantyzowanych MLX (re-kodowanie) | **6/6 ANE** | 36,0 | **4 787** (1,1%) / 5 058 (24,4% NIE) | **8,08 / 7,64** | **92–93%** |
| **int4 per-channel** z wag zdekwantyzowanych MLX (re-kodowanie) | **6/6 ANE** | 18,0 | **4 433** (0,4%) / 4 686 (34,4% NIE) | **8,72 / 8,25** | **99–101%** |
| K rozbite na 64 kawałki po 64, każdy `linear` per-channel z offsetem, suma | 464 ANE, 0 CPU | 20,5 | 37 141 (0,2%) / 38 162 | 1,04 / 1,01 | 12% |
| K rozbite na 8 kawałków po 512 | 26 ANE, 32 CPU | 18,3 | 41 500 / 46 700 | 0,8–0,9 | 10–11% |

Wniosek z sond: kompilator ANE w macOS 26.6 (ANECCompile przez E5RT) przyjmuje w
`linear`/`conv` **tylko dekwantyzację z jedną skalą na kanał wyjściowy i bez
przesunięcia** (ewentualnie z całkowitym zero-pointem, ale za 40% prędkości).
Każda skala zależna od pozycji wzdłuż K — grupa dowolnej wielkości, LUT per-grupa,
int8 z grupą — ląduje na CPU. Rozbicie na kawałki po 64 formalnie zostaje na ANE,
ale liczy 8× wolniej (464 małych operacji, K=64 na jeden `linear`). To oznacza, że
**wagi MLX affine grupa 64 nie mają bit-exact reprezentacji wykonywalnej na ANE**;
trzeba je re-kwantyzować per-channel.

### 0.1b Czas przeplatany, T = 256/512/1024 (int4 per-channel = 100%)

Przebieg 1 (load 4–5) i przebieg 2 (load ~3). Kolumna „wobec int4" = czas int4 /
czas wariantu w tej samej rundzie.

| T | kodowanie | mlmodelc [MiB] | p.1 mediana [µs] | IQR | p.2 mediana [µs] | IQR | TFLOPS (1 / 2) | wobec int4 (1 / 2) |
|--:|---|--:|--:|--:|--:|--:|--:|--:|
| 256 | int4 per-channel | 18,0 | 2 582 | 5,6% (NIE) | **2 293** | 2,3% | 7,49 / **8,43** | 100% |
| 256 | blockwise g64, offset f16 | 20,3 | 11 746 | 10,0% (NIE) | 8 713 | 2,8% | 1,65 / 2,22 | 22 / 26% |
| 256 | blockwise g64, offset uint4 | 19,4 | 11 892 | 5,7% (NIE) | 8 803 | 3,3% (NIE) | 1,63 / 2,20 | 22 / 26% |
| 256 | int8 per-channel (affine16) | 36,0 | 2 768 | 3,4% (NIE) | 2 605 | 2,1% | 6,98 / 7,42 | 93 / 88% |
| 512 | int4 per-channel | 18,0 | 4 857 | 9,6% (NIE) | **4 333** | 4,4% (NIE) | 7,96 / **8,92** | 100% |
| 512 | blockwise g64, offset f16 | 20,3 | 22 886 | 33,3% (NIE) | 17 800 | 3,8% (NIE) | 1,69 / 2,17 | 21 / 24% |
| 512 | blockwise g64, offset uint4 | 19,4 | 20 063 | 55,8% (NIE) | 18 072 | 4,0% (NIE) | 1,93 / 2,14 | 24 / 24% |
| 512 | int8 per-channel (affine16) | 36,0 | 4 905 | 18,3% (NIE) | 4 744 | 4,0% (NIE) | 7,88 / 8,15 | 99 / 91% |
| 1024 | int4 per-channel | 18,0 | 11 003 | 25,4% (NIE) | **8 677** | 5,4% (NIE) | 7,03 / **8,91** | 100% |
| 1024 | blockwise g64, offset f16 | 20,3 | 81 210 | 36,5% (NIE) | 36 413 | 1,0% | 0,95 / 2,12 | 14 / 24% |
| 1024 | blockwise g64, offset uint4 | 19,4 | 84 099 | 22,1% (NIE) | 35 494 | 0,8% | 0,92 / 2,18 | 13 / 24% |
| 1024 | int8 per-channel (affine16) | 36,0 | 12 498 | 10,2% (NIE) | 9 209 | 0,9% | 6,19 / 8,39 | 88 / 94% |

Przebieg 1 był zbierany przy dwóch równoległych kompilacjach Rusta — warianty na CPU
(blockwise) cierpią na tym najbardziej (0,9 TFLOPS przy T=1024), ale i int4 na ANE
traci (7,0 wobec 8,9), bo ścieżka predict ma odcinek CPU (A9 §2). Przebieg 2 zgadza
się z A9 (int4 8,4–8,9 TFLOPS). **W obu przebiegach blockwise = 13–26% int4**, int8
per-channel = 88–99%.

### 0.1c Numeryka (T=512, referencja `cblas_sgemm` f32)

Referencja „coremltools" liczy wagi, które CoreML faktycznie zdekwantyzował
(`decompress_weights`); „MLX" liczy q·scale+bias na tych samych q/scale/bias
(f16 parametry, f32 arytmetyka), czyli to, co liczyłby MLX.

| wagi | wykonawca | referencja | względna L2 | max \|Δ\| | max \|ref\| |
|---|---|---|--:|--:|--:|
| MLX grupa 64 (kwantyzacja sama) | CPU f32 | fp16 | 1,58e-1 | 0,90 | 5,52 |
| blockwise (offset f16 = −bias/scale) | CPU f32 (coremltools) | MLX | **9,4e-4** | 5,8e-3 | 5,72 |
| blockwise | CoreML (→ **CPU**, BNNS f16) | coremltools | 1,26e-2 | 8,5e-2 | 5,72 |
| blockwise | CoreML (→ CPU) | MLX | 1,26e-2 | 8,4e-2 | 5,72 |
| blockwise_zp (zero-point uint4) | CoreML (→ CPU) | MLX | 1,61e-1 | 0,95 | 5,72 |
| int4 per-channel (A9) | ANE | coremltools | 5,48e-3 | 3,4e-2 | 5,89 |
| **int8 per-channel z wag MLX** | re-kodowanie samo (CPU f32) | MLX | 1,51e-2 | 0,105 | 5,72 |
| int8 per-channel z wag MLX | ANE | coremltools | 5,55e-3 | 3,3e-2 | 5,72 |
| **int8 per-channel z wag MLX** | **ANE** | **MLX** | **1,61e-2** | 0,104 | 5,72 |
| int4 per-channel z wag MLX | re-kodowanie samo | MLX | 2,76e-1 | 1,62 | 5,72 |
| int4 per-channel z wag MLX | ANE | coremltools | 5,44e-3 | 3,2e-2 | 6,07 |
| **int4 per-channel z wag MLX** | **ANE** | **MLX** | **2,77e-1** | 1,62 | 5,72 |

Trzy rzeczy do zapamiętania: (1) offset f16 = −bias/scale **nie jest bit-exact**
wobec q·scale+bias (35% wag równych w f16, 9,4e-4 względnej L2 wag) — gdyby
blockwise działało, i tak nie byłoby identyczne z MLX; (2) ścieżka CPU CoreML, na
którą spada blockwise, liczy z błędem 1,26e-2, dwa razy gorszym niż ANE (5,5e-3,
jak w A9 §6); (3) re-kodowanie wag MLX do **int8 per-channel kosztuje 1,5e-2**
względnej L2 wyjścia, a na ANE łącznie 1,6e-2 — wobec 1,6e-1 samej kwantyzacji
grupa 64 to +10% błędu; do **int4 per-channel** — 2,8e-1, czyli więcej niż sama
kwantyzacja MLX. Na wagach gaussowskich bez outlierów; na prawdziwych wagach 7B
trzeba zmierzyć osobno (per-channel int4 na wagach z outlierami będzie gorsze).

## 0.2 Multifunction

Jeden mlpackage z funkcjami `T256`/`T512`/`T1024` (`MultiFunctionDescriptor.add_function`
z trzech mlpackage int4 per-channel, `save_multifunction`, `xcrun coremlcompiler compile`).
Pierwsza wersja zbudowana z blockwise (na CPU) dała te same wnioski o deduplikacji
i ładowaniu; poniżej wersja int4 (na ANE).

### Rozmiar na dysku — deduplikacja

| | mlpackage [MiB] | mlmodelc [MiB] |
|---|--:|--:|
| suma trzech osobnych modeli (T256+T512+T1024) | 54,11 | 54,11 |
| **multifunction, 3 funkcje** | **18,04** | **18,05** |

`model.mil` trzech funkcji wskazuje **te same offsety** w jednym `weights/weight.bin`
(11 blobów, każdy przywołany 3 razy). Deduplikacja jest pełna: koszt trzech kształtów
= koszt jednego + 10 KB MIL.

### Ładowanie (`MLModel(contentsOf:configuration:)`, `configuration.functionName`)

„Zimne" = pierwszy raz po skompilowaniu, z wyczyszczonym
`~/Library/Caches/eks_a10_ane` (cache E5RT tego procesu); „ciepłe" = drugi proces;
„ponowne" = drugi `MLModel` tego samego pliku w tym samym procesie. Dla modeli
liczonych na ANE cache per-proces nie rośnie (8 KB na model — skompilowany program
ANE mieszka w `aned`), a jednak zimne ≠ ciepłe, więc cache jest po stronie systemu.

| model | funkcja | zimne [ms] | ciepłe [ms] | ponowne w procesie [ms] | 1. predict zimny [µs] |
|---|---|--:|--:|--:|--:|
| ffn_multi_int4 | T256 | 242 | 35 | 10 | 5 637 |
| ffn_multi_int4 | T512 | 163 | 30 | 10 | 5 102 |
| ffn_multi_int4 | T1024 | 198 | 30 | 11 | 10 622 |
| ffn_T256_int4 (osobny) | main | 197 | 33 | 9 | 2 897 |
| ffn_T512_int4 (osobny) | main | 190 | 30 | 10 | 5 188 |
| ffn_T1024_int4 (osobny) | main | 200 | 30 | 9 | 10 933 |

Ładowanie funkcji z multifunction kosztuje tyle, co osobnego modelu — każda funkcja
jest kompilowana dla ANE osobno (T256 z multifunction zimno 242 ms, nie „darmowo"
po T512). Pierwszy predict jest 1,2–2× dłuższy niż ustalony (5,6 ms wobec 2,3 dla
T=256) — pierwsze wywołanie mapuje bufory; to jednorazowe.

### TFLOPS funkcji wobec osobnego modelu (przeplatane, dwa procesy)

| T | wariant | zimny proces: mediana [µs] | IQR | ciepły proces: mediana [µs] | IQR | TFLOPS (ciepły) | multi wobec osobnego (zimny / ciepły) |
|--:|---|--:|--:|--:|--:|--:|--:|
| 256 | osobny model | 3 961 | 17,2% (NIE) | 3 949 | 7,1% (NIE) | 4,89 | — |
| 256 | multifunction T256 | 3 995 | 4,1% (NIE) | 3 891 | 12,3% (NIE) | 4,97 | +0,9% / −1,5% |
| 512 | osobny model | 7 946 | 7,6% (NIE) | **4 454** | 2,9% | **8,68** | — |
| 512 | multifunction T512 | 7 952 | 2,2% | **4 411** | 2,7% | **8,76** | +0,1% / −1,0% |
| 1024 | osobny model | 12 390 | 4,2% (NIE) | **8 677** | 1,1% | **8,91** | — |
| 1024 | multifunction T1024 | 12 276 | 3,9% (NIE) | **8 713** | 0,7% | **8,87** | −0,9% / +0,4% |

Zimny proces trafił w szczyt obciążenia (4,9 TFLOPS na int4 przy T=512 wobec 8,7 —
patrz uwaga o DVFS w A9 §5), ale **para osobny/multi w każdej rundzie różni się
o ≤1,5%**. T=256 w obu procesach ma 4,9 TFLOPS i szeroki IQR — ten kształt jest
najbardziej wrażliwy na obciążony CPU (2,3 ms na ANE, 0,27 ms narzutu). Wyjście
funkcji multifunction jest **bit w bit** równe wyjściu osobnego modelu (0 różnych
bajtów z 1 M / 2 M / 4 M, sekcja numeric).

### Pamięć

Przyrosty po załadowaniu trzech modeli/funkcji i jednym predict na każdym, potem po
zwolnieniu (`vm_stat`, RSS `aned` przez `ps`, `phys_footprint` procesu; `footprint`
wymaga roota dla `aned`). Dwie kolejności w osobnych procesach.

| kolejność | zestaw | Δ po załadowaniu 3 [MiB] | Δ po zwolnieniu [MiB] |
|---|---|---|---|
| multi → osobne | multifunction ×3 | proces +31,5, aned RSS +3,0, wired +57,8, wolne −5,2, kompresor −9,6 | proces +3,4, aned +3,0, wired +103,2, kompresor −82,4 |
| | 3 osobne modele | proces +28,3, aned RSS +2,1, wired +57,6, wolne +15,2, kompresor −60,8 | proces +0,2, aned +2,1, wired −93,8, kompresor −147,3 |
| osobne → multi | 3 osobne modele | proces +31,4, aned RSS +2,1, wired +93,8, wolne +3,7, kompresor −64,2 | proces +3,2, aned +2,1, wired −80,2, kompresor −162,3 |
| | multifunction ×3 | proces +28,5, aned RSS −0,6, wired +191,6, wolne +32,3, kompresor −108,5 | proces +0,4, aned −0,6, wired +22,8, kompresor −228,9 |

Proces rośnie o 28–31 MiB w obu przypadkach (bufory we/wy 3 kształtów = 28 MiB —
zgadza się), `aned` o 2–3 MiB, a `wired` i kompresor skaczą o ±100–230 MiB **bez
związku z zestawem** (kompilacje Rusta obok). Wagi (18 lub 54 MiB) nie są widoczne
w żadnym liczniku — jak w A9, żyją w mapowaniu poza procesem; ta metoda nie potrafi
rozstrzygnąć, czy `aned` trzyma je raz, czy trzy razy. Rozstrzyga za to dysk (18
wobec 54 MiB) i strona odczytu: przy ładowaniu CoreML mapuje `weight.bin`, a przy
multifunction to jeden plik.

## 0.3 Wyjście z krokiem (`outputBackings` na buforze [T, 11264])

Model int4 per-channel rank-2 [T,4096] → [T,4096]. Bufor docelowy [T, 11264] f16
wyrównany do strony, wypełniony wartownikiem (−777). `outputBackings["y"] =
MLMultiArray(dataPointer: base + colOffset·2, shape: [T, 4096], dataType: .float16,
strides: [11264, 1])`. Kontrole po predict: (i) `dataPointer` zwróconej tablicy ==
nasz wskaźnik (in-place; gdy CoreML odrzuca backing, **nie zgłasza błędu**, tylko
alokuje własną tablicę — dlatego ta kontrola jest konieczna), (ii) bajty okna ==
bajty wyjścia kontigualnego (a2), (iii) każda komórka poza oknem == wartownik.
Czas przeplatany z (a2) i z (a2)+memcpy T wierszy po 8 KiB do bufora szerokiego.

| T | wariant | przyjęte | in-place | bajty = (a2) | poza oknem nietknięte | mediana [µs] | IQR | ważny | wobec (a2) |
|--:|---|---|---|---|---|--:|--:|---|--:|
| 256 | (a2) kontigualne + outputBackings | tak | tak | — | — | **2 646** | 25,7% | NIE | — |
| 256 | (a2) + memcpy 256 wierszy | tak | — | — | — | 2 861 | 36,3% | NIE | +8,1% |
| 256 | krok 11264, colOffset 0 | **tak** | **tak** | **tak** | **tak** | 2 692 | 15,1% | NIE | +1,7% |
| 256 | krok 11264, colOffset 4096 (8 KiB od strony) | **tak** | **tak** | **tak** | **tak** | 2 780 | 16,7% | NIE | +5,1% |
| 512 | (a2) kontigualne + outputBackings | tak | tak | — | — | **4 729** | 1,1% | tak | — |
| 512 | (a2) + memcpy 512 wierszy | tak | — | — | — | 4 957 | 2,0% | tak | +4,8% |
| 512 | krok 11264, colOffset 0 | **tak** | **tak** | **tak** | **tak** | 4 676 | 3,3% | NIE | −1,1% |
| 512 | krok 11264, colOffset 4096 | **tak** | **tak** | **tak** | **tak** | 4 736 | 5,3% | NIE | +0,2% |
| 1024 | (a2) kontigualne + outputBackings | tak | tak | — | — | **8 998** | 3,2% | NIE | — |
| 1024 | (a2) + memcpy 1024 wierszy | tak | — | — | — | 9 415 | 3,0% | NIE | +4,6% |
| 1024 | krok 11264, colOffset 0 | **tak** | **tak** | **tak** | **tak** | 9 067 | 3,7% | NIE | +0,8% |
| 1024 | krok 11264, colOffset 4096 | **tak** | **tak** | **tak** | **tak** | 8 962 | 3,5% | NIE | −0,4% |

CoreML **przyjmuje niekontigualny outputBacking i pisze wprost do niego**, także gdy
początek okna nie leży na granicy strony. Różnice czasu (−1,1…+5,1%) są w rozrzucie
kontigualnego (T=256 ma tu IQR 15–26% przez obciążenie maszyny; T=512/1024 są
w 1–5%). Jedyna widoczna alternatywa — kopia wierszy po stronie hosta — kosztuje
mierzalne +4,6…+8,1% (T×8 KiB przez memcpy). Czy CoreML wewnątrz kopiuje z bufora
tymczasowego do naszego, nie da się odróżnić od pisania ANE wprost (oba dają te same
bajty i ten sam czas); dla implementacji to bez znaczenia — koszt jest zerowy w
granicach pomiaru.

## Wnioski

1. **K0 niespełnione, i to strukturalnie.** Wagi MLX affine grupa 64 nie liczą się na
   ANE w żadnej reprezentacji `constexpr_*` dostępnej w coremltools 9.0 / macOS 26.6;
   kompilator ANE przyjmuje tylko per-channel symetryczne (int4/int8) lub per-channel
   z całkowitym zero-pointem (60% prędkości). Spadek na CPU daje 24% prędkości i 2×
   gorszą numerykę. Ścieżka ANE w Rust **musi re-kwantyzować** wycinek ANE z wag MLX
   do per-channel: int8 (1,6e-2 błędu wyjścia, 88–94% prędkości int4, 36 MiB na
   wycinek FFN) albo int4 (bez straty prędkości, ale 2,8e-1 błędu na wagach losowych —
   do zmierzenia na prawdziwych wagach zanim wejdzie do gry). Bit-exactness z MLX jest
   nieosiągalna niezależnie od kodowania (ANE i tak sumuje w fp16: 5,5e-3).
2. **Multifunction: tak.** Jeden mlpackage, jedne wagi na dysku (18 MiB zamiast 54),
   ładowanie i TFLOPS funkcji identyczne z osobnym modelem, wyjście bit w bit.
   Wadą jest tylko to, że każda funkcja kompiluje się dla ANE osobno (zimno ~200 ms
   każda), więc trzy kształty to trzy zimne ładowania — jak dla osobnych modeli.
3. **Strided output: tak.** `outputBackings` z krokiem 11264 pisze in-place, bez
   błędu, bez kopii mierzalnej, także z oknem niewyrównanym do strony. Kernel scatter
   i kopia hosta są zbędne; wynik ANE ląduje wprost w kolumnach wycinka bufora
   aktywacji [T, 11264].

## Nierozstrzygnięte

- **Błąd re-kwantyzacji na prawdziwych wagach 7B** (z outlierami) dla int8 i int4
  per-channel — tu zmierzony tylko na wagach gaussowskich. To rozstrzyga między int8
  (2× dysk, −6…−12% prędkości) a int4.
- **Gdzie `aned` trzyma skompilowane programy i wagi** — cache per-proces w
  `~/Library/Caches/<binarka>` dla modeli ANE ma 8 KB, a zimne/ciepłe różni się
  6–8×; miejsce cache systemowego nie zostało zlokalizowane (bez roota nie widać
  `footprint`/`vmmap` `aned`). Przez to pytanie „czy wagi multifunction są w pamięci
  raz" pozostaje bez pomiaru — dowód jest tylko dyskowy.
- **Pamięć systemowa** mierzona przy obciążeniu ±100–230 MiB (kompilacje obok);
  gdyby ktoś chciał rozstrzygnąć 36 MiB różnicy, trzeba pustej maszyny i `sudo footprint`.
- **Per-channel z zero-pointem uint4 na ANE liczy 60% int4** — nie sprawdzono, czy
  to koszt trwały, czy artefakt jednego przebiegu (IQR 6%); jeśli asymetria per-channel
  okaże się potrzebna numerycznie, wymaga własnego pomiaru.
- `powermetrics` (sudo) nadal niezebrane.

## Źródła

- EKS-A9 — `docs/pomiary/eks-a9-ane-wspolbieznie-m1.md` (protokół, kształt, int4 per-channel)
- coremltools, `constexpr_blockwise_shift_scale` / `constexpr_lut_to_dense` (opset iOS18) —
  https://apple.github.io/coremltools/source/coremltools.converters.mil.mil.ops.defs.html#module-coremltools.converters.mil.mil.ops.defs.iOS18.compression
- coremltools, multifunction — https://apple.github.io/coremltools/docs-guides/source/multifunction-models.html
- MLModelConfiguration.functionName — https://developer.apple.com/documentation/coreml/mlmodelconfiguration/functionname
- MLPredictionOptions.outputBackings — https://developer.apple.com/documentation/coreml/mlpredictionoptions/outputbackings
- MLX `mx.quantize` (affine, group_size, bits) — https://ml-explore.github.io/mlx/build/html/python/_autosummary/mlx.core.quantize.html
