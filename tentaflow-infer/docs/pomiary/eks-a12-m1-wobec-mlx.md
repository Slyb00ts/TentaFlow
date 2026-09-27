# EKS-A12 — dekodowanie na Apple M1 wobec MLX i wobec pasma

Pytanie: użytkownik pamięta „około 20 tok/s w dekodowaniu i znacznie lepszy
prefill" na Bieliku-Minitron-7B 4-bit, a wczorajszy pomiar na M1 dał 11,0–11,3
tok/s. Czy silnik na M1 jest wolniejszy, niż powinien?

**Odpowiedź: nie. „20 tok/s" to liczba z M4 (EKS-A5/A7/A8), nie z tej maszyny.**
Na M1 sufit fizyczny dekodowania to ~11,9 tok/s i wczorajsze 11,0–11,3 było na
93–95% tego sufitu. Dziś, pod presją pamięci, silnik i MLX spadły razem — silnik
jest na 90% MLX przy tym samym protokole, a w prefillu jest od MLX szybszy.

## Stanowisko

Apple M1 (MacBookPro17,1; 4P+4E, **8 rdzeni GPU**), 16 GiB, pasmo katalogowe
68,25 GB/s, macOS 26.6.2. Data: 2026-09-12. `pmset -g therm`: bez ostrzeżeń.

**Stan pamięci podczas pomiarów** (`vm_stat`, strony 16 KiB): wolne 0,2–4,9 GB
(zmienne między przebiegami), nieaktywne 3,0–6,1 GB, wired 2,6–3,9 GB, swap
**1,98 GB w użyciu** z 3 GB, kompresor trzyma **531 k stron = 8,3 GB** w 1,8 GB.
Chrome ~2,2 GB RSS w kilku procesach, Claude ~1,1 GB. Podczas jednego przebiegu
testu dekodowania licznik kompresji urósł o 174 k stron (2,8 GB), dekompresji
o 177 k, pageouty +26, bez swapinów. Dla porównania EKS-A10/A11 (wczoraj, ta
sama maszyna) miały 5,8 GB wolnych + 2,1 GB speculative.

## 1. Pasmo pamięci na M1 (EKS-A1, `tools/eks-apple/run.sh`)

Ten sam kernel strumieniowy co na M4 (bufor 2 GiB, sweep akumulatory × grupy).
Dwa uruchomienia procesu:

| uruchomienie | najlepszy WAŻNY (IQR ≤ 3%) | konfiguracja | % z 68,25 GB/s |
|---|---:|---|---:|
| 1 | **48,6 GB/s** | 1 akumulator, 1024 grupy | 71% |
| 2 | **50,1 GB/s** | 1 akumulator, 256 grup | 73% |

Uwaga: na M4 wszystkie 10 wierszy sweepu były ważne (IQR ≤ 3%); tu ważne były
1–2 z 15, reszta miała IQR 5–27%. To skutek presji pamięci, nie kernela.
Reguła z M4 potwierdzona: jeden akumulator wygrywa, więcej łańcuchów obniża pasmo
(50,1 → 48,9 → 42,4 → 38,7 GB/s dla 1, 2, 4, 8).

EKS-A3 na M1: dyspozycja w jednym command bufferze 1,31 µs (M4: 0,61), osobny
command buffer 35 µs (M4: 19,6), powrót na hosta ~305 µs (M4: ~94; IQR 37%).

**Sufit dekodowania** = 50,1·10⁹ / 4,2068·10⁹ B = **11,9 tok/s** (48,6 → 11,6).

## 2. Wynik

Wszystko z jednego przedpołudnia, GPU zajęte tylko przez mierzony proces.
Silnik: `cargo test -p forge-model --release --features metal --test
generate_vs_mlx how_fast_decode_runs -- --ignored --nocapture --test-threads=1`
(prompt 256, 31 kroków, rozgrzewka + mediana z 3), pięć uruchomień procesu.
MLX: `tools/mlx-oracle/bench_mlx.py` (mlx 0.32.2, mlx-lm 0.31.3, ten sam
protokół), dwa uruchomienia, przeplatane z silnikiem; do tego
`mlx_lm.generate --max-tokens 64` (prompt polski 426 tokenów), trzy razy.

| | M1 dziś | M1 wczoraj (EKS-A10) | M4 (EKS-A5/A7/A8) |
|---|---:|---:|---:|
| pasmo zmierzone (EKS-A1) | 48,6–50,1 GB/s | — | 102,4 GB/s |
| sufit dekodowania | 11,6–11,9 tok/s | — | 24,34 tok/s |
| **silnik, dekodowanie** | **8,4 tok/s** (×5, 35,2–35,4 GB/s; 70–73% sufitu) | **11,0–11,3** (46,3–47,5 GB/s; 93–95% sufitu) | 21,2–21,9 (89,7–92,2 GB/s; 87–90%) |
| MLX, dekodowanie (bench_mlx.py) | **9,2 / 9,3 tok/s** (38,9 GB/s; 78%) | — | 21,3–22,4 (88–92%) |
| MLX, dekodowanie (`generate`, 64 tok.) | 10,2 / 10,2 / 10,2 tok/s | — | — |
| silnik / MLX (ten sam protokół) | **90%** | — | 98% |
| **silnik, prefill 256 (samo GPU)** | **89,4 tok/s** | ~100 (GPU), 130 (GPU+CPU) | 200,8–257 |
| MLX, prefill | 69,2 tok/s (256 tok.) / 61–65 (426 tok., `generate`) | — | 219 |
| silnik / MLX prefill | **129%** | — | 92% |
| EKS-A7 kontrola negatywna (M4) | — | — | 20,9 → 17,9 pod obciążeniem CPU |

Powtarzalność silnika jest wzorowa: 8,4 tok/s w pięciu procesach (3,684–3,706 s
na 31 tokenów, rozrzut 0,6%). Ścieżka token-po-tokenie w teście prefillu daje
to samo: 8,5 tok/s.

## 3. Skąd „20 tok/s"

1. **To liczba z M4.** EKS-A5: 21,9 tok/s (nasz) / 22,4 (MLX); EKS-A7: 20,9;
   EKS-A8: 21,2–21,3 / MLX 21,3–21,9 — wszystko przy 102,4 GB/s i 10 rdzeniach
   GPU. „Znacznie lepszy prefill" to 200–257 tok/s z tych samych dokumentów.
   Dekodowanie skaluje się z pasmem: 102,4/50,1 = 2,04×, a 21,9/11,0 = 1,99×.
   Liczby z M1 i M4 są więc ze sobą zgodne co do procenta.
2. **Nie jest to inny model z cache.** W `~/.cache/huggingface/hub` są jeszcze:
   `LibraxisAI/Bielik-1.5b-v3-mlx-mxfp4` (model.safetensors 849 MB — na M1
   dawałby ~50+ tok/s, nie 20) i `speakleash/Bielik-PL-Minitron-7B-v3.0-Instruct`
   (bf16, 3 × 5,0 GB = 15 GB — nie mieści się na 16 GB). Żaden z nich nie daje
   „około 20" na tej maszynie. Nie uruchamiano ich.
3. Na M1 20 tok/s wymagałoby 84 GB/s, czyli 123% pasma katalogowego. Fizycznie
   niemożliwe dla 4,2 GB wag.

## 4. Czy silnik na M1 ma problem

**Nie.** Trzy argumenty:

- Wczorajsze 11,0–11,3 tok/s to 93–95% zmierzonego sufitu pasma — wyżej niż
  na M4 (87–90%). Powyżej tego nie ma nic do zdobycia bez zmiany formatu wag
  lub spekulacji (EKS-A8).
- Dziś oba programy spadły o tyle samo (silnik 11,0 → 8,4 = −24%; MLX
  spodziewane ~11,5 → 9,3). Wspólny spadek dwóch niezależnych implementacji
  to sprzęt/system, nie kernel. Dowód: 2,8 GB kompresji i 2,8 GB dekompresji
  stron podczas jednego przebiegu, swap 2 GB, kompresor 8,3 GB, sam harness
  pasma z IQR 5–27% (na M4 ≤ 3%).
- W prefillu silnik (samo GPU, 89,4) jest o 29% szybszy od MLX (69,2), a wczoraj
  GPU+CPU dawało 130 i GPU+CPU+ANE 180–199 (EKS-A11).

Luka 10% w dekodowaniu wobec MLX przy tym samym protokole (na M4 było 2%) jest
poniżej progu, od którego opłaca się profilować, i mieści się w tym, co presja
pamięci robi z pomiarem (ten sam efekt widać w EKS-A5: zimny/rozgrzany dawało
13%). Wobec `mlx_lm.generate` (10,2) luka to 18%, ale to inny protokół: 64
tokeny zamiast 31, potok asynchroniczny `async_eval` i sampling — nie jest to
porównanie tej samej pracy.

## 5. Co zrobić, żeby zmierzyć to porządnie

Powtórzyć §2 po zwolnieniu pamięci (zamknięte Chrome/VM, swap 0, wolne ≥ 6 GB,
jak wczoraj w EKS-A10/A11). Spodziewane: silnik 11,0–11,3, MLX 11–12, harness
pasma ważny we wszystkich wierszach. Jeżeli wtedy luka do MLX przekroczy 15%,
dopiero wtedy profil kernela wektorowego (xctrace, jak EKS-A7 druga runda). Cel
„20 tok/s" na M1 nie istnieje — zapisać w planie jako liczbę M4.

Surowe wyjścia: `scratchpad/eks_a1_m1.md`, `eks_a1_m1_run2.md`, `mlx_run1.txt`,
`interleave.log` (sesja pomiarowa, poza repo).
