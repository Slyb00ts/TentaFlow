# EKS-A9 — czy ANE może być trzecią jednostką prefillu (Apple M1)

EKS-A7 odrzucił Neural Engine jedną liczbą z literatury: „CoreML kosztuje
20–24 ms na wywołanie". Ta liczba nie była zmierzona lokalnie. Ten dokument
mierzy ją — i wszystko, co trzeba, żeby ANE dostało albo straciło miejsce obok
GPU i CPU w prefillu.

Maszyna: Apple M1 (8 rdzeni, 4 P + 4 E, GPU 8 rdzeni, pasywnie chłodzony),
16 GB, macOS 26.6.2 (25G83), Xcode / Swift 6.3.3, coremltools 9.0. Stan
termiczny (`ProcessInfo` + `pmset -g therm`) był **nominal bez ostrzeżeń przed,
między i po każdej serii** — nie ma wyników zebranych po throttlingu.

Kształt: wycinek FFN modelu 7B (d_model 4096, inter 11264), ANE liczy
Ns = 3072 wierszy wymiaru inter (27%): gate [3072×4096], up [3072×4096],
silu(gate)·up, down [4096×3072], wejście x [T×4096] f16, wyjście [T×4096] f16.
Osobny model o stałym kształcie dla T = 256 / 512 / 1024 (bez enumerated
shapes). FLOP na wywołanie = 2·T·4096·3072·3. Wagi losowe N(0, 0,02), x N(0, 1).

Narzędzia: `tools/eks-apple/eks_a9_gen.py` (modele, MIL Builder → mlprogram
fp16, opset macOS15; int4 przez `linear_quantize_weights` per_channel,
LUT4 przez `palettize_weights` kmeans per_tensor; `xcrun coremlcompiler`),
`tools/eks-apple/eks_a9_ane.swift` (harness, `./run.sh a9 <modele> [sekcja]`).
Protokół N0: rozgrzewka 300 wywołań na tym samym kształcie, 5 przebiegów po 20
wywołań z odrzuceniem pierwszego, mediana i IQR, „ważny" przy IQR/mediana ≤ 3%.

## Krótko

| | literatura (EKS-A7) | zmierzone na M1 |
|---|---:|---:|
| narzut jednego predict na ANE | 20–24 ms | **~0,27 ms** |
| narzut predict, gdy CoreML kieruje na CPU | — | 36 µs |
| FFN T=1024, wagi int4 | — | **8,1–8,4 TFLOPS** |
| FFN T=1024, wagi fp16 | — | 5,6 TFLOPS |
| sufit macierzowy GPU M1 (kernel EKS-A2, bez pamięci) | — | 2,30 TFLOPS |
| GPU + ANE naraz: strata GPU / ANE | — | 11% / 15–22% (GEMM z pamięci), 1,6% / 7,7% (GPU bez pamięci) |
| błąd ANE wobec fp32 (względna L2) | — | 5,5e-3 |

Liczba z literatury była **o dwa rzędy wielkości za wysoka** dla tej ścieżki.
ANE na M1 liczy ten wycinek 3,5× szybciej niż sufit arytmetyczny GPU tej samej
maszyny i sumuje się z GPU, dopóki GPU nie jest ograniczone pasmem.

## 1. Narzut jednego predict

Model trywialny [wiersze,dim]·[dim,dim] fp16. Poniżej [64,1024] CoreML kieruje
program na CPU niezależnie od `computeUnits` (MLComputePlan: `linear:CPU`),
więc pierwsze wiersze mierzą narzut ścieżki CPU, a narzut ANE czyta się od
`trivial_64x1024` w górę. Mediana z 200 wywołań na przebieg.

| model | plan | cpuAndNeuralEngine [µs] | IQR | all [µs] | cpuOnly [µs] |
|---|---|--:|--:|--:|--:|
| 1×64 | CPU | 36,4 | 9,8% (NIE) | 36,0 | 36,0 |
| 1×256 | CPU | 36,4 | 0,3% | 36,5 | 36,7 |
| 8×256 | CPU | 39,2 | 5,6% (NIE) | 39,0 | 38,4 |
| 1×1024 | CPU | 43,5 | 1,8% | 44,0 | 44,1 |
| 8×1024 | CPU | 64,1 | 2,3% | 64,1 | 64,1 |
| **64×1024** (134 MFLOP) | **ANE** | **304,8** | 0,3% | 307,2 | 89,0 |
| 128×1024 (268 MFLOP) | ANE | 315,1 | 1,2% | 311,3 | 138,5 |
| 256×1024 (537 MFLOP) | ANE | 337,2 | 1,5% | 342,1 | 237,5 |
| 512×1024 (1074 MFLOP) | ANE | 464,5 | 3,1% (NIE) | 455,4 | 440,7 |
| 64×2048 (537 MFLOP, wagi 8 MiB) | ANE | 428,4 | 1,0% | 479,2 | 334,1 |
| 64×4096 (2,1 GFLOP, wagi 32 MiB) | ANE | 847,7 | 1,6% | 855,7 | 1473,0 |

Regresja czasu po FLOP dla serii `·1024` na ANE: nachylenie 0,173 µs/MFLOP
(5,8 TFLOPS przyrostowo), **punkt przecięcia 268 µs**. To jest koszt jednego
predict na ANE: kolejka do `aned`, zlecenie, powrót. Wobec 0,61 µs dyspozycji
Metala (EKS-A3) to dużo; wobec warstw liczących 2–20 ms to 1–13%, a wobec
20–24 ms z literatury — 80× mniej.

Seria `64×dim` pokazuje drugi koszt: przy 64 wierszach czas rośnie z wagami,
nie z FLOP (32 MiB wag → +540 µs), czyli ANE czyta wagi z pamięci przy każdym
wywołaniu i przy małym T jest ograniczone ich pasmem (~60 GB/s). Dlatego dalej
liczy się T ≥ 256.

## 2. Wycinek FFN na ANE

Dwa pełne przebiegi sekcji (osobne procesy, 30 s przerwy) — bo połowa pomiarów
przy T ≥ 512 nie mieści się w 3% IQR i trzeba pokazać, że to własność
jednostki, a nie jednego przebiegu. Kolumna „footprint" to przyrost
`phys_footprint` procesu po `MLModel(contentsOf:)` / po pierwszym predict.

| T | wagi | mlmodelc | +footprint ładowanie / 1. predict | przebieg 1 [µs] | IQR | przebieg 2 [µs] | IQR | TFLOPS (1 / 2) |
|--:|---|--:|---|--:|--:|--:|--:|--:|
| 256 | fp16 | 72,0 MiB | +5,4 / +11,5 MiB | 3 807 | 0,3% | 3 828 | 2,2% | 5,08 / 5,05 |
| 256 | int4 | 18,0 | +0,1 / +4,2 | **2 278** | 1,3% | 2 400 | 0,7% | **8,48 / 8,05** |
| 256 | lut4 | 18,0 | +0,0 / +4,1 | 2 303 | 0,9% | 2 442 | 6,7% (NIE) | 8,39 / 7,91 |
| 512 | fp16 | 72,0 | +0,1 / +8,1 | 7 193 | 0,8% | 7 142 | 0,5% | 5,37 / 5,41 |
| 512 | int4 | 18,0 | +0,1 / +8,1 | 4 809 | 32,2% (NIE) | 4 740 | 20,3% (NIE) | 8,04 / 8,16 |
| 512 | lut4 | 18,0 | +0,0 / +8,0 | 5 055 | 11,1% (NIE) | **4 664** | 1,7% | 7,65 / **8,29** |
| 1024 | fp16 | 72,0 | +0,0 / +16,1 | 13 777 | 11,5% (NIE) | 13 671 | 11,4% (NIE) | 5,61 / 5,65 |
| 1024 | int4 | 18,0 | +0,0 / +16,0 | 9 196 | 5,6% (NIE) | 9 512 | 11,0% (NIE) | 8,41 / 8,13 |
| 1024 | lut4 | 18,0 | +0,0 / +16,0 | 10 385 | 13,0% (NIE) | 8 775 | 34,7% (NIE) | 7,44 / 8,81 |

`computeUnits = .all` daje te same liczby w granicach rozrzutu (T=1024 int4:
10 273 / 9 610 µs; fp16: 14 215 / 13 607) — CoreML i tak kieruje wszystko na
ANE (§3), więc `.all` niczego nie zmienia poza dodatkowym mapowaniem wag do
procesu w części przebiegów (+48–97 MiB przy fp16, nie zawsze).

**fp16 wobec int4 przy tych samych FLOP: 0,6× czasu.** Gdyby wagi 4-bitowe
były rozpakowywane do fp16 przy ładowaniu, czas byłby równy fp16. Nie jest —
ANE czyta je skompresowane. Pamięcią procesu tego nie widać: przyrost footprintu
po załadowaniu to 0,0–0,1 MiB dla 18 MiB modelu i 0–5 MiB dla 72 MiB, a po
pierwszym predict dokładnie 2·T·4096·2 B (bufory we/wy). **Wagi ANE żyją poza
footprintem procesu** (mapowane przez `aned`), więc RSS nie odpowiada na pytanie
o rozpakowanie; odpowiada czas.

### Rozrzut nie jest błędem pomiaru

Rozkład 400 pojedynczych predict po 300 rozgrzewkowych, `cpuAndNeuralEngine`:

| model | min [µs] | p10 | mediana | p90 | p99 | max | > 1,5·mediana |
|---|--:|--:|--:|--:|--:|--:|--:|
| T=1024 int4 | 8 156 | 8 361 | **8 763** | 10 881 | 12 331 | 12 993 | 0,0% |
| T=1024 fp16 | 13 006 | 13 485 | **14 397** | 15 370 | 18 294 | 62 569 | 0,5% |
| T=512 int4 | 4 034 | 4 174 | **4 322** | 4 673 | 5 085 | 19 667 | 0,2% |
| T=256 int4 | 2 139 | 2 192 | **2 275** | 2 508 | 2 814 | 3 021 | 0,0% |

To nie pojedyncze wyskoki (udział > 1,5·mediany ≤ 0,5%), tylko szeroka chmura:
p10→p90 to +30% przy T=1024, +12% przy T=512, +14% przy T=256; średnie kolejnych
50 wywołań wędrują 8,98–10,02 ms bez trendu. Najszybsze wywołanie T=1024 int4
to 8 156 µs = **9,5 TFLOPS**, mediana pojedynczych 8 763 µs = 8,8 TFLOPS.
Wskazówka co do przyczyny jest w §5: z obciążonym CPU obok ANE liczy **o 9%
szybciej i z IQR 0,3–1,0%** — ścieżka predict ma część na CPU i cierpi na jego
DVFS, gdy proces poza tym śpi na oczekiwaniu.

### Kontrola: ten sam model na innych jednostkach (T=512 int4)

| computeUnits | plan | mediana [µs] | IQR | TFLOPS |
|---|---|--:|--:|--:|
| cpuAndNeuralEngine | 6 ops ANE | **4 355** | 0,8% | **8,88** |
| cpuAndGPU | 6 ops GPU | 21 332 | 0,6% | 1,81 |
| cpuOnly | 6 ops CPU | 17 113 | 5,5% (NIE) | 2,26 |

Ten sam program, ta sama maszyna: ANE 4,9× szybciej niż GPU przez CoreML
i 3,9× szybciej niż CPU. Liczba 8,9 TFLOPS nie jest osiągalna niczym innym na
M1 — GPU przez Metal ma sufit 2,30 TFLOPS (§5), CPU przez `cblas_sgemm` 0,66.

## 3. Czy liczy ANE — MLComputePlan

`MLComputePlan.load(contentsOf:configuration:)` → `deviceUsage(for:).preferred`
dla każdej operacji `main` (w tym SDK pole nazywa się `preferred`, nie
`preferredComputeDevice`). Wszystkie 18 modeli FFN i 3 obrazowe, dla
`cpuAndNeuralEngine` i `.all`:

| model | linear ×3, silu, mul, identity (+ reshape ×2 w obrazowych) | const / constexpr | koszt szacowany na ANE |
|---|---|---|--:|
| ffn_T{256,512,1024}_{fp16,int4,lut4} | **wszystkie ANE** | brak przydziału (stałe) | **100%** |
| ffn_img_T{256,512,1024}_int4 | wszystkie ANE | brak | 100% |
| trivial_≤8×1024 | wszystkie CPU | — | 0% |

Wagi 4-bitowe są w programie jako `constexpr_blockwise_shift_scale` (int4) i
`constexpr_lut_to_dense` (LUT4) — czyli operacje dekompresji opset iOS18, które
ANE przyjmuje bez wstawiania niczego na CPU/GPU. Żadna operacja żadnego modelu
FFN nie trafiła na GPU ani na CPU, w żadnej konfiguracji.

`powermetrics --samplers ane_power,gpu_power` **nie został zebrany**: `sudo -n`
wymaga na tej maszynie hasła. Potwierdzenie ANE opiera się na MLComputePlan
i na kontroli czasów `cpuOnly` / `cpuAndGPU` / `cpuAndNeuralEngine` powyżej,
które są rozłączne o czynnik 4–5.

## 4. Wejście i wyjście: MLMultiArray wobec CVPixelBuffer (IOSurface)

Graf rank-4 [1,1,T,4096] jest identyczny; różni się tylko opis interfejsu
(MultiArray wobec obraz `GRAYSCALE_FLOAT16`). Wariant obrazowy zbudowany przez
przepisanie `imageType` w spec — MIL Builder z `source="milinternal"` nie
przyjmuje `ImageType` w `ct.convert`, a to jest dokładnie to, co `ct.convert`
robi wewnętrznie. Bufor: `CVPixelBufferCreate` z
`kCVPixelBufferIOSurfacePropertiesKey`, format `OneComponent16Half`, 4096
kolumn × T wierszy; `CVPixelBufferGetIOSurface` ≠ nil sprawdzone. Wyjście też
obrazem, przez `outputBackings`. Model int4.

| T | (a) MLMultiArray CoreML, wyjście CoreML | (a2) MLMultiArray na buforze wyrównanym do strony + `outputBackings` | (b) CVPixelBuffer IOSurface we/wy |
|--:|--:|--:|--:|
| 256 | 2 407 µs, IQR 14,7% (NIE) | 2 316, 3,3% (NIE), −3,8% | **2 261**, 0,4%, −6,1% |
| 512 | 5 590, 18,1% (NIE) | **4 261**, 0,7%, −23,8% | 4 815, 5,0% (NIE), −13,9% |
| 1024 | 9 670, 9,0% (NIE) | **8 401**, 1,8%, −13,1% | 9 434, 2,3%, −2,4% |

Wyjście przez obraz jest **bit w bit** równe wyjściu przez tablicę
(max |Δ| = 0 dla każdego T). Różnice czasów mieszczą się w rozrzucie ANE z §2
(p10→p90 to 12–30%), więc twierdzenie „IOSurface jest szybszy" nie ma tu
oparcia. To, co się powtarza we wszystkich trzech kształtach: (a2) jest zawsze
nie gorsze od (a) i ma ciasny IQR, a (b) nie jest lepsze od (a2). Praktyczny
wniosek: **własny bufor wyrównany do strony z `outputBackings` wystarcza**;
obraz na IOSurface niczego mierzalnego nie dodaje, a wymaga innego typu
interfejsu. Kopia we/wy przy T=1024 to 2×8 MiB — przy 60 GB/s ~270 µs, tyle
co narzut predict, i to jest górna granica tego, co bufory mogą oddać.

## 5. Współbieżność: GPU + ANE + CPU w jednym procesie

Wątek A: pętla predict `ffn_T1024_int4` na ANE. Wątek B: GEMM
[1024×4096]·[4096×4096] f16 na Metalu przez `simdgroup_matrix`, 8 mnożeń na
jeden bufor poleceń, host czeka raz (kontrola poprawności kernela: błąd
względny 1,1e-5). Wątek C: `cblas_sgemm` f32 tego samego kształtu. Kontrola
negatywna: kernel z EKS-A2 (łańcuchy `simdgroup_multiply_accumulate` w
rejestrach, zero ruchu pamięci). Okno 3 s, 5 okien, pierwsze odrzucone, mediana
TFLOPS każdego wątku. Dwa niezależne przebiegi.

| warunek | wątek | przebieg 1 | IQR | przebieg 2 | IQR | strata (1 / 2) |
|---|---|--:|--:|--:|--:|--:|
| GPU sam | GPU | 1,768 | 0,1% | 1,756 | 0,3% | — |
| ANE sam | ANE | 7,535 | 3,3% (NIE) | 7,680 | 2,5% | — |
| CPU sam | CPU | 0,645 | 4,7% (NIE) | 0,661 | 3,0% | — |
| **GPU + ANE** | GPU | 1,583 | 4,1% (NIE) | 1,566 | 0,4% | **10,5% / 10,8%** |
| | ANE | 6,374 | 2,0% | 6,004 | 0,7% | **15,4% / 21,8%** |
| GPU + CPU | GPU | 1,758 | 0,1% | 1,748 | 0,6% | 0,5% / 0,5% |
| | CPU | 0,606 | 0,8% | 0,589 | 4,7% (NIE) | 5,9% / 10,8% |
| ANE + CPU | ANE | 8,254 | 1,0% | 8,394 | 0,3% | **−9,5% / −9,3%** (zysk) |
| | CPU | 0,566 | 1,0% | 0,603 | 0,2% | 12,2% / 8,7% |
| **GPU + ANE + CPU** | GPU | 1,543 | 0,4% | 1,590 | 3,2% (NIE) | 12,8% / 9,5% |
| | ANE | 6,584 | 0,1% | 6,924 | 4,9% (NIE) | 12,6% / 9,8% |
| | CPU | 0,452 | 0,4% | 0,446 | 8,7% (NIE) | 29,9% / 32,6% |
| GPU-ALU sam (EKS-A2, bez pamięci) | GPU | — | | 2,302 | 1,8% | — |
| **GPU-ALU + ANE** | GPU | — | | 2,267 | 3,1% (NIE) | **1,6%** |
| | ANE | — | | 7,088 | 0,8% | **7,7%** |

Łącznie w trójce: 1,54 + 6,58 + 0,45 = **8,6 TFLOPS** tam, gdzie samo GPU daje
1,77, a GPU + CPU (dzisiejsza ścieżka EKS-A7) 2,36. Jednostki się sumują, ale
nie za darmo, i strata ma nazwisko:

**Walka idzie o pasmo, nie o ANE.** Kernel GPU bez ruchu pamięci traci przy
ANE 1,6%, a GEMM czytający fragmenty z pamięci urządzenia — 11%. Mój kernel
GEMM jest naiwny (fragmenty ładowane wprost z pamięci, ~2 GB ruchu na mnożenie
przy 1,77 TFLOPS), więc 11% to **górna granica** dla kernela zbliżonego do
produkcyjnego, który kafluje przez pamięć grupową. ANE traci więcej (8–22%),
bo przy 8 TFLOPS z 18 MiB wag na wywołanie samo jest bliżej pasma.

**CPU obok ANE pomaga ANE.** +9% i IQR z 3% na 0,3–1,0% — jedyne wyjaśnienie,
które pasuje, to DVFS: obciążony rdzeń trzyma częstotliwość, na której ścieżka
CoreML → `aned` → ANE ma krótszy odcinek CPU. To ta sama obserwacja, co szeroka
chmura w §2. Kosztuje to CPU 9–12%.

**CPU traci najwięcej w trójce (30–33%)** i to jest uczciwa cena: `cblas_sgemm`
na M1 daje tylko 0,65 TFLOPS (EKS-A7 na M4: 1,52), a dzieli pasmo z dwiema
jednostkami, które biorą go po 1,5 i 6,5 TFLOPS. Na M1 CPU jest najsłabszym
z trzech, a nie drugim, jak na M4.

## 6. Numeryka (T=512)

Referencja: `cblas_sgemm` f32 na CPU na **tych samych wagach, które zapisał
coremltools** (zdekwantyzowanych przez `decompress_weights`, więc dla int4/LUT4
referencja zawiera błąd kwantyzacji, a różnica mierzy wyłącznie arytmetykę
jednostki). Osobno: sama kwantyzacja, czyli referencja wariantu wobec
referencji fp16.

| wagi | wykonawca | względna L2 | max \|Δ\| | max \|ref\| |
|---|---|--:|--:|--:|
| fp16 | ANE (`cpuAndNeuralEngine` = `.all`) | **5,57e-3** | 3,10e-2 | 5,52 |
| fp16 | CoreML `cpuOnly` | 1,13e-2 | 7,42e-2 | 5,52 |
| int4 | kwantyzacja sama | 2,78e-1 | 1,56 | 5,52 |
| int4 | ANE | **5,48e-3** | 3,38e-2 | 5,89 |
| int4 | CoreML `cpuOnly` | 1,01e-2 | 8,33e-2 | 5,89 |
| lut4 | kwantyzacja sama | 1,70e-1 | 0,99 | 5,52 |
| lut4 | ANE | **5,61e-3** | 3,33e-2 | 5,45 |
| lut4 | CoreML `cpuOnly` | 1,14e-2 | 7,71e-2 | 5,45 |

ANE liczy z względną L2 5,5e-3 wobec f32 niezależnie od formatu wag — to błąd
aktywacji fp16 na wejściu/wyjściu i wewnątrz (silu·up jest w fp16), max |Δ|
0,6% rozpiętości wyjścia. Dla porównania: podział CPU/GPU z EKS-A7 dał 1,15e-2
na logitach. Ścieżka CoreML na CPU jest **dwa razy gorsza** od ANE (1,1e-2) —
pewnie sumuje w fp16. Kwantyzacja per-channel int4 na wagach gaussowskich bez
outlierów daje 28% błędu wyjścia, LUT4 17%; to własność wag losowych i
granulacji per-channel, nie ANE, i w prawdziwym modelu należy mierzyć na
prawdziwych wagach z grupami 32/64 — czego ten eksperyment nie robił.

## Wnioski

1. **Powód odrzucenia ANE w EKS-A7 był fałszywy.** Narzut predict na ANE to
   ~270 µs, nie 20–24 ms. Przy warstwie FFN 7B liczącej na ANE 9–14 ms to
   2–3%.
2. **ANE na M1 to 8–9 TFLOPS na int4 i 5,5 na fp16** przy T ≥ 256 — 3,5–4×
   sufit macierzowy GPU tej maszyny (2,30) i 13× jej CPU (0,65). Na M1 ANE nie
   jest „trzecią" jednostką, tylko pierwszą; GPU byłoby drugą.
3. **Sumują się**: GPU + ANE + CPU dają 8,6 TFLOPS wobec 2,36 dla GPU + CPU.
   Strata GPU 10% pochodzi z pasma (kernel bez pamięci traci 1,6%), więc
   z kaflowanym kernelem powinna być mniejsza; ANE traci 8–22%.
4. **4-bitowe wagi są czytane skompresowane** (0,6× czasu fp16 przy równych
   FLOP), a ich pamięć nie obciąża procesu.
5. **Numerycznie w porządku**: 5,5e-3 względnej L2 wobec f32, mniej niż
   różnica CPU/GPU zaakceptowana w EKS-A7.
6. **IOSurface nie jest dźwignią**: bufor wyrównany do strony z
   `outputBackings` daje to samo, obraz nic nie dokłada.

Co to znaczy dla prefillu: ANE liczy wycinek FFN o stałym kształcie, wynik
wraca do procesu jako f16 i trzeba go dodać do sumy cząstkowej GPU — dokładnie
tak, jak ścieżka CPU z EKS-A7 dodaje swój wycinek wierszy. Różnice: (a) każdy
kształt T to osobny skompilowany model (kafle 256/512/1024 pokrywają wszystko,
co prefill dziś wysyła), (b) 270 µs na wywołanie, czyli podział na poziomie
całego FFN warstwy, nie pojedynczego GEMM, (c) wagi wycinka ANE muszą być w
formacie CoreML — druga kopia 4-bitowa, ~27% FFN, dla 7B około 0,5 GB.

## Nierozstrzygnięte

- **Rozrzut przy T ≥ 512** (IQR 5–35%, p10→p90 +30%) nie ma zmierzonej
  przyczyny — hipoteza DVFS opiera się na tym, że obciążony CPU obok
  przyspiesza ANE o 9% i zbija IQR do 1%. Powinno się to sprawdzić wątkiem
  spinującym pusto na jednym rdzeniu (bez pracy) i `powermetrics` z sudo.
- **`powermetrics` nie zebrane** (sudo z hasłem). Pobór ANE/GPU podczas
  współbieżności jest nieznany; na pasywnie chłodzonym M1 to ważne przy
  dłuższych seriach, choć w tym eksperymencie stan termiczny nie opuścił
  „nominal".
- **Kernel GPU jest naiwny.** Strata 11% GPU przy ANE to górna granica dla
  kernela pasmożernego; produkcyjny kernel projektu nie był tu użyty (zakaz
  dotykania Rusta), więc liczba dla niego jest między 1,6% a 11%.
- **Uwaga i normy** nie były mierzone na ANE — tylko FFN. Uwaga ma dynamiczny
  kształt po długości sekwencji i wymagałaby enumerated shapes, których ten
  eksperyment celowo nie badał.
- **CPU na M1 daje 0,65 TFLOPS f32** (`cblas_sgemm` [1024×4096]·[4096×4096]),
  wobec 1,52 na M4. Ścieżka CPU z EKS-A7 jest na M1 warta 2,4× mniej, a przy
  ANE i GPU obok traci trzecią część — na tej maszynie jej udział powinien być
  przeliczony, nie skopiowany z M4.
- Modele mają wejście [T,4096] rank-2; nie sprawdzono, czy układ rank-4
  [1,4096,1,T] (kanały na osi 1, „natywny" dla ANE) coś zmienia. Rank-4
  [1,1,T,4096] z §4 liczy w tym samym czasie co rank-2.

## Źródła

- EKS-A7 — `docs/pomiary/eks-a7-cpu-gpu-wspolbieznie-m4.md` (liczba 20–24 ms
  i cytowane tam prace: FusionML, SqueezeBits, hybrid-ane-mlx-bench)
- EKS-A2 — `docs/pomiary/eks-a2-simdgroup-matrix-m4.md` (kernel ALU użyty
  jako kontrola negatywna)
- MLComputePlan — https://developer.apple.com/documentation/coreml/mlcomputeplan
- coremltools, kompresja wag (linear_quantize_weights, palettize_weights,
  decompress_weights) — https://apple.github.io/coremltools/docs-guides/source/opt-overview.html
- MLPredictionOptions.outputBackings —
  https://developer.apple.com/documentation/coreml/mlpredictionoptions/outputbackings
