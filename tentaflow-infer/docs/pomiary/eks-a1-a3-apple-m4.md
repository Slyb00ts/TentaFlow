# EKS-A1 / EKS-A3 — sufit pamięci i koszt dyspozycji na Apple M4

**Sprzęt:** Apple M4 (bazowy), 10 rdzeni CPU (4P + 6E), **10 rdzeni GPU**, 16 GiB pamięci
unified, macOS 26.5.2, Metal 4. Zalecany budżet roboczy zgłaszany przez sterownik:
12 124 MiB. Pamięć współdzielona na poziomie grupy roboczej: 32 768 B.

**Kod:** `tools/eks-apple/eks_apple.swift`, uruchamiany `tools/eks-apple/run.sh`.
**Data:** 2026-08-02. **Stan termiczny:** `nominal` przed i po każdym przebiegu.

**Protokół** (zgodny z `PLAN_NAPRAWY.md` §9 N0 i §7.6): rozgrzewka 300 iteracji na tym
samym kształcie co pomiar, proces ciepły, 5 przebiegów z odrzuceniem pierwszego, mediana
i IQR, znacznik `ważny` przy `IQR/mediana ≤ 3%`. Cztery niezależne uruchomienia procesu.

---

## 1. Werdykt

| | Wynik | Skutek dla planu |
|---|---|---|
| **EKS-A1** przepustowość pamięci | **102,4 GB/s = 85,3%** ze 120 GB/s katalogowych | cele dekodowania na Apple **przeliczone w dół**, patrz §4 |
| **EKS-A1** sweep ILP | **reguła z AMD nie przenosi się** — najlepszy wynik przy JEDNYM akumulatorze | zakaz przenoszenia „≥ 8 akumulatorów" na Metal |
| **EKS-A3** dyspozycja w command bufferze | **0,61 µs** | **fuzja nie jest na Apple dźwignią** |
| **EKS-A3** osobny command buffer na dyspozycję | **19,6 µs** | command buffer per warstwa jest zakazany |
| **EKS-A3** powrót na hosta na dyspozycję | **~94 µs** | oczekiwanie hosta per warstwa jest zakazane |

---

## 2. EKS-A1 — przepustowość pamięci

Kernel strumieniowy czyta bufor **2 GiB** (daleko poza jakąkolwiek pamięcią podręczną tej
części) wypełniony realnymi wartościami, nie zerami. Sweep po liczbie niezależnych
łańcuchów akumulacji i po liczbie grup roboczych.

| akumulatory | grup | mediana [ms] | IQR/mediana | ważny | GB/s | % z 120 |
|---|--:|--:|--:|---|--:|--:|
| **1** | **256** | 20,795 | 1,4% | tak | **103,3** | **86,1%** |
| 1 | 1024 | 21,149 | 1,8% | tak | 101,5 | 84,6% |
| 1 | 4096 | 20,810 | 1,3% | tak | 103,2 | 86,0% |
| 2 | 256 | 21,651 | 1,1% | tak | 99,2 | 82,7% |
| 2 | 4096 | 22,009 | 0,2% | tak | 97,6 | 81,3% |
| 4 | 256 | 23,627 | 1,5% | tak | 90,9 | 75,7% |
| 4 | 4096 | 22,851 | 3,0% | tak | 94,0 | 78,3% |
| 8 | 256 | 26,480 | 1,4% | tak | 81,1 | 67,6% |
| 8 | 1024 | 38,522 | 2,4% | tak | 55,7 | 46,5% |
| 16 | 4096 | 21,854 | 2,3% | tak | 98,3 | 81,9% |

Powtarzalność najlepszej konfiguracji w czterech uruchomieniach procesu:
**102,4 / 103,3 / 102,0 / 102,4 GB/s**. Do planowania przyjmujemy **102,4 GB/s**.

### 2.1. Reguła ILP z AMD nie przenosi się na Apple

Na RDNA zmierzono, że przy czterech łańcuchach akumulacji pomiar jest ograniczony
opóźnieniem i pokazuje **dokładnie połowę** sufitu, a krzywa wypłaszcza się na ośmiu —
stąd wymaganie „≥ 8 niezależnych akumulatorów jest warunkiem pomiaru rooflinu".

Tutaj zależność jest **odwrotna i monotonicznie malejąca** do ośmiu akumulatorów:
103,3 → 99,2 → 90,9 → 81,1 GB/s. Jeden akumulator wygrywa na każdym rozmiarze siatki.
Wynik jest stabilny (IQR ≤ 1,8%) i powtarza się w czterech uruchomieniach, więc nie jest
artefaktem pomiaru.

**Konsekwencja projektowa:** kernele strumieniowe na Metal projektujemy na prostą pętlę
o jednym łańcuchu i szerokim odczycie (`float4`), a nie na ręcznie rozwijane akumulatory.
Wymaganie ILP zostaje w mocy **wyłącznie dla AMD i NVIDII** i musi być zapisane jako
własność targetu w rejestrze możliwości, nie jako reguła globalna.

**Czego ten pomiar NIE rozstrzyga:** dlaczego tak jest. Hipoteza robocza — więcej
akumulatorów to więcej rejestrów na wątek i niższa zajętość, a jeden strumień zachowuje
lepszą lokalność wierszy DRAM. Rozstrzygnięcie wymagałoby licznika zajętości, którego ten
harness nie czyta. Do celów planowania wystarczy fakt, że sufit osiąga się przy acc=1.

---

## 3. EKS-A3 — koszt dyspozycji

Cztery pomiary, każdy po rozgrzewce 300 iteracji.

| pomiar | µs na dyspozycję | IQR | ważny | powtórzenia (4 procesy) |
|---|--:|--:|---|---|
| dyspozycja w **jednym** command bufferze, pusty kernel | **0,61** | 0,6–3,0% | tak | 0,621 / 0,717 / 0,603 / 0,607 |
| jak wyżej, z zależnością danych (RMW) | **0,87** | 3,4% | granicznie | +40–44% wobec pustego |
| **osobny command buffer** na dyspozycję, bez czekania | **19,6** | 0,1–2,0% | tak | 19,57 / 20,53 / 19,56 / 19,55 |
| `commit` + `waitUntilCompleted` na **każdą** dyspozycję | **~94** | 1,9–20,5% | częściowo | 94,1 / 131,1 / 93,1 / 94,4 |

Ostatni wiersz ma w dwóch przebiegach IQR powyżej progu 3% i zgodnie z protokołem nie
zalicza się jako pomiar precyzyjny — jest wiarygodny wyłącznie co do rzędu wielkości,
a ten rząd wielkości wystarcza do wniosku.

### 3.1. Przełożenie na krok dekodowania

| ścieżka | dyspozycji na token | narzut na token |
|---|--:|--:|
| 681 dyspozycji w jednym command bufferze | 681 | **0,41 ms** |
| 200 dyspozycji w jednym command bufferze | 200 | 0,12 ms |
| 65 dyspozycji w jednym command bufferze | 65 | **0,04 ms** |
| 65 dyspozycji, **osobny bufor każda** | 65 | **1,27 ms** |
| 65 **powrotów na hosta** | 65 | **6,12 ms** |

Przy oczekiwanym kroku dekodowania ~50 ms (§4) daje to odpowiednio 0,8% / 0,08% / 2,5% / 12,2%.

### 3.2. Trzy wnioski, które zmieniają projekt ścieżki Apple

1. **Fuzja kerneli nie jest na Apple dźwignią wydajności.** Zejście z 681 do 65 dyspozycji
   oszczędza **0,37 ms z ~50 ms kroku, czyli 0,7%**. Na AMD ta sama zmiana warta jest
   2,26 ms z 32 ms, czyli 7% — i tam pozostaje priorytetem. **Wniosku z AMD nie wolno było
   przenieść i to jest dokładnie ten pomiar, który miał o tym rozstrzygnąć.**
   Fuzja zostaje uzasadniona na Apple innymi względami (mniej ruchu przez pamięć, mniej
   materializowanych buforów pośrednich), ale nie kosztem dyspozycji.
2. **Command buffer per warstwa jest zakazany.** 19,6 µs to 32× koszt dyspozycji;
   65 warstw w osobnych buforach kosztuje 1,27 ms na token bez żadnej pracy.
3. **Oczekiwanie hosta w pętli warstw jest zakazane bezwzględnie.** ~94 µs na powrót to
   154× koszt dyspozycji. Ścieżka dekodowania ma zamykać krok w jednym (lub kilku)
   command bufferach i synchronizować się z hostem **raz na krok**, nigdy na warstwę.

Odpowiednik liczbowy z AMD dla porównania: dyspozycja 3,83 µs, powrót na hosta 18,20 µs.
Apple ma dyspozycję **6,3× tańszą**, a powrót na hosta **5,2× droższy** — proporcje są
odwrócone, więc i optymalizacja jest inna.

---

## 4. Skutek dla celów w `PLAN_NAPRAWY.md` §7.7

Cele Apple były postawione na dwóch liczbach, z których **obie były błędne**:

| | było | jest | źródło błędu |
|---|--:|--:|---|
| rozmiar modelu Bielik-7B-MLX-4bit | 3,9 GB | **4,207 GB** | `du` podaje GiB, wpisano jako GB |
| przepustowość pamięci | 120 GB/s | **102,4 GB/s** | liczba katalogowa zamiast pomiaru |
| sufit dekodowania | 30,8 tok/s | **24,3 tok/s** | iloczyn obu powyższych |
| cel v1 dekodowania | ≥ 24 tok/s | **≥ 19 tok/s** | 24 tok/s to 98,6% sufitu — nieosiągalne |

Rozmiar dokładny: 4 206 804 396 B. Sufit = 102,4·10⁹ / 4,2068·10⁹ = **24,34 tok/s**.
Cel v1 = 80% sufitu = **19,5 tok/s**, zapisujemy **≥ 19 tok/s**; wartość rozciągnięta
(86% sufitu, tyle ile kernel strumieniowy wyciąga ze sprzętu) to 21 tok/s.

To jest ten sam błąd metodyczny, który ten projekt zarzucił poprzednikowi — liczba
katalogowa w mianowniku zamiast pomiaru — popełniony przy pisaniu planu i wychwycony
przez pierwszy eksperyment. Zostaje zapisany, żeby nie wrócił.

---

## 5. Co zostaje otwarte

- **EKS-A2 — przepustowość `simdgroup_matrix` w TFLOPS** na kształtach warstw modelu,
  f16 i bf16. Bez niego cel prefillu (≥ 175 tok/s) pozostaje **wyprowadzony ze skalowania
  14,8 TFLOP/s zmierzonych na M4 Max po liczbie rdzeni GPU**, a nie zmierzony.
- Przyczyna zachowania sweepu ILP (§2.1) — hipoteza zajętości niepotwierdzona.
- Pomiar na tej samej maszynie pod obciążeniem termicznym (`fair`/`serious`), żeby ustalić,
  o ile przebieg po throttlingu zaniża wynik. Protokół już zapisuje stan; brakuje serii.
