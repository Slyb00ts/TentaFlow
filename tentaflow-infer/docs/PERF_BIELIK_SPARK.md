# Bielik-7B-NVFP4 na DGX Spark — profil prefillu i granica obecnej ścieżki

Pomiary na GB10 (sm_121a, 48 SM, pamięć zunifikowana 121 GiB),
`forge bench --weights-pool-gb 24`, model `TentaFlow/Bielik-PL-Minitron-7B-NVFP4`.

## Stan

| prompt | prefill tok/s | decode tok/s |
|---|---|---|
| 512 | 2 989 | 40,5 |
| 2 048 | 3 192 | 38,5 |
| 4 096 | 2 877 | 35,8 |

Odniesienie na tym samym sprzęcie i modelu: **vLLM 0.26 daje 48 tok/s decode**
oraz ~10 000 tok/s prefillu na zimno (mierzone przez HTTP, więc lekko zaniżone
dla vLLM).

Prefill 3 192 tok/s przy 2048 tokenach to 2·7e9·2048 / 0,64 s = **44,7 TFLOPS**.

## Ścieżka wykonania

`FORGE_GEMM` bez wartości włącza automatycznie hybrydowy prefill FP8:
projekcje Q/O/gate/up/down NVFP4 i `lm_head` są przepakowywane do FP8 na GPU
(7 GB paczek, 0,17 s). Zmierzony wpływ:

| wariant | prefill tok/s |
|---|---|
| auto (FP8) | 3 002 |
| `fp8mod-ffn` (to samo, wymuszone) | 2 983 |
| `nvfp4` (surowe, bez paczek) | 2 055 |

Decode jest identyczny we wszystkich wariantach (38,1–38,3), bo zostaje na NVFP4.

## Trzy GEMM-y prefillu nie są równe

`bench_fp8_modular_tiles.mojo`, M=1024, najlepszy kafel:

| projekcja | kształt (N,K) | FLOP | czas | TFLOPS |
|---|---|---|---|---|
| q/o | 4096, 4096 | 34,4 G | 235,6 µs | 146 |
| down | 4096, 11264 | 94,5 G | 867,0 µs | 109 |
| gate/up | 11264, 4096 | 94,5 G | 2 025,7 µs | **46,6** |

`down` i `gate/up` mają **identyczną liczbę operacji i identyczny ruch operandów**
(oba czytają 44 MiB wag i 184 MB aktywacji po zsumowaniu kafli), a różnią się
2,3-krotnie. Jedyna różnica strukturalna to liczba kafli kolumnowych: 16 wobec 44.

### Co z tego wyciśnięto

Brakujący wariant BN=256 dla `down` (jedyny kształt, gdzie ten kafel daje 41%,
a nie ~4%) — dodany, efekt end-to-end **+6,3% przy 2048 i +7,8% przy 4096**.

## Granica: zajętość 16,67%

`ncu` na kernelu prefillu:

| metryka | q/o (16,8) | gate/up (44,8) |
|---|---|---|
| przepustowość SM | 43,5% | 21,7% |
| aktywne warpy | 16,3% | 16,0% |
| instr./cykl | 0,11 | 0,05 |
| fale na SM | 2,67 | 7,33 |

Ograniczenia zajętości — **oba** dopuszczają jeden blok na SM:

```
rejestry/wątek:            224   -> limit 1 blok
pamięć współdzielona/blok: 98,3 KB -> limit 1 blok
maks. aktywnych warpów:    16,67%
```

224 rejestry biorą się głównie z akumulatorów: kafel warpa 64×64 to 4096 wartości
F32 na warp, czyli 128 rejestrów na wątek zanim policzymy cokolwiek innego.
98,3 KB to 4 etapy × (128+256) × 64 B.

## Zmierzone ślepe uliczki

Każda sprawdzona pomiarem, nie odrzucona z rozumowania:

- **Wektorowy epilog** zamiast pętli po elementach: 2 025,2 vs 2 025,7 µs, czyli
  zero. Kompilator Mojo i tak rozwijał `comptime for`. Hipoteza brała się stąd,
  że `gate/up` zapisuje 11,5 mln wyjść wobec 4,2 mln w `down` — okazała się
  nietrafiona.
- **BM=256**: przekracza pamięć współdzieloną (256+256)×64×4 = 128 KB.
- **Kafel warpa inny niż 64×64** (64×32, 32×64): kernel Modulara odrzuca
  konfigurację w czasie wykonania.
- **Własne kernele `gemm_fp8_wmma_*`**: budowane wyłącznie dla AMD, nie ma ich
  w zestawie sm_121a.

## Wniosek

W obrębie `multistage_gemm_kernel` Modulara pokrętła są wyczerpane. Zajętość
16,67% wynika z rozmiaru rejestrów i pamięci współdzielonej tego kernela, a nie
z naszego doboru kafla — a dwa niezależne ograniczenia oznaczają, że zbicie
jednego niczego nie da.

Dalsze możliwości, od najtańszej:

1. **Kolejność bloków w siatce.** Kernel dostaje siatkę `(N/BN, T/BM)`, więc
   kolejne bloki różnią się kaflem B (1 MiB każdy). Przy 44 kaflach kolumnowych
   `gate/up` trzyma w locie 44 MiB wag, przy 16 w `down` — 16 MiB. To najlepiej
   tłumaczy różnicę 2,3× przy identycznym ruchu, ale zmiana mapowania wymaga
   wejścia w kernel, bo indeksy liczone są z `blockIdx`.
2. **Własny GEMM FP8 pod te kształty** dla sm_121a — pełna kontrola nad kaflem,
   etapami i kolejnością bloków.
3. Nowszy kernel z biblioteki Modulara, jeśli mają wariant dla Blackwella;
   pakiet jest skompilowany, więc bez źródeł nie da się tego sprawdzić z repo.
