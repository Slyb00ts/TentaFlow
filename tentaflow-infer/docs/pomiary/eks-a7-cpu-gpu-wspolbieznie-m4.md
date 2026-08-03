# EKS-A7 — czy CPU i GPU liczą prefill jednocześnie (Apple M4)

Pytanie wyszło od ANE: skoro Apple ma osobną jednostkę do mnożeń, czy nie
policzyć na niej prefillu równolegle z GPU. ANE odpada z powodu, który da się
podać jedną liczbą — jedyne wejście do niej to CoreML, a to kosztuje **20–24 ms
na wywołanie** plus ~7 µs na wiersz, przy naszych warstwach liczących 2–20 ms i
dispatchu Metala 0,61 µs. Praca badająca granice współwykonania na Apple Silicon
wyklucza ANE dokładnie na tej podstawie i bada parę CPU+GPU.

Ten dokument mierzy tę parę u nas.

Maszyna: Mac mini M4 (aktywnie chłodzony), 4 P + 6 E, 16 GB. Model: Bielik-7B
MLX 4-bit.

## Punkt wyjścia

| | |
|---|---:|
| prefill GPU, 256 tokenów | 215,6 tok/s = **3,02 TFLOPS** |
| sufit macierzowy GPU (EKS-A2) | 3,94 TFLOPS |
| pasmo pamięci (EKS-A1) | 102,4 GB/s |

Prefill wykorzystuje 77% mocy obliczeniowej GPU, a pasma prawie nie rusza — wagi
czyta raz na kafel, nie raz na token. **Brakuje mocy, nie pasma**, i to jest
warunek, przy którym dołożenie drugiej jednostki ma sens. Dekodowanie jest
odwrotne: 88,0 z 102,4 GB/s, czyli 86% pasma.

## Ile daje CPU

`cblas_sgemm` na kształtach warstwy Bielika, 256 tokenów:

| kształt | czas | |
|---|---:|---:|
| q_proj / o_proj [4096 x 4096] | 5 649 us | 1,52 TFLOPS |
| k_proj / v_proj [1024 x 4096] | 1 381 us | 1,56 |
| gate / up [11264 x 4096] | 15 237 us | 1,55 |
| down [4096 x 11264] | 17 247 us | 1,37 |

Podtrzymywane przez 25 s: 1,52 TFLOPS średnio, 319 rund bez osypywania się.
To **połowa tego, co obecnie robi GPU** — nie margines błędu.

f16 nie pomaga na prędkość: `BNNSMatMul` daje 1,41 TFLOPS f16 wobec 1,25 f32
(1,13x), czyli AMX nie ma podwojonej ścieżki f16, a i tak wypada poniżej
`cblas_sgemm` f32. f16 kupuje wyłącznie połowę pamięci.

## Czy się sumują

Test rozstrzygający: to samo obciążenie CPU puszczone RÓWNOLEGLE z prefillem na
GPU. Punkty odniesienia GPU pochodzą z bramki `no_batch_size_falls_off_a_cliff`,
czyli z czystego prefillu bez dekodowania.

| wsad | GPU sam | GPU + CPU | strata GPU |
|---:|---:|---:|---:|
| 8 | 12 678,9 us/tok | 12 789,4 | 0,9% |
| 16 | 12 011,7 | 12 079,7 | 0,6% |
| 32 | 11 784,0 | 11 807,2 | 0,2% |
| 64 | 5 208,7 | 5 221,6 | 0,2% |
| 128 | 4 769,4 | 4 790,8 | 0,4% |
| 256 | 4 629,6 | 4 719,8 | 1,9% |

CPU w tym samym oknie: **1,47 TFLOPS** wobec 1,52 osobno (−3%).

**Jednostki się sumują.** GPU traci medianowo 0,3%, CPU 3%. Łącznie dostępne
3,02 + 1,47 = **4,49 TFLOPS** tam, gdzie samo GPU daje 3,02.

Kontrola negatywna z tego samego przebiegu: dekodowanie pod obciążeniem CPU
spadło z **20,9 na 17,9 tok/s** (88,0 → 75,3 GB/s), czyli −14%. Dekodowanie jest
ograniczone pasmem, a pamięć jest wspólna — dokładanie mocy obliczeniowej nic
tam nie da i tylko szkodzi. **Podział wolno włączać wyłącznie w prefillu.**

## Czego brakuje w rachunku: wagi

GPU czyta 4 bity wprost w kernelu, Accelerate nie umie. Dwie drogi:

**Rozpakowanie w locie.** Kosztuje więcej, niż wynikałoby z liczby operacji
(1/256 pracy mnożenia), bo jest ograniczone zapisem 184 MB f32 i skalarne:

| | czas | narzut |
|---|---:|---:|
| samo mnożenie | 15 462 us | — |
| rozpakowanie, jeden wątek | 6 557 us | 41% |
| rozpakowanie, `dispatch_apply` | 4 127 us | **27%** |

Zrównoleglenie daje tylko 1,6x, bo wąskim gardłem jest zapis, nie arytmetyka.
Efektywnie **1,21 TFLOPS**, za to zero dodatkowej pamięci.

**Trwała kopia f16 przydziału CPU.** Zero kosztu na wywołanie, pełne 1,41
TFLOPS, ale przy 32% wierszy to +4,5 GB. Na tej maszynie (16 GB, model 4 GB)
zmieści się; na 8 GB nie.

## Wniosek

Przy podziale wierszy tak, żeby obie jednostki kończyły równocześnie:

| wariant | udział CPU | łącznie | sufit prefillu |
|---|---:|---:|---:|
| rozpakowanie w locie | 29% | 4,23 TFLOPS | +40% → 302 tok/s |
| trwała kopia f16 | 32% | 4,43 TFLOPS | +47% → 317 tok/s |

To są SUFITY, bez kosztu bariery synchronizacji na każdym mnożeniu (7 na
warstwę, 40 warstw = 280 barier na kafel) i bez uwagi, której się nie dzieli.
Literatura mierzy w tym samym układzie 1,15–1,38x na poziomie bloku, ale
**1,18–1,25x end-to-end na TTFT** — i ta druga liczba jest uczciwszą prognozą:
215,6 → **~260 tok/s wobec 218 u MLX**.

Znana pułapka: przy leniwym grafie bez jawnych punktów materializacji ten sam
podział daje 0,66x, czyli szkodzi. Nas to nie dotyczy — dispatchujemy zachłannie
do bufora poleceń, nie budujemy leniwego grafu jak MLX. To akurat jest przewaga,
którą mamy za darmo z wcześniejszej decyzji architektonicznej.

## Nierozstrzygnięte

Bariera synchronizacji jest jedyną dużą niewiadomą i nie da się jej zmierzyć bez
prototypu — trzeba realnie podzielić jedno mnożenie i porównać zegar oraz
zgodność co do bitu z obecną ścieżką.

## Źródła

- FusionML: Prefill, Not Decode — https://arxiv.org/html/2607.22785v1
- Disaggregated Inference on Apple Silicon (SqueezeBits) — https://blog.squeezebits.com/disaggregated-inference-on-apple-silicon-npu-prefill-and-gpu-decode-67176
- hybrid-ane-mlx-bench — https://github.com/AtomGradient/hybrid-ane-mlx-bench
