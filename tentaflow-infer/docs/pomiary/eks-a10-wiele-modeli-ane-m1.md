# EKS-A10 — 80 modeli ANE naraz: czy CoreML liczy na CPU? (Apple M1, 16 GB)

Pytanie: dlaczego w silniku predict na modelach produkcyjnych
`.runtime/ane/bielik-minitron-7b-s060-int8/` (80 mlmodelc: L00..L39 `_gate_up` i `_down`,
multifunction T256/T512/T1024, int8 per-channel) trwa 116–204 ms zamiast ~22 ms, które
daje jeden model gate_up T1024 w izolacji; a przejście kolejno po 80 modelach dało 56 ms
na predict. Hipoteza do sprawdzenia: przy wielu załadowanych modelach CoreML po cichu
liczy część na CPU (BNNS ~1,5 TFLOPS), a ostrzeżenie „ANE model load has failed for
on-device compiled macho. Must re-compile the E5 bundle” jest tego śladem.

Maszyna: Apple M1 (4P+4E), **16 GB**, macOS 26.6.2 (25G83), Swift 6.3.3. Maszyna
**pod presją pamięci przez cały pomiar**: przed startem wired ~2,4 GB, kompresor 0,3–1,6 GB,
**swap użyty 3,2–4,8 GB**, wolne 0,2–6 GB zależnie od momentu (Chrome, Claude itd. w tle,
load average 1,3–3,7). To nie jest wada pomiaru, tylko jego sedno — patrz wnioski.

Harness: `tools/eks-apple/eks_a10_multi.swift` (`./run.sh a10multi <modele> sweep|run|sweep-inproc …`).
Protokół: N modeli (na przemian `L{k}_gate_up`, `L{k}_down`, k = 0…), funkcja T1024,
`cpuAndNeuralEngine`, każde N w **nowym procesie**; rozgrzewka 3 predict na model; 5 rund
round-robin po wszystkich N; per predict: czas ścienny, czas CPU procesu (`getrusage`
user+system, osobno system), błędy stron (`ru_minflt`/`ru_majflt`), po oknie pomiaru czas
CPU per wątek (`thread_info`), `vm_stat` (pageins/swapins/dekompresje), RSS `aned`,
footprint procesu. fd 2 przekierowane do pliku i przeszukane; E5RT pisze komunikaty
**także na stdout** (fd 1) — stdout potomka przeszukany również. FLOP: gate_up
2·T·4096·13440, down 2·T·11264·2432. Wejścia f16 pseudolosowe na buforach wyrównanych do strony.

## Krótko

| pytanie | odpowiedź |
|---|---|
| **(a) czy przy wielu modelach liczy CPU?** | **NIE.** Czas *user* na predict to 0,2–0,5 ms przy każdym N (BNNS dla gate_up T1024 = 113 GFLOP musiałby dać ≥75 ms user). Cały nadmiar czasu CPU to czas **system** na wątku głównym (błędy stron: przy balaście 9–11 tys. drobnych błędów stron na predict, 6,1 mln dekompresji w oknie pomiaru). MLComputePlan po załadowaniu 80 modeli: **80/80 `linear` na ANE, 0 na CPU/GPU**. Ostrzeżenie „E5 bundle / ANE model load has failed” **nie wystąpiło ani razu** w 20 przebiegach (stdout i stderr, do 80 modeli, także `MLModel.compileModel`, `all`, T256, kopie plików). |
| **(b) od jakiego N / rozmiaru?** | Nie od liczby modeli, tylko od **pamięci wired**: 80 × ten sam plik (80 instancji `MLModel`) = **14,9 ms, 7,6 TFLOPS, IQR 2,6%** — jak jeden model. 80 warstw T1024 = wagi 3,1 GB + bufory we/wy ANE ~2,4 GB = **+6,0 GB wired** (2,4 → 8,5 GB na maszynie 16 GB) → strona swapowana przy każdym predict. Próg na tej maszynie: N=40 (+3,0 GB wired) bez straty; N=60 (+4,5 GB) +12%, IQR 20%; N=80 (+6,0 GB) +20–25% przy 5 GB wolnego, **+70%** (25,6 ms, 12 ms CPU) przy 2,7 GB wolnego, **+140%** (36–40 ms, 31 ms system) z 4 GiB balastu w procesie (symulacja wag silnika); 80 osobnych KOPII (4,2 GB wag, wired 10,5 GB): 27,6 ms, max 229 ms. |
| **(c) czy da się obejść?** | Kompilacja on-device (`compileModel` z mlpackage): **nic** (17,6 ms, jak mlmodelc). `all` zamiast `cpuAndNeuralEngine`: nic (18,0). `allowLowPrecisionAccumulationOnGPU`+`modelDisplayName`: nic (18,3). Wspólne `outputBackings`: nic (20,3; bufory ANE i tak per model). Działa tylko **zmniejszenie wired**: T256 zamiast T1024 (bufory we/wy 0,6 GB zamiast 2,4; 4,4 ms/predict = 6,4 TFLOPS, ale 4 predict na 1024 tokeny), **mniej modeli naraz** (≤40 przy tej maszynie; ładowanie z ciepłego cache 12–13 ms/model, zwalnianie oddaje wired w całości), a przede wszystkim **budżet pamięci silnika**: wagi FFN wycinka są w silniku podwójnie (int8 w E5 + 4-bit w checkpoincie MLX 4,2 GB). 116–204 ms w silniku to ta sama mechanika (swap) w gorszych warunkach niż tu. |

**Wniosek:** to nie fallback na CPU i nie limit liczby modeli; to **thrashing pamięci** —
wagi + bufory ANE są wired (nie da się ich wyrzucić), więc system wypycha do kompresora
i swapu wszystko inne, w tym strony, których dotyka predict (wejście, wyjście, stan
procesu), a czas „CPU” to jądro obsługujące błędy stron na wątku wywołującym.

## 1. Przemiatanie N (T1024, `cpuAndNeuralEngine`, warstwy, każde N w nowym procesie)

Mediany per predict; TFLOPS z mediany wall. „CPU” = czas CPU procesu na predict (user+system).
Przed N=80 wolne było 2,7 GB (po poprzednich N kompresor 1,6 GB).

| N | gate_up wall [ms] | gate_up CPU [ms] | gate_up TFLOPS | down wall [ms] | down CPU [ms] | down TFLOPS | runda N predict [ms] | CPU/wall (okno) | E5 bundle | aned RSS [MiB] |
|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| 1 | 14,26 | 0,86 | **7,90** | — | — | — | 14 | 0,06 | 0 | 6 |
| 2 | 16,06 | 1,56 | 7,02 | 19,19 | 2,10 | 2,92 | 36 | 0,14 | 0 | 7 |
| 5 | 15,32 | 1,23 | 7,36 | 17,73 | 1,89 | 3,16 | 82 | 0,10 | 0 | 10 |
| 10 | 15,23 | 1,26 | 7,40 | 17,54 | 2,11 | 3,20 | 164 | 0,10 | 0 | 17 |
| 20 | 15,27 | 1,41 | 7,38 | 17,61 | 1,84 | 3,19 | 331 | 0,11 | 0 | 31 |
| 40 | 15,28 | 1,86 | 7,38 | 18,18 | 2,77 | 3,09 | 667 | 0,13 | 0 | 36 |
| 80 | **25,56** (IQR 34%, max 54) | **12,05** | 4,41 | **25,70** | **10,10** | 2,18 | 2141 (min 1446, max 2488) | 0,36 | 0 | **8** |

Pamięć [MiB] (wired / footprint procesu / kompresor / wolne):

| N | przed | po załadowaniu | po predictach |
|--:|---|---|---|
| 40 | 2422 / 2 / 1665 / 3429 | 5463 / 10 / 1608 / 217 | 5464 / **1263** / 1580 / 56 |
| 80 | 2425 / 2 / 1579 / 2774 | 5257 / 13 / 1572 / 80 | **8495** / **2487** / 2075 / 59; aned RSS 36 → **8** (wypchnięty do swapu) |

Czas CPU w oknie pomiaru N=80: 3548 ms, z czego **main 3535 ms** — nie pula wątków
BNNS/CoreML, tylko wątek wywołujący `prediction`. Ten sam N=80 uruchomiony chwilę
później z ciepłym cache E5 i 3,7 GB wolnego: 18,6 / 18,1 ms, CPU 2,9 / 2,5 ms
(system 2,7 / 1,7) — ta sama konfiguracja, inny stan pamięci maszyny.

Powtórka progu (ciepły cache, 5,2 GB wolnego na starcie):

| N | gate_up [ms] | IQR | CPU [ms] | down [ms] | wired po predictach [MiB] | vm_stat w oknie predictów |
|--:|--:|--:|--:|--:|--:|---|
| 40 | 15,49 | 2,3% | 1,87 | 18,14 | 5488 | swapins +8, dekompresje +35 |
| 60 | 17,38 | **20,4%** | 2,75 | 18,27 | 6949 | swapins +140, dekompresje +212 |
| 80 | 19,16 (max 74) | 11,9% | 3,09 | 18,07 (max 42) | 8479 | swapins +332, **dekompresje +162 905** (2,5 GB) |

## 2. Kontrole N=80 (T1024 o ile nie zaznaczono; „sys” = część systemowa CPU na predict)

| wariant | gate_up wall [ms] | CPU (sys) [ms] | TFLOPS | down wall [ms] | CPU (sys) | wired po predictach [MiB] | footprint proc. [MiB] | E5 | uwagi |
|---|--:|--:|--:|--:|--:|--:|--:|--:|---|
| warstwy, ciepły cache (+MLComputePlan) | 18,56 | 2,92 (2,68) | 6,07 | 18,13 | 2,46 (1,73) | 8472 | 2485 | 0 | plan: **80/80 linear na ANE**, 0 CPU/GPU (30 s) |
| **80 × ten sam plik** `L00_gate_up` | **14,85** (IQR 2,6%) | 1,61 (1,33) | **7,59** | — | — | **2541** (+125) | 2782 | 0 | 80 instancji MLModel, jeden program E5 → wagi dzielone |
| **80 kopii** (klony APFS) `L00_gate_up` | **27,59** (IQR 32%, **max 229**) | **13,54 (13,02)** | 4,09 | — | — | **10 562** | 2787 | 0 | 4,2 GB wag; 512 błędów stron/predict; swap 3,5 → 4,8 GB; runda min 1795, max 4937 ms |
| funkcja **T256** | 4,38 (IQR 91%, max 18,5) | 0,50 (0,40) | 6,43 | 4,74 | 0,46 | 6387 | 634 | 0 | bufory we/wy 4× mniejsze; 16 modeli >1,5× (do 9 ms) — nadal presja |
| `computeUnits = .all` | 17,98 | 2,62 (2,32) | 6,27 | 18,34 | 2,79 | 8467 | 2545 | 0 | inna konfiguracja = ponowna kompilacja E5 (263 ms/model) |
| `allowLowPrecisionAccumulationOnGPU` + `modelDisplayName` | 18,27 | 2,97 (2,68) | 6,17 | 18,21 | 2,73 | 8466 | 2485 | 0 | bez zmian |
| `MLModel.compileModel` z .mlpackage (on-device) | 17,64 | 2,75 (2,48) | 6,39 | 18,53 | 2,81 | 8464 | 2488 | 0 | kompilacja 25 ms + ładowanie 303 ms/model; bez zmian |
| wspólne `outputBackings` (1 bufor na rodzaj) | 20,26 | 5,84 (4,07) | 5,56 | 18,80 | 3,35 | 8476 | 2516 | 0 | bufory per model zostają (kopia do naszego bufora kosztuje user) |
| jeden proces: 40 → zwolnij → 80 | 18,26 | 2,79 (2,51) | 6,17 | 18,22 | 2,52 | 8479 | 2515 | 0 | po `removeAll`: wired 8479 → **2428**, footprint 70 — zwalnianie działa |
| **balast 4 GiB** w procesie, N=1 | 17,53 | main 0,9 | 6,43 | — | — | 2552 | 4168 | 0 | balast sam nie szkodzi |
| **balast 4 GiB**, N=40 | 21,64 (max 48) | 12,9 (3,4 sys, reszta wątek balastu) | 5,21 | 26,66 | 16,7 (4,3) | 5468 | 5359 | 0 | swapins +1468, dekompresje +68 193 |
| **balast 4 GiB**, N=80 | **36,63** | **34,3 (31,5)** | 3,08 | **40,37** | **35,4 (31,7)** | 8501 | 6583 | 0 | **8948 / 11 069 błędów stron na predict**, dekompresje +6,14 mln (94 GB), swapouts +18 676; runda 3080 ms |

Bilans wired przy N=80 T1024: wagi 3,1 GB (52,6 MiB gate_up + 26,1 down na warstwę) +
bufory we/wy programu ANE per model (gate_up: x 8 MiB + y 26,3 MiB; down: x 22 MiB + y 4,8 MiB;
średnio ~31 MiB × 80 = 2,4 GB, widoczne 1:1 jako przyrost footprintu procesu 2485 MiB)
= 5,5 GB ≈ zmierzone **+6,0 GB**. T256: 3,1 + 0,6 = 3,7 ≈ zmierzone +3,9 GB.

## 3. Ostrzeżenie E5 i cache programów ANE

- Komunikaty E5RT idą na **stdout** (fd 1) — np. „E5RT encountered an STL exception …”
  wylądował w przechwyconym stdout, nie w pliku stderr. Harness przeszukuje oba strumienie.
- W żadnym z 20 przebiegów (1…80 modeli, T1024/T256, mlmodelc/compileModel, ane/all,
  kopie plików) tekst „E5 bundle” / „ANE model load has failed” **nie wystąpił**.
  `log show --last 3d` też nie zawiera „re-compile the E5” z żadnego procesu.
- Cache: `~/Library/Caches/<nazwa procesu>/com.apple.e5rt.e5bundlecache/25G83/<hash>/`.
  Dla `eks_a10_multi` po eksperymentach: **3,1 GB, 80 wpisów po 26/52 MiB** — skompilowany
  program E5 zawiera **pełną kopię wag** na dysku, osobno dla każdej nazwy procesu.
  Klucz zależy od treści, ale też od konfiguracji i ścieżki: T256, `all`, klony plików
  i `compileModel` do katalogu tymczasowego kompilowały od nowa (224–352 ms/model,
  CPU 6–8 s na 80 modeli); z ciepłym cache ładowanie = 12–13 ms/model (80 modeli w 1,0–1,1 s).
- **SPROSTOWANIE (EKS-A11, `eks-a11-ane-t256-cache-m1.md` §1):** poniższy trop okazał
  się błędnym odczytem. Program ANE z wagami (`model.hwx`, 26/52 MiB) leży w
  `/Library/Caches/com.apple.aned/25G83/ModelAssetsCache/<nazwa procesu>/` i silnik
  trafia w niego tak samo jak harness (ładowanie 12–19 ms/model); wpisy e5bundlecache
  bez wag to stan normalny, a 80 wpisów z `weights.bin` w cache harnessu pochodzi z
  jednego przebiegu `compileModel` z katalogu tymczasowego.
- **Trop dla silnika (nieaktualny):** katalogi cache binarek testowych silnika
  (`~/Library/Caches/cpu_share_prefill-0ba35e45b311095d/…`, 480 wpisów z dziś,
  `coreml_backend-71b167e02d91ec1e/…`) zawierają **tylko `model.milhash` (4–8 KiB na wpis),
  bez ciał programów** — czyli w silniku E5 bundle nigdy nie trafiają na dysk. To
  najprawdopodobniej źródło „on-device compiled macho … must re-compile the E5 bundle”
  (ładowanie z pustego wpisu nie udaje się i program jest kompilowany w pamięci przy każdym
  ładowaniu) i dodatkowe ~3 GB **anonimowej** pamięci procesu (zamiast mapowania pliku
  z cache), co pogłębia thrashing. Do potwierdzenia w silniku (tu nie uruchamiano cargo):
  `log stream --predicate 'process == "aned" OR eventMessage CONTAINS "E5"'` podczas
  ładowania, oraz sprawdzenie, czy nazwa procesu cargo (z hashem) / brak `CFBundleIdentifier`
  nie blokuje zapisu cache. Wymuszenie cache tylko do odczytu w harnessie daje inny,
  twardy błąd (`E5RT … create_directories: Permission denied`, ładowanie nie udaje się),
  więc mechanizm w silniku jest inny niż brak uprawnień.

## 4. Co z tego wynika dla silnika

1. **Nie szukać fallbacku na CPU** — nie ma go; MLComputePlan i czas user zgodnie mówią ANE.
   Miarą jest czas *system* i błędy stron na predict, nie sumaryczny czas CPU.
2. **Budżet wired na 16 GB.** 80 modeli T1024 = +6 GB wired; do tego wired bazowe ~2,4 GB
   i pamięć rezydentna silnika (checkpoint MLX 4,2 GB, bufory GPU). Powyżej ~9–10 GB
   wired+rezydentne maszyna swapuje i każdy predict kosztuje 20–200 ms błędów stron.
   Opcje: (i) nie trzymać w silniku 4-bitowej kopii wycinka FFN, który liczy ANE
   (dublowanie 3,1 GB int8 + ~1,6 GB 4-bit); (ii) T256 zamiast T1024 zmniejsza bufory
   ANE o 1,8 GB kosztem 4 predict na 1024 tokeny (6,4 TFLOPS mimo to); (iii) trzymać
   załadowanych ≤40 modeli i doładowywać z ciepłego cache (12–13 ms/model — ale to 60%
   kosztu predict, więc tylko jako awaryjne); (iv) mniejszy `share` wycinka ANE.
3. **Cache E5 w silniku** — sprawdzić, dlaczego ciała programów nie są zapisywane
   (punkt 3); bez cache każde ładowanie to 200–350 ms kompilacji i anonimowa pamięć.
4. Zwalnianie modeli działa (wired wraca do bazy, `aned` oddaje RSS), więc strategia
   „ładuj partiami” jest wykonalna; nie ma potrzeby nowego procesu.

## 5. Pliki

- `tools/eks-apple/eks_a10_multi.swift`, `tools/eks-apple/run.sh a10multi`.
- Wyniki surowe (stdout każdego przebiegu, stderr per N) w katalogu scratch sesji;
  mlpackage do `compileModel` wyeksportowano `tools/ane-export/ane_export.py --keep-mlpackage --no-plan`
  (80 modeli, 72 s).
