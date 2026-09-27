# EKS-A1 / EKS-A3 — harness pomiarowy Apple

Dwa eksperymenty rozstrzygające z `docs/PLAN_NAPRAWY.md` §7.7:

- **EKS-A1** — jaki procent katalogowej przepustowości pamięci oddaje kernel strumieniowy,
  ze sweepem po liczbie akumulatorów (ILP) i po rozmiarze siatki.
- **EKS-A3** — koszt dyspozycji wewnątrz command buffera, koszt osobnego command buffera
  i koszt powrotu na hosta. To rozstrzyga, czy fuzja kerneli jest na Apple dźwignią.

## Uruchomienie

```bash
./run.sh
```

Wymaga wyłącznie Xcode Command Line Tools (`swiftc` + framework Metal). Harness wypisuje
wynik w markdownie, gotowy do wklejenia do raportu w `docs/pomiary/`.

## Protokół

Zgodny z `PLAN_NAPRAWY.md` §9 N0: rozgrzewka 300 iteracji na tym samym kształcie co pomiar,
proces ciepły, 5 przebiegów z odrzuceniem pierwszego, mediana i IQR, znacznik `ważny` przy
`IQR/mediana ≤ 3%`. Stan termiczny zapisywany przed i po — to odpowiednik `pp_dpm_mclk`
z protokołu AMD i pomiar wykonany po throttlingu nie jest porównywalny.

Wyniki z 2026-08-02 (Apple M4, 10 rdzeni GPU): `docs/pomiary/eks-a1-a3-apple-m4.md`.

## EKS-A9 — ANE jako trzecia jednostka prefillu

Sprawdza lokalnie liczbę „CoreML kosztuje 20–24 ms na wywołanie”, na której EKS-A7
odrzucił ANE. Dwa pliki:

- `eks_a9_gen.py` — generuje modele CoreML (wycinek FFN 7B: gate/up [3072×4096],
  silu·up, down [4096×3072]; T = 256/512/1024; wagi fp16 / int4 per-channel / LUT4;
  modele trywialne do narzutu; warianty z wejściem obrazowym pod IOSurface) i zapisuje
  wagi zdekwantyzowane do referencji fp32.
- `eks_a9_ane.swift` — harness: narzut predict, TFLOPS na ANE, MLComputePlan (czy liczy
  ANE), MLMultiArray wobec CVPixelBuffer, współbieżność ANE+GPU+CPU, numeryka wobec
  `cblas_sgemm`.

```bash
uv venv --python 3.12 /tmp/a9venv && uv pip install --python /tmp/a9venv/bin/python coremltools numpy
/tmp/a9venv/bin/python eks_a9_gen.py /tmp/a9models
./run.sh a9 /tmp/a9models            # wszystkie sekcje
./run.sh a9 /tmp/a9models concurrent # jedna sekcja: overhead|ffn|dist|units|plan|io|concurrent|numeric
```

Wymaga macOS 15+ (MLComputePlan, opset iOS18 z wagami 4-bitowymi na ANE). `powermetrics`
wymaga sudo — jeśli hasło jest potrzebne, sekcja jest pomijana i raport to odnotowuje.
Protokół jak wyżej (N0). Wyniki z 2026-09-11 (Apple M1): `docs/pomiary/eks-a9-ane-wspolbieznie-m1.md`.

## EKS-A10 faza 0 — blockwise / multifunction / wyjście z krokiem

Trzy sondy przed implementacją ścieżki ANE w Rust (plan EKS-A10): czy 4-bitowe wagi
z grupą 64 wzdłuż K (format MLX affine) liczą się na ANE, czy jeden mlpackage
multifunction (T256/T512/T1024) dzieli wagi, i czy `outputBackings` przyjmuje
tablicę z krokiem (wynik wprost w szerszym buforze). Pliki:

- `eks_a9_gen.py --a10` — modele fazy 0 na tych samych wagach co A9: `int4`
  (per-channel, jak A9), `blockwise` (grupa 64, `constexpr_blockwise_shift_scale`,
  offset f16 = −bias/scale), `blockwise_zp` (grupa 64, offset uint4), `affine16`
  (int8 per-channel, `constexpr_affine_dequantize` iOS16; w coremltools 9.0 ten op
  nie przyjmuje 4 bitów) oraz `ffn_multi_<wariant>` (`MultiFunctionDescriptor` +
  `save_multifunction`, funkcje `T256`/`T512`/`T1024`).
- `eks_a10_probe_gen.py` — 17 dodatkowych kodowań (grupy 64…2048 wzdłuż K i N, offset
  per-channel f16/uint4, LUT per-grupa, int8 z grupą, `matmul`, K rozbite na kawałki,
  re-kodowanie wag MLX do int8/int4 per-channel) do `probe.txt`.
- `eks_a10_ane.swift` — harness: `plan` (MLComputePlan każdego modelu i każdej funkcji),
  `probe` (dodatkowe kodowania z `probe.txt`), `ffn` (TFLOPS kodowań PRZEPLATANE:
  warianty załadowane naraz, 9 rund A,B,C,D po 20 predict, pierwsza odrzucona),
  `numeric` (wobec `cblas_sgemm` na wagach zdekwantyzowanych przez coremltools i wg
  wzoru MLX q·scale+bias), `multi` (rozmiary, ładowanie z `functionName`, cache ANE
  `~/Library/Caches/eks_a10_ane`, TFLOPS funkcji wobec osobnego modelu),
  `multimem` / `multimem-rev` (footprint procesu, RSS `aned`, `vm_stat`),
  `strided` (outputBackings z krokiem 11264 na buforze [T,11264], kontrola in-place
  po wskaźniku, bajty wobec wyjścia kontigualnego, wartownik poza oknem).

```bash
venv/bin/python eks_a9_gen.py /tmp/a10models --a10
rm -rf ~/Library/Caches/eks_a10_ane      # zimne ładowanie ANE
./run.sh a10 /tmp/a10models multi        # sekcje: plan|probe|ffn|numeric|multi|multimem|multimem-rev|strided
```

Wynik w skrócie (2026-09-11, Apple M1, `docs/pomiary/eks-a10-faza0-ane-m1.md`):
blockwise grupa 64 **spada na CPU** (24% prędkości int4 per-channel) — ANE przyjmuje
tylko per-channel symetryczne int4/int8, więc wagi MLX trzeba re-kwantyzować;
multifunction **dzieli wagi** (18 MiB zamiast 54) i liczy tak samo jak osobny model;
strided `outputBackings` **działa in-place** bez kosztu.

## EKS-A10 — 80 modeli ANE naraz (czy CoreML liczy na CPU?)

`eks_a10_multi.swift` — diagnoza spowolnienia predict przy wielu załadowanych modelach
produkcyjnych (`.runtime/ane/bielik-minitron-7b-s060-int8/`, 80 mlmodelc multifunction).
Ładuje N modeli (T1024, `cpuAndNeuralEngine`), mierzy per predict czas ścienny, czas CPU
procesu (`getrusage`, osobno system), błędy stron, czas CPU wątków (`thread_info`),
`vm_stat`/RSS `aned`, zlicza komunikaty E5RT (stdout i stderr), MLComputePlan po
załadowaniu wszystkich. Kontrole: `fn=T256`, `source=same-path|copies|compiled`
(`pkg=<katalog mlpackage>` z `ane_export.py --keep-mlpackage`), `units=all`, `lowprec=1`,
`backings=1`, `ballast=<MiB>` (wątek trzymający pamięć jak wagi silnika), `sweep-inproc`
(zwalnianie między N).

```bash
./run.sh a10multi <katalog_modeli> sweep                   # N = 1,2,5,10,20,40,80, każde w nowym procesie
./run.sh a10multi <katalog_modeli> run n=80 plan=1         # jeden przebieg + MLComputePlan
./run.sh a10multi <katalog_modeli> run n=80 ballast=4096   # z 4 GiB balastu
```

Wynik w skrócie (2026-09-11, Apple M1 16 GB, `docs/pomiary/eks-a10-wiele-modeli-ane-m1.md`):
**CPU nie liczy** (user 0,2–0,5 ms/predict, plan 80/80 na ANE, ostrzeżenie E5 nie wystąpiło);
80 instancji tego samego pliku = 14,9 ms jak jeden model; spowolnienie idzie za **pamięcią
wired** (wagi 3,1 GB + bufory we/wy ANE 2,4 GB przy T1024 = +6 GB) → swap, czas **system**
na wątku głównym: N=80 18–26 ms, z 4 GiB balastu 36–40 ms (31 ms system, 9–11 tys. błędów
stron/predict). `compileModel`, `all`, `lowprec`, `outputBackings` — bez zmian; pomaga tylko
mniej wired (T256, ≤40 modeli, brak dublowania wag w silniku).
