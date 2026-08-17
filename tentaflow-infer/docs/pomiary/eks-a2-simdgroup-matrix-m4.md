# EKS-A2 — przepustowość `simdgroup_matrix` na Apple M4

**Sprzęt:** Apple M4 (bazowy), 10 rdzeni GPU, 16 GiB unified, macOS 26.5.2, Metal 4.
**Kod:** `tools/eks-apple/eks_a2.swift`, uruchamiany `tools/eks-apple/run.sh a2`.
**Data:** 2026-08-02. **Stan termiczny:** `nominal` przed i po.

**Protokół** jak w EKS-A1/A3: rozgrzewka na tym samym kształcie, proces ciepły, 5 przebiegów
z odrzuceniem pierwszego, mediana + IQR, znacznik `ważny` przy `IQR/mediana ≤ 3%`.

---

## 1. Werdykt

| | Wynik |
|---|---|
| `simdgroup_matrix` f16 → f16 | **3,94 TFLOPS** |
| `simdgroup_matrix` f16 → f32 | **3,89 TFLOPS** (akumulacja w f32 kosztuje 1,3%) |
| `simdgroup_matrix` bf16 → f32 | **3,90 TFLOPS** (bf16 za darmo) |
| Zwykłe FMA na FP32 (`float4`) | **3,07 TFLOPS** |
| **Przewaga instrukcji macierzowej nad ALU** | **1,28×** |
| **Sufit obliczeniowy prefillu** (7,5 mld param., 2·P na token) | **~260 tok/s** |

**Wniosek naczelny: na M4 `simdgroup_matrix` nie jest rdzeniem tensorowym.** Daje 1,28×
nad zwykłym FMA, a nie rząd wielkości. To potwierdza pomiarem na tej maszynie to, co
zewnętrzna analiza Metal 4.1 stwierdziła dla M4 Max: instrukcja macierzowa wykonuje się
na tych samych rdzeniach shaderów, bez dedykowanej ścieżki. Dedykowane akceleratory
neuronowe pojawiają się dopiero w M5.

**Skutek dla planu:** prefill na Apple jest ograniczony ALU, a nie formatem wag.
Kwantyzacja pomaga tam **wyłącznie po stronie pamięci** — nie skraca liczenia. Wcześniejsze
oszacowanie ~250 tok/s (skalowanie 14,8 TFLOP/s z M4 Max po liczbie rdzeni) okazało się
trafne z dokładnością **6%**, więc cel prefillu **≥ 175 tok/s = 67% sufitu** zostaje bez zmian,
ale przestaje być hipotezą.

---

## 2. Pułapka pomiarowa, która unieważniła pierwszy przebieg

Pierwsza wersja porównania FMA raportowała **409 TFLOPS** na dziesięciordzeniowym GPU.
Sygnatura błędu była jednoznaczna: **czas nie zmieniał się przy czterokrotnym i
szesnastokrotnym zwiększeniu siatki** (4,26 ms przy 160 grupach, 4,25 ms przy 640, 4,28 ms
przy 2560), choć rósł liniowo z liczbą iteracji.

Przyczyna: cała arytmetyka pętli była **identyczna dla każdego wątku**. Operandy były
stałymi, więc obliczenie jest jednorodne w obrębie fali, a kompilator przenosi je na
ścieżkę skalarną — wykonuje raz zamiast raz na lane. Naprawa to jedna linia:

```metal
float4 b = float4(0.999999f + float(gid) * 1e-12f);   // operand zależny od wątku
```

Po niej czas skaluje się liniowo z siatką (105 → 390 → 1557 → 6222 ms dla 1×/4×/16×/64×)
i wynik ustala się na **3,07 TFLOPS**, czyli **133× niżej**.

**To jest odpowiednik pułapki „kafel 192×128 wychodził najszybszy, bo wcale nie wnosił wag"
i pułapki `v_dot4_i32_i8` mierzonego wyłącznie na danych dodatnich.** Reguła do zapisania
w zestawie regresyjnym:

> **Mikrobenchmark arytmetyki na Apple musi mieć operand zależny od identyfikatora wątku
> i kontrolę skalowania z rozmiarem siatki.** Bez pierwszego kompilator wykonuje pracę raz
> na falę zamiast raz na lane; bez drugiego nikt tego nie zauważy, bo liczba wygląda
> po prostu imponująco.

**Kontrola pomiaru macierzowego.** Ta sama wątpliwość dotyczyła głównego pomiaru, więc
został powtórzony z operandami **ładowanymi z pamięci i różnymi dla każdej fali**
(`simdgroup_load` z bufora indeksowanego numerem grupy i fali). Wynik: 3,75 / 3,83 / 3,96
TFLOPS dla 40 / 160 / 640 grup, czas rosnący liniowo z siatką i z liczbą iteracji — czyli
**identycznie jak w wariancie ze stałymi**. Pomiar macierzowy pułapce nie podlegał: operacja
na kaflu 8×8 jest z natury per-fala, nie jednorodna skalarnie.

---

## 3. Wyniki

### 3.1. Instrukcja macierzowa

Jedna `simdgroup_multiply_accumulate` na kaflu 8×8×8 to 1024 operacje zmiennoprzecinkowe.
Pętla nie dotyka pamięci — mierzy sufit arytmetyczny.

| wariant | akumulatory | grup | mediana [ms] | IQR | TFLOPS |
|---|--:|--:|--:|--:|--:|
| f16 → f16 | 1 | 640 | 271,72 | 0,3% | 3,86 |
| f16 → f16 | 2 | 640 | 536,27 | 1,4% | 3,91 |
| f16 → f16 | 4 | 640 | 1065,31 | 1,1% | **3,94** |
| f16 → f16 | 8 | 640 | 2136,51 | 0,2% | 3,93 |
| f16 → f16 | 16 | 640 | 4258,91 | 0,7% | 3,94 |
| f16 → f32 | 8 | 640 | 2168,10 | 0,3% | 3,87 |
| bf16 → f32 | 8 | 640 | 2152,45 | 1,0% | 3,90 |

Wpływ rozmiaru siatki (f16 → f16, 4 akumulatory): 40 grup → 3,45 TFLOPS,
160 → 3,86, 640 → **3,94**. Wysycenie następuje dopiero powyżej ~160 grup roboczych.

### 3.2. Zwykłe FMA na FP32, po naprawie z §2

| akumulatory | grup | mediana [ms] | IQR | TFLOPS |
|---|--:|--:|--:|--:|
| 2 | 640 | 170,94 | 0,7% | **3,07** |
| 4 | 640 | 392,06 | 1,4% | 2,67 |
| 8 | 640 | 791,80 | 1,4% | 2,65 |
| 16 | 640 | 1642,14 | 0,1% | 2,55 |

---

## 4. Cztery wnioski projektowe

1. **Akumulacja w f32 jest praktycznie darmowa** (3,89 wobec 3,94 TFLOPS = 1,3%).
   Kernele GEMM na Metal akumulują w f32 **domyślnie**; nie ma powodu handlować dokładnością
   za wydajność, a tolerancje testów golden robią się przez to łagodniejsze do spełnienia.
2. **bf16 kosztuje tyle co f16** (3,90 wobec 3,89 przy tej samej akumulacji). To istotne
   dla ścieżki MLX: skale i przesunięcia w checkpointach MLX są w BF16, więc nie trzeba
   ich konwertować przy ładowaniu.
3. **Więcej akumulatorów nie pomaga, a przy FMA szkodzi** (3,07 → 2,55 przy 2 → 16).
   To trzecie niezależne potwierdzenie, że reguła „≥ 8 niezależnych akumulatorów jest
   warunkiem pomiaru rooflinu", wyprowadzona na RDNA, **na Apple nie obowiązuje**.
   Dźwignią jest liczba grup roboczych, nie ILP w wątku.
4. **Prefill na Apple jest ograniczony ALU, nie formatem wag.** Przy 1,28× przewagi
   instrukcji macierzowej i braku sprzętowego fp8 (emulowany, 0,94× fp16) oraz fp4 nie ma
   czego szukać w niższej precyzji obliczeń. Kwantyzacja daje na Apple wyłącznie
   oszczędność pamięci — i to jest cała jej rola w tym silniku na tej platformie.

---

## 5. Co zostaje otwarte

- **Odstęp między sufitem instrukcji a realnym GEMM.** Tu zmierzono wyłącznie tempo
  wydawania instrukcji przy operandach rezydentnych. Realny kernel dokłada ruch przez
  pamięć, staging w pamięci grupy roboczej i dekwantyzację. Ile z 3,94 TFLOPS zostaje —
  rozstrzyga dopiero pierwszy kernel GEMM w fazie NA2 i to on ustali, czy cel
  ≥ 175 tok/s (67% sufitu) jest ostry, czy luźny.
- **Kształt kafla.** Zmierzono kafel 8×8×8 w izolacji; dobór `M×N×K` na poziomie kernela
  należy do autotunera z §4.1 planu.
- **M5.** Wnioski z §4 dotyczą M1–M4. Na M5 akceleratory neuronowe w rdzeniach GPU zmieniają
  punkt 4 i wymagają powtórzenia całego tego pomiaru jako osobnego wpisu w rejestrze
  możliwości (`apple_g10`).
