# Natywne MTP NVFP4 dla Qwen3.5/3.6

Raport stabilnego etapu natywnego MTP/NextN w FORGE dla gęstego hybrydowego
GGUF `qwen35`. Nazwa checkpointu używa Qwen3.6, natomiast identyfikator
architektury zapisany w GGUF to `qwen35`.

## Środowisko i model

- GPU: NVIDIA GeForce RTX 4090.
- Backend zweryfikowany wykonawczo: CUDA.
- Model: `protoLabsAI/ThinkingCap-Qwen3.6-27B-MTP-GGUF`, plik
  `ThinkingCap-Qwen3.6-27B-NVFP4-MTP.gguf`.
- Układ: 64 warstwy targetu, w tym 48 DeltaNet i 16 pełnej atencji, oraz jeden
  blok `nextn_predict_layers` używany przez MTP.
- Kwantyzacja: GGUF NVFP4 dla głównych projekcji oraz Q8_0/F32 dla tensorów,
  które checkpoint przechowuje w tych formatach.
- Tryb: pojedynczy strumień, greedy, bez cache prefiksu, `max_active=1`.

AMD/ROCm i Metal nie były uruchamiane. Kernele mają źródła Mojo i zachowują
podział odpowiedni do przyszłego codegenu AMDGPU/Metal, ale nie jest to dowód
zgodności ani wydajności na tych backendach.

## Zakres implementacji

Loader oddziela warstwę NextN od autoregresyjnego trunku, zachowuje natywny
układ NVFP4 i współdzieli embedding oraz głowę wyjściową targetu, gdy GGUF nie
zawiera ich dedykowanych odpowiedników. MTP generuje draft K=2 albo K=3 na GPU.
Target weryfikuje draft blokowo, wykonuje batched argmax i zatwierdza KV oraz
stan hybrydowego DeltaNet.

Aktualny wariant B2 wykonuje dwa greedy-exact requesty o tym samym K we wspólnym
cyklu. KV, atencja, DeltaNet, decyzje acceptance/correction i commit mają
segmentowane bufory per lane. DeltaNet przechowuje wspólny stan potrzebny przez
forward i odtwarza zaakceptowany stan podczas commit, zamiast utrzymywać komplet
checkpointów wszystkich kroków. Usunęło to około 1,125 GiB scratchu. CPU nadal
uruchamia cykl i odczytuje wynik sterujący, natomiast obliczenia modelu i sampling
greedy pozostają na GPU.

`--speculative mtp` ustawia maksymalny budżet K=3 i adaptacyjnie porównuje tempo
K=2 oraz K=3. Dostępne są też jawne `mtp:2` i `mtp:3`. Każda próba benchmarku
porównuje pełną sekwencję tokenów z sekwencyjnym greedy i przerywa się przy
różnicy.

## Aktualny wynik MTP B2 (2026-07-22)

Ta sekcja zastępuje starsze punkty kontrolne wydajności poniżej. Scheduler
grupuje requesty według wybranego budżetu i paruje wyłącznie lane'y z tym samym
K=2 albo K=3. Różne K, niepełna para, `mtp+ngram`, tiering, brak device-side
embeddingu lub niespełniony kontrakt kerneli przechodzą na seryjne MTP B1.
Sampling inny niż greedy-exact przechodzi poza natywną ścieżką MTP. Zmienna
`FORGE_NATIVE_MTP_B2` jest ścisłym kill-switchem: brak wartości lub `1` włącza
B2, `0` wymusza B1, a każda inna wartość zatrzymuje start z błędem.

Pomiary RTX 4090 obejmują dwa identyczne requesty, 128 tokenów wyjścia i pięć
powtórzeń. Każdy poprawny przebieg zachował pełną zgodność ID obu lane'ów z
seryjnym greedy. `ON` oznacza adaptacyjne K=2/K=3 po włączeniu warp32 attention;
`OFF` jest stabilną pięciopomiarową kontrolą B1 z tej samej serii.

| Prompt | B2 ON, mediana (zakres) | B2 OFF, mediana (zakres) | Zmiana mediany |
|---|---:|---:|---:|
| raw128 | **137,40** (98,14-137,51) tok/s | 101,97 (97,12-102,06) tok/s | **+34,75%** |
| raw512 | **97,78** (97,53-98,15) tok/s | 76,38 (76,35-76,48) tok/s | **+28,02%** |

W czterech szybkich próbach raw128 wykonano 68 verifier forwardów i zaakceptowano
188 tokenów; wolniejsza próba zmieniła reżim schedulera na 74/181, ale zachowała
te same ID. Raw512 był stabilny: 90 forwardów i 170 zaakceptowanych tokenów w
każdym przebiegu.

Kontrola stałego K=3, także pięć powtórzeń i pełna parity:

| Prompt | Mediana | Zakres | Verifier forwardy B2 |
|---|---:|---:|---:|
| raw128 | **136,97 tok/s** | 136,90-137,01 | 34 |
| raw512 | **94,34 tok/s** | 94,25-94,41 | 46 |

Artefakty pomiarów: `/tmp/mtp-b2-attn-adaptive-raw128.log`,
`/tmp/mtp-b2-attn-adaptive-raw512.log`, `/tmp/mtp-b2-five-raw128-off.log` i
`/tmp/mtp-b2-five-raw512-off.log`.

### Profil po warp32 attention

Na NVIDIA jedna warp32 obsługuje query/head segmentowanej atencji; przenośny
wariant CTA pozostaje dla pozostałych urządzeń. Mikrobenchmark B2 T4, Q24/KV4,
head_dim=256 skrócił attention z 112,319 do 52,799 us dla ctx128, z 441,683 do
203,944 us dla ctx512 i z 1609,602 do 743,132 us dla ctx2048. Maksymalna różnica
wyniku syntetycznego wyniosła zero.

Profil raw512 zawierał 90 cykli B2 i 206 178 uruchomień kerneli. Największe
pozycje GPU na cykl: projekcje NVFP4 B8 25,181 ms, draft head Q8 8,668 ms,
segmentowana atencja warp32 5,664 ms, Q8 B8 5,379 ms, DeltaNet forward 2,393 ms,
exact logits 2,176 ms i commit DeltaNet 1,296 ms. Po stronie runtime dominowało
249 `cuCtxSynchronize` (10,304 s), 206 178 `cuLaunchKernel` (2,131 s) oraz 1422
`cuMemcpyHtoD_v2` (2,007 s). Dekodowanie nadal wykonuje dwa
`cuCtxSynchronize` na cykl B2. Artefakty: `/tmp/mtp-b2-warp-attn-raw512.nsys-rep`
i `/tmp/mtp-b2-warp-attn-raw512.sqlite`.

### Jednorundowy pack/gather verifiera

Draft ID pozostają na GPU od proposera do verifiera. Kernel Mojo pakuje wejście
`[B,T]`, pozycje i widoczność w układzie sequence-major, a batchowy gather
embeddingu targetu obsługuje F16, Q8_0 i GGUF NVFP4. NVFP4 ma wariant warp32
dla NVIDIA oraz wariant przenośny dla pozostałych backendów. Host przekazuje
wyłącznie znane pozycje bazowe i tablice stron przez pamięć pinned, bez odczytu
draftu i bez hostowego `Vec`/H2D embeddingu. Jedyny odczyt D2H obejmuje końcowe
decyzje i ID potrzebne do commit/emisji, po czym wykonywany jest jeden sync.
Każdy format zeruje wynik dla ujemnego ID lub ID równego/większego od rozmiaru
słownika bez odczytu wagi; finalna walidacja decyzji zwraca wtedy kontrolowany
błąd zamiast dopuszczać GPU OOB.

Profil `/tmp/mtp-b2-one-roundtrip-raw512.nsys-rep` objął 24 pełne cykle B2.
Każdy cykl miał dokładnie jeden `cuCtxSynchronize` oraz cztery H2D: dwie pozycje
bazowe i dwie tablice stron. Memcheck kerneli pack/gather zakończył się bez
błędów i bez naruszenia canary dla K2/K3, F16/Q8_0/NVFP4 oraz błędnych ID `-1`
i `vocab`.

Współczesny pomiar A/B względem czystego `7d472a0a`, po pięć powtórzeń i z
pełną zgodnością ID, dał:

| Prompt | Jednorundowa mediana (zakres) | HEAD mediana (zakres) | Zmiana |
|---|---:|---:|---:|
| raw128 | **127,91** (126,99-128,62) tok/s | 127,20 (127,08-127,31) tok/s | **+0,56%** |
| raw512 | **93,56** (93,53-93,72) tok/s | 93,45 (93,43-93,53) tok/s | **+0,12%** |

Stałe K=3 dla raw128 osiągnęło 125,63 tok/s wobec 125,82 tok/s na HEAD
(-0,15%), czyli różnicę w granicach szumu. Zmiana usuwa round-trip i pierwszy
sync, ale w aktualnym profilu throughput nadal dominuje praca kerneli modelu.
Logi: `/tmp/mtp-b2-one-roundtrip-five-raw128.log`,
`/tmp/mtp-b2-one-roundtrip-five-raw512.log`,
`/tmp/mtp-b2-head-contemporary-five-raw128.log` i
`/tmp/mtp-b2-head-contemporary-five-raw512.log`.

### Współdzielona kwantyzacja Q8 wejścia DeltaNet

Checkpoint shared-Q8 przygotowuje `pb.x` raz i używa go dla trzech projekcji
Q8_0: `gate_proj`, `alpha_proj` i `beta_proj`. `out_proj` nie należy do grupy:
osobno kwantyzuje `normed`, które ma inną semantykę i może mieć inny wymiar.
Routing wymaga zgodnego typu Q8_0 i liczby kolumn wszystkich trzech wag; w innym
przypadku pozostaje dotychczasowa ścieżka.

Izolowany mikrobenchmark RTX 4090 używał wierszy `[5120, 48, 48]`, 5120 kolumn,
siedmiu naprzemiennych próbek po 200 iteracji i mediany czasów GPU. Nie jest to
pomiar E2E modelu.

| Wariant | T=6 | T=8 | Uruchomienia grupy |
|---|---:|---:|---:|
| Osobna kwantyzacja | 53,452 us | 58,537 us | 6 |
| Shared-Q8 | **47,691 us** | **54,451 us** | **4** |
| Zmiana | **+10,78%** | **+6,98%** | **-2** |

W pełnym B2 każda z 48 warstw DeltaNet wykonywała dotąd cztery kwantyzacje:
trzy dla grupy `gate`/`alpha`/`beta` i jedną dla `out`. Po zmianie oczekiwane są
dwie, czyli łącznie **192 -> 96** wywołań `quantize_act_q8_1` na cykl B2. Jest
to projekcja wymagająca potwierdzenia przez nsys.

Testy T6/T8 potwierdziły exact bits, canary i top1, trzy GEMM-y enqueue przed
jednym sync, osobny oracle `out_proj`, niezerowy offset wagi, dwa strumienie z
przejściem T6 -> T8 i realokacją scratchu oraz kontrolowane błędy eventu,
unieważnienie uchwytu i zatrucie scratchu.

**PENDING:** realny 27B full-ID K2/K3, pięciopomiarowe A/B raw128/raw512 oraz
liczniki i czas nsys. Obcy proces `tentaflow` zmniejszył wolną pamięć GPU poniżej
22,5 GiB, dlatego tego etapu nie uruchomiono. Powyższych liczb nie należy
przedstawiać jako wyniku E2E.

### Ograniczenia i odrzucone eksperymenty

Pozostały narzut CPU to przygotowanie pozycji bazowych i tablic stron oraz
obsługa końcowej decyzji. Strony przypięte przez cache prefiksu są konserwatywnie
liczone przez admission i mogą opóźnić przyjęcie requestu mimo fizycznego
współdzielenia. Pełny builder blokuje obecnie FP8 wymagające PTX ISA 8.4,
ponieważ Mojo emituje PTX 8.1; izolowany AOT badanych kerneli działa.

Próba skierowania B8 do istniejącego BM32 obniżyła raw512 z 82,13 do 38,66
tok/s. Dedykowane warianty M8 MMA m16, BN64/BN128 również były wolniejsze od B8,
więc eksperyment usunięto i nie jest częścią dispatchu.

### llama.cpp B2: wynik diagnostyczny, nie baseline

Harness llama.cpp raportował mediany pure MTP 197,37 tok/s dla raw128 i 131,15
tok/s dla raw512 oraz MTP+n-gram 228,99 i 138,97 tok/s. Wyniki nie przeszły
jednak bramki correctness: tylko **5 z 24** wyjść lane'ów zgadzało się z oracle
`np1`. Zgodność lane0 z lane1 nie zastępuje zgodności z `np1`, dlatego tych
liczb nie wolno traktować jako porównania wydajności. Dane diagnostyczne:
`/tmp/llama-mtp-b2-results.json`, `/tmp/llama-nospec-np1.json` i
`/tmp/llama-mtp-np1-oracle.json`.

## Wyniki retained

| Silnik i tryb | raw128 | raw512 |
|---|---:|---:|
| llama.cpp, pure MTP, ten sam lokalny GGUF | 111,079 tok/s | 87,652 tok/s |
| FORGE, pure MTP K=3 | około 102,0 tok/s | około 72,6 tok/s |
| FORGE, router `mtp+ngram:3` | 118,897 tok/s | 76,688 tok/s |

`raw128` i `raw512` oznaczają długość surowego promptu użytego w porównaniu.
Liczby FORGE są wynikami po włączeniu retained checkpointów, dokładnej głowy
Q8 B3/B4,
scalonego przygotowania DeltaNet, szybkich projekcji NVFP4 B3/B4 na NVIDIA oraz
grafów stałej części verifiera T=3/T=4. Każdy przebieg zakończył się identycznymi
ID tokenów względem sekwencyjnego greedy. Wyniki llama.cpp dotyczą pure MTP bez
n-gramu, dlatego wiersz routera FORGE jest dodatkowym wynikiem, a nie porównaniem
tej samej techniki spekulacji.

Zgodność dokładnej głowy Q8 T3/T4 jest wynikiem empirycznym: goldeny kernela,
audyt stanów i pełne przebiegi wykazały bitową zgodność dla badanych danych.
Nie stanowi to formalnego dowodu identycznej kolejności redukcji FP32 dla każdego
możliwego wejścia ani dla niemierzonych backendów.

Pure MTP FORGE osiąga około 91,8% wyniku llama.cpp dla raw128 i około 82,8% dla
raw512.
Retained commit i graf verifiera usuwają zbędne obliczenia oraz większość
narzutu uruchomień z verifiera, ale nie domykają luki. Proposer pozostaje eager:
próby przechwycenia jego pełnego kroku, samej części hidden oraz wariantu z
pierścieniem buforów zmieniały stan kolejnych kroków MTP i obniżały akceptację
z 74,2% do około 40-54%, więc nie zostały zachowane.

Profil Nsight Systems dla 12 przebiegów verify zarejestrował dwa grafy T=3/T=4
i 10 wywołań `cuGraphLaunch`; pierwsze wykonanie każdego T rozgrzewa ścieżkę
eager. Mediana wywołania grafu wyniosła 180,9 us. Dominujące projekcje NVFP4
B4 i B3 miały odpowiednio 54,8 us oraz 53,3 us średnio na wywołanie. Skan
DeltaNet używa wyspecjalizowanych wariantów T=3/T=4 dla `d_state=128`; decyzja,
wybór wiersza i commit są w tym samym grafie co obliczenia verifiera.

## vLLM 0.25.1

Nie ma liczby vLLM dla tego samego artefaktu. Próba uruchomienia vLLM 0.25.1 na
lokalnym jednoplikowym GGUF nie przeszła inicjalizacji modelu: loader potraktował
ścieżkę jako źródło wymagające konfiguracji Hugging Face i tokenizera, zamiast
przyjąć GGUF jako kompletny lokalny checkpoint. W efekcie zakończył pracę na
etapie rozwiązywania konfiguracji, przed załadowaniem wag i benchmarkiem.

Nie zastąpiono tego modelu checkpointem safetensors ani inną kwantyzacją, ponieważ
nie byłoby to porównanie tych samych wag i tego samego MTP. Brak wyniku vLLM
oznacza brak obsługi badanego lokalnego artefaktu w tym protokole, a nie wynik
wydajności równy zero.

## Ograniczenia

- Tylko greedy-exact: `temperature=0`, sampling GPU i brak repetition penalty.
- Większe `max_active` przechodzi startup preflight. Native MTP B2 paruje po dwa
  zgodne sloty, a pozostałe sekwencje wykonuje seryjnie.
- Budżet natywnego MTP wynosi wyłącznie K=2 lub K=3.
- CUDA jest jedynym backendem sprawdzonym wykonawczo dla tego etapu.
- EAGLE, DFlash, DSpark, draft-model, n-gram jako rozszerzenie natywnego MTP,
  tree-attention i akceptacja stochastyczna pozostają poza tym etapem.

## Następny próg wydajności

Priorytetem jest bitowo zgodna redukcja narzutu eager proposera oraz dalsza
optymalizacja dominujących projekcji NVFP4. Scalone przygotowanie
DeltaNet usuwa 479 uruchomień kerneli i 1968 kopii D2D na cykl T=4. Ostatni
krok proposera, który jedynie materializuje KV i hidden, pomija już głowę
logits.

## Continuous admission dla dwóch slotów

Benchmark serwera używał jednego mierzonego żądania na slot, 8 tokenów promptu,
128 tokenów wyjściowych i wcześniejszego warmupu każdego slotu. Completion-only
liczono od pierwszego wyemitowanego tokenu do ostatniego `Done`, a end-to-end od
submitu wszystkich mierzonych żądań.

| `max_active` | Completion-only | End-to-end | Średni TTFT | Średni ITL |
|---:|---:|---:|---:|---:|
| 1 | 85,46 tok/s | 75,41 tok/s | 199,62 ms | 11,79 ms |
| 2 | 85,02 tok/s | 75,05 tok/s | 418,03 ms | 23,43 ms |

Profil `nsys` z tego samego protokołu wykazał 2 capture i 46 `cuGraphLaunch`
dla jednego slotu oraz 4 capture i 96 `cuGraphLaunch` dla dwóch slotów. Cache
per slot działa, ale seryjnie przeplatany forward zachowuje praktycznie stały
aggregate throughput i podwaja ITL. Ten etap zapewnia correctness, izolację i
continuous admission; wzrost przepustowości wymaga batchowych kerneli
hybrydowych dla targetu DeltaNet i draftu MTP.

## Minimalny pion targetu B2

Niespekulacyjny target B2 zachowuje mixer attention/DeltaNet osobno dla każdego
slotu, ale scala FFN i głowę logits przez istniejące batch GEMM. B1 pozostaje na
dotychczasowej ścieżce. Test porównał pełne ID dwóch concurrent streams z
serialnym oracle. Benchmark miał 8 tokenów promptu, 128 tokenów wyjściowych,
warmup każdego slotu i jawne `FORGE_HYBRID_PREFILL_CHUNK=128`.

| `max_active` | Completion-only | End-to-end | Średni TTFT | Średni ITL |
|---:|---:|---:|---:|---:|
| 1 | 38,30 tok/s | 36,15 tok/s | 199,17 ms | 26,31 ms |
| 2 | 41,79 tok/s | 39,24 tok/s | 398,92 ms | 48,23 ms |

B2 zwiększa aggregate completion throughput o 9,1% i end-to-end o 8,5%.
Native MTP korzysta z osobnego pionu B2 opisanego wyżej: scheduler paruje tylko
sekwencje z tym samym K, a segmentowany verifier `[B,T]` zachowuje osobny stan
per slot.
Każda zmiana musi zachować porównanie token po tokenie z sekwencyjnym greedy.

## Parowanie B2 dla B3/B4

Scheduler niespekulacyjnego targetu dzieli aktywne sekwencje na pary B2. B3
wykonuje jedną parę i jeden seryjny ogon, a B4 dwie pary. Pomiar na RTX 4090 dla
modelu o SHA-256
`d627e7e4abeac0ddefe92278bc2c37103116ac03e271ce0d44cb7763ded63b3a` obejmował
trzy powtórzenia po warmupie, 8 tokenów promptu i 128 tokenów wyjściowych na
sekwencję. Wszystkie lane'y zachowały pełną zgodność ID z seryjnym oracle.

| Szerokość | Mediana serial round-robin | Mediana parowania B2 | Zmiana |
|---:|---:|---:|---:|
| B3 | 37,92 tok/s | 40,41 tok/s | +6,58% |
| B4 | 37,90 tok/s | 41,32 tok/s | +9,01% |

Test E2E obejmuje także różne parametry samplingu per lane oraz anulowanie
środkowej sekwencji i ponowne użycie zwolnionego slotu. Natywne MTP ma odrębne
parowanie same-K B2; nie korzysta z pionu targetu niespekulacyjnego opisanego w
tej sekcji.

## Szybka ścieżka NVFP4 B3/B4 na NVIDIA

Krótkie GEMM B3/B4 mają osobny kernel Mojo wybierany tylko dla NVIDIA z warpem
32. Dwa warpy CTA liczą dwa niezależne wiersze, dekoder E2M1 korzysta z
16-elementowej LUT w pamięci współdzielonej, a 16 aktywacji jest pobieranych
jednym szerokim odczytem. Dotychczasowy kernel pozostaje fallbackiem dla innych
backendów.

Izolowany pomiar RTX 4090, czasy jednego wywołania:

| Kształt `rows x cols` | B3 przed | B3 po | B4 przed | B4 po |
|---|---:|---:|---:|---:|
| 5120 x 5120 | 0,02830 ms | 0,02112 ms | 0,02953 ms | 0,02368 ms |
| 17408 x 5120 | 0,08676 ms | 0,05000 ms | 0,09038 ms | 0,05845 ms |
| 5120 x 17408 | 0,09369 ms | 0,05682 ms | 0,08767 ms | 0,06251 ms |

Golden `0x7f` i `output_scale` jest bitowo zgodny dla B3/B4. Test pełnego
ThinkingCap NVFP4 K=3 zachował identyczną sekwencję względem greedy; krótki
pomiar prompt128/32 osiągnął około 91,15 tok/s przy 85,2% akceptacji.

## Kafelkowany skan DeltaNet T3/T4

Dla `d_state <= 128` skan dzieli niezależne kolumny stanu na kafle szerokości
warpa. Na NVIDIA/Qwen zwiększa to siatkę z 32 CTA po 128 wątków do 128 CTA po
32 wątki. Dispatch wykorzystuje rozmiar warpa backendu, natomiast T2 i większe
stany zachowują dotychczasową ścieżkę.

`ptxas sm_80` raportuje 32 rejestry i zero spillów dla T3 oraz T4. Pamięć
współdzielona na CTA spadła z 8192 B do 1024 B. Profil `nsys` na RTX 4090:

| Skan | Przed | Po | Przyspieszenie |
|---|---:|---:|---:|
| T3, `32 x 128` | 95,6 us | 36,8 us | 2,60x |
| T4, `32 x 128` | 127,0 us | 48,4 us | 2,63x |

Izolowane porównanie całego wyjścia i wszystkich checkpointów jest bitowo
zgodne. Golden Rust przeszedł względem sekwencyjnego oracle, włącznie z commit
GPU dla akceptacji od zera do K. Pełny smoke ThinkingCap K=3 zachował serial
parity i nie wykonał host gatherów.

Kontrolny pomiar A/B pełnego cyklu MTP dał medianę 38,977 ms dla starego skanu
i 36,685 ms dla kafelkowanego, czyli około 5,9% krótszy cykl. `tok/s` nie jest
tu porównywane bezpośrednio, ponieważ dwa przebiegi miały inną akceptację draftu.
AMD/ROCm i Metal nie były mierzone wykonawczo.

## Prefill DeltaNet block64

Profil `nsys` dla raw512 i chunk128 wskazał 4805 uruchomień w 652,5 ms,
przy 648,7 ms łącznego czasu kerneli. Luki między kernelami wynoszą zatem
około 3,9 ms i nie są głównym ograniczeniem. NVFP4 MMA zajmuje 342,7 ms,
skan DeltaNet 145,5 ms, a Q8 i8mma 101,0 ms.

Dynamiczny skan `d_state=128` używa teraz kafla co najmniej 64-wątkowego.
Na NVIDIA zmienia to `grid192/block32` na `grid96/block64`; AMD z wave64
zachowuje szerokość 64. Wynik i stan są bitowo zgodne dla T=128, 48 głów oraz
po 20 kolejnych iteracjach. Izolowany czas spadł z 790,75 us do 756,92 us.

Kontrolne A/B pełnego raw512/chunk128, po pięć repetycji:

| Wariant | Mediana prefill | Przepustowość |
|---|---:|---:|
| block32 | 652,417 ms | 784,8 tok/s |
| block64 | 642,345 ms | 797,1 tok/s |

GPU gather embeddingu nie jest obecnie opłacalnym kierunkiem dla tego modelu:
transfer 512 wierszy zajmuje około 0,42 ms, natomiast pełna tabela F16 wymagałaby
około 2,54 GB dodatkowego VRAM.

## Q8_0 B3/B4 DP4A na NVIDIA

Krótkie projekcje Q8_0 w warstwach DeltaNet mają osobny wariant NVIDIA z
czterema wierszami na CTA. Każdy 32-elementowy iloczyn int8 jest wykonywany
ośmioma instrukcjami DP4A zamiast rozszerzania 32 wartości do int32 i redukcji
wektorowej. Inne backendy zachowują dotychczasowy przenośny kernel ośmiu
wierszy na CTA.

Profil pełnego verifiera zawiera dla każdego T po 48 wywołań `6144 x 5120`,
48 wywołań `5120 x 5120` i 96 wywołań `48 x 5120`. Izolowany pomiar z
niezerowymi wagami i aktywacjami:

| T | Kształt | Przed | DP4A | Zmiana |
|---|---|---:|---:|---:|
| B3 | 48 x 5120 | 5,19 us | 4,01 us | -22,7% |
| B3 | 5120 x 5120 | 15,01 us | 12,26 us | -18,3% |
| B3 | 6144 x 5120 | 15,35 us | 14,29 us | -6,9% |
| B4 | 48 x 5120 | 6,16 us | 4,62 us | -25,0% |
| B4 | 5120 x 5120 | 16,87 us | 13,89 us | -17,7% |
| B4 | 6144 x 5120 | 17,83 us | 17,66 us | -0,9% |

Ważenie zgodnie z liczbą wywołań daje około 13% dla B3 i 11,5% dla B4.
Wyniki F16 i F32 są bitowo zgodne dla wszystkich trzech kształtów. `ptxas
sm_80` raportuje 50 rejestrów dla B3 i 44 dla B4, zero spillów i zero barier;
dotychczasowe kernele używały odpowiednio 79 i 80 rejestrów.

## Odrzucone NVFP4 Q8_1/DP4A B3/B4

Sprawdzono kwantyzację całego krótkiego batcha jednym wywołaniem i projekcję
DP4A zachowującą kolejność szeregowego GEMV. Ścieżka była tylko diagnostyczna i
nie została włączona do runtime ani do produkcyjnego zestawu artefaktów.

Golden z niezerowymi wagami, kodami aktywacji i skalami przeszedł bitowo dla
T3/T4. Skorygowany benchmark objął rzeczywiste projekcje ThinkingCap: gated Q
`12288x5120`, K/V `1024x5120`, O `5120x6144`, gate/up `17408x5120` oraz down
`5120x17408`. Exact uwzględnia koszt jednego prepassu kwantyzacji batcha.

| Kształt | F16 B3 | exact B3 | F16 B4 | exact B4 |
|---|---:|---:|---:|---:|
| 12288 x 5120 | 36,90 us | 93,46 us | 42,95 us | 127,00 us |
| 1024 x 5120 | 7,74 us | 12,66 us | 8,97 us | 15,66 us |
| 5120 x 6144 | 23,56 us | 48,33 us | 27,04 us | 65,56 us |
| 17408 x 5120 | 45,78 us | 119,33 us | 53,90 us | 166,29 us |
| 5120 x 17408 | 53,01 us | 116,46 us | 62,32 us | 161,91 us |

Exact DP4A jest 1,6-3,1 razy wolniejszy, dlatego launcher, zmienna środowiskowa,
benchmark i PTX zostały usunięte.

## NVIDIA NVFP4 F16 B1

B1 używa dokładnie tej samej matematyki co produkcyjne B3/B4: dwóch wierszy na
CTA, wspólnej LUT E2M1, szerokiego odczytu 16 aktywacji i tej samej redukcji
warpa. B3/B4 są bitowo identyczne z odpowiednio trzema i czterema osobnymi
wywołaniami B1 dla każdego wiersza wszystkich pięciu rzeczywistych kształtów.

| Kształt | stary F16 B1 | nowy B1 | DP4A B1 | Q8_1 prepass |
|---|---:|---:|---:|---:|
| 12288 x 5120 | 77,07 us | 24,81 us | 98,31 us | 2,18 us |
| 1024 x 5120 | 9,75 us | 5,42 us | 10,38 us | 2,14 us |
| 5120 x 6144 | 39,40 us | 15,19 us | 42,30 us | 2,24 us |
| 17408 x 5120 | 107,74 us | 33,49 us | 138,18 us | 3,03 us |
| 5120 x 17408 | 95,45 us | 30,47 us | 112,64 us | 2,82 us |

`ptxas sm_80` raportuje 31 rejestrów, zero spillów i 64 B pamięci współdzielonej.
Profil `nsys` całego miksu 2000 wywołań dał średnio 21,35 us dla B1, 66,06 us
dla starego F16 i 79,57 us dla DP4A. Jawny launcher
`gemv_nvfp4_gguf_b1_f16` przeszedł kontrolę pierwszego stanu bitowo. Pełne
prose A/B zachowało parity:

| Prompt / target | serial | n-gram | akceptacja | oracle |
|---|---:|---:|---:|---:|
| 128 / 128 | 37,508 tok/s | 116,923 tok/s | 96,9% | 129,976 tok/s |
| 512 / 128 | 36,086 tok/s | 63,964 tok/s | 95,2% | 124,873 tok/s |

Test repeat również zachował parity: 37,450 wobec 37,453 tok/s dla raw128 oraz
36,064 wobec 36,065 tok/s dla raw512. Akceptacja n-gram wyniosła zero, ponieważ
model nie kontynuował zadanego wzorca, więc wszystkie 128 kroków użyło fallbacku.
Natywne MTP także zachowało parity:

| Prompt | serial | MTP | akceptacja | cykl p50 |
|---|---:|---:|---:|---:|
| raw128 | 37,500 tok/s | 86,516 tok/s | 74,2% | 36,987 ms |
| raw512 | 36,097 tok/s | 82,993 tok/s | 73,9% | 38,557 ms |

B1 przeszedł komplet testów kernelowych, prose, repeat i natywnego MTP.

## Router MTP + n-gram

Tryb `mtp+ngram:3` najpierw używa pełnego draftu n-gram. Trafiony draft jest
weryfikowany batchem targetu, po czym eager catch-up przesuwa KV i carry MTP po
`fed + accepted` bez logitów. Brak pełnego draftu uruchamia natywne MTP. Audyt
wymuszonych akceptacji 0..3 potwierdził bitową zgodność target h/x/SSM oraz MTP
hidden/K/V/len z referencją sekwencyjną.

Pomiary actual, trzy osobne uruchomienia, prose K=3:

| Prompt / target | serial | `mtp+ngram` | akceptacja | n-gram / fallback MTP |
|---|---:|---:|---:|---:|
| 128 / 128 | 37,503 tok/s | 118,897 tok/s | 97,0% | 32 / 1 |
| 512 / 128 | 36,111 tok/s | 76,688 tok/s | 61,6% | 21 / 25 |

Kontrola repeat raw128 nie znalazła pełnego draftu n-gram: 40/40 cykli przeszło
przez fallback MTP, zachowując parity i osiągając 86,97 tok/s. Wartości
`oracle_upper` nie są raportowane jako wynik actual.

### Rollout N/N B2

Brak `FORGE_MTP_NGRAM_BATCH` wybiera `auto`: dwa pełne drafty n-gram o tym samym
K=2 albo K=3 używają wspólnego source-agnostic target verifiera B2 tylko dla
strukturalnie zgodnego modelu na zweryfikowanym NVIDIA warp32. `0` wymusza B1,
a `1` pozwala uruchomić przenośną ścieżkę na eksperymentalnym backendzie, nadal
wymagając capability modelu. AMD/Metal w `auto` pozostają w B1. Draft ID są
pakowane na GPU, a różne długości retained obu lane doganiają MTP bez logitów
przez trzy kernele Mojo:

- `mtp_norm_join_shifted_segmented_f16`;
- `kv_append_batch_segmented_masked_f16`;
- `mtp_commit_catchup_metadata_segmented`.

Golden syntetyczny obejmuje K2/K3, macierz retained 1..T dla obu lane, osobny
initial hidden, izolację stron i lane, canary oraz granice buforów. Testy hostowe
obejmują cancel/reuse pending draftu i atomową prewalidację commitu obu lane.
Mały `compute-sanitizer --tool memcheck` dla poprawnego masked append zakończył
się `ERROR SUMMARY: 0 errors`.

Segmentowana attention NVIDIA zachowuje kolejność redukcji dokładnego verifiera
seryjnego: cztery warpy dzielą pozycje kontekstu, a warp 0 scala części w tej
samej kolejności. Porównanie bitowe obejmuje T1 przy ctx1, T6 przy granicach
stron ctx31/32/33 i ctx128 oraz T8 przy ctx512/2048, obie kolejności lane,
rozłączne mapy stron i canary. Mikrobenchmark produkcyjnego exact4/fallbacku
przenośnego wyniósł 2,96/5,70 us dla ctx4, 19,58/111,92 us dla ctx128,
67,33/440,77 us dla ctx512 i 239,19/1608,67 us dla ctx2048. Względny błąd L2
wyniósł od 5,63e-9 do 2,97e-5, a maksymalny błąd od 5,96e-8 do 2,44e-4;
benchmark przerywa pracę powyżej odpowiednio 0,002 i 0,001.

Realny N/N E2E dla 27B, jeden warmup i pięć prób mierzonych:

| Prompt | gate ON | gate OFF | Zysk | E2E ON/OFF | N/N B2 / przebieg |
|---|---:|---:|---:|---:|---:|
| raw128 | 159,70 tok/s | 122,87 tok/s | +29,98% | 96,04/82,30 tok/s | 32/0 |
| raw512 | 94,33 tok/s | 83,78 tok/s | +12,59% | 37,66/35,85 tok/s | 20/0 |

Pełne ID są identyczne ON/OFF we wszystkich próbach. Macierz retained K2/K3,
lane swap oraz cancel/reuse zachowały pełny snapshot MTP. Profil raw512 objął
40 cykli N/N (warmup + próba) i dokładnie 40 uruchomień każdego kernela catch-up:
norm/join, maskowanego append KV i commitu metadanych. Każdy commit miał osobną
końcową synchronizację kontekstu. Mieszane parowanie N/M nie jest zaimplementowane.
Licznik Prometheus `forge_engine_mtp_ngram_b2_steps_total` rośnie dopiero po
udanej wspólnej weryfikacji.

Smoke rollout raw128 po zmianie wartości domyślnej, jeden warmup i jedna próba,
potwierdził `auto=32`, `0=0` i `1=32` wspólne kroki N/N. Próby mierzone
osiągnęły odpowiednio 162,60, 122,76 i 162,72 tok/s. Pełne ID wszystkich trzech
trybów są identyczne, SHA-256 listy ID:
`1b4e2f2977962ce09f40540d3149a284a2a662a8025ecf45efe2432ce6af91bb`.
Realne testy lane-swap i cancel/reuse/izolacji przeszły na tym samym modelu 27B.

## Batchowy KV-only catch-up MTP

Catch-up po chunku targetu nie potrzebuje logitów ani wyjścia pełnego bloku MTP.
Kernel Mojo `mtp_norm_join_shifted_f16` przygotowuje cały batch z carry sprzed
chunka i przesuniętych hidden targetu. Runtime wykonuje potem wspólne `eh_proj`,
normę wejścia, projekcje K/V, K-norm, RoPE i jeden zapis do osobnego KV MTP.

Pomiar RTX 4090, chunk 128, trzy próby mierzone:

| Prompt | target p50 | catch-up stary | catch-up KV-only | SHA parity |
|---|---:|---:|---:|---|
| raw128 | 171,008 ms | około 52,8 ms | **2,814 ms** | tak |
| raw512 | 689,421 ms | 227,696 ms | **11,262 ms** | tak |

Raw512 zachował SHA `c45733e9...be27` względem jawnego przebiegu referencyjnego.
Decode 128 zachował SHA `1512c5c9...aacf`, 34
forwardy weryfikacji i 94 zaakceptowane tokeny. Golden kernela obejmuje T=1, 2,
31, 32 i 33 oraz bitową niezmienniczość podziału 32=2x16. Audyt byte-snapshot
dla wymuszonych akceptacji 0..3 potwierdził bez różnic carry, logiczne K/V i
długość KV MTP względem ścieżki tokenowej.

Izolowany pomiar target prefill na tych samych ID tokenów, bez wliczania TTFT:

| Silnik, pure MTP bez n-gramu | raw128 | raw512 |
|---|---:|---:|
| llama.cpp | **1995,54 tok/s** | **2651,05 tok/s** |
| FORGE target | **748,5 tok/s** | **742,7 tok/s** |

Referencyjny serial llama.cpp dla raw512 osiągnął **2758,09 tok/s**. Wyniki tej
tabeli opisują wyłącznie target prefill; osobny catch-up MTP FORGE wyniósł
11,262 ms, a pełny TTFT obejmuje oba etapy oraz pozostały narzut żądania.

## Współdzielona LUT E2M1 w exact BM128

Produkcyjny kernel NVIDIA `gemm_nvfp4_gguf_mma_f16_bm128` dekoduje 16 wartości
E2M1 przez jedną LUT FP32 w pamięci współdzielonej CTA. BM32 oraz przenośne
kernele pozostają niezmienione. Golden porównuje wszystkie bity BM128 z BM32
dla `T=129`, 67 wierszy, niepełnych kafli, skali wyjściowej 0,625 i specjalnej
skali UE4M3 `0x7f`.

Pomiar RTX 4090 używał pięciu osobnych procesów dla każdego wariantu, po jednym
przebiegu rozgrzewającym w procesie, chunka 128 i tych samych plików tokenów:

| Prompt | BM128 arytmetyczny | BM128 LUT | Przyspieszenie | SHA krótkiego decode |
|---|---:|---:|---:|---|
| raw128 | 170,695 ms | 162,643 ms | 1,0495x | `7afc17ec...fca9` |
| raw512 | 689,329 ms | 655,513 ms | 1,0516x | `b415d6ba...4f38` |

Łączny czas obu workloadów spadł z 860,024 do 818,156 ms, czyli o 5,1%.
Pełny raw128 decode zachował SHA `1512c5c9...aacf`, 34 forwardy weryfikacji,
94 zaakceptowane tokeny i osiągnął 102,8 tok/s. Profil `nsys` raw512 po zmianie
wskazał 50,6% czasu w BM128, 21,2% w dynamicznym in-place scan DeltaNet, 14,6%
w exact i8mma i 5,1% w dynamicznym prepare.

## Kafelek BN32 dla projekcji 1024 w prefill

Wariant `gemm_nvfp4_gguf_mma_f16_bm128_bn32` zmniejsza kafelek wyjściowy
BM128 z 64 do 32 wierszy i używa czterech warpów. Dispatch wybiera go wyłącznie
dla 128 tokenów, 1024 wierszy oraz GPU NVIDIA z warpem 32. Inne kształty nadal
używają BN64, ponieważ pomiar samodzielny wykazał dla nich regresje od 6,1% do
65,5%. Golden BN32 zachował bitową zgodność z BN64 dla niepełnych kafli,
specjalnej skali UE4M3 `0x7f` i skali wyjścia 0,625.

Pomiar raw512 na RTX 4090 obejmował pięć osobnych procesów, po jednym przebiegu
rozgrzewającym i jednym mierzonym, chunk 128, bez speculative decode:

| Wariant | Czasy target prefill | Mediana | Przepustowość | SHA decode |
|---|---|---:|---:|---|
| BN64 | 641,801; 645,780; 645,812; 646,508; 646,657 ms | 645,812 ms | 792,8 tok/s | `b415d6ba...4f38` |
| BN32 dla 1024 | 637,434; 641,509; 641,454; 641,312; 641,808 ms | **641,454 ms** | **798,2 tok/s** | `b415d6ba...4f38` |

Mediana poprawiła się o 0,675%. Profil `nsys` zachował 11 926 uruchomień
kerneli. BN32 użył `grid=32`, `block=128` i zajął 24,853 ms dla 256 wywołań
obejmujących warmup oraz pomiar, czyli około 12,43 ms na prefill wobec około
17,18 ms dla poprzedniego kernela. Dodatkowy artefakt PTX ma 24 837 bajtów i
nie wymaga dodatkowego bufora roboczego w VRAM.

## Exact shared-state scan DeltaNet

Dla jednej głowy rekurencja ma postać
`S_t = a_t (I - beta_t k_t k_t^T) S_(t-1) + beta_t k_t v_t^T`.
Równoległy prefix-scan wymagałby składania par gęstych operatorów afinicznych.
Traci to strukturę rank-1, zwiększa koszt względem sekwencyjnego `O(T D^2)` i
zmienia kolejność działań FP32, więc nie może zachować bitowej zgodności
wymaganej przez silnik.

Zamiast zmieniać kolejność obliczeń kernel block64 przechowuje kafel stanu
`128x64` FP32 w 33 792 bajtach pamięci współdzielonej przez cały chunk T=128.
Stan jest czytany z globalnej pamięci raz i zapisywany raz po ostatnim tokenie.
Dla 48 głów i D=128 ogranicza to globalny ruch stanu z około 1,50 GiB do 6 MiB,
bez zmiany wzorów, kolejności akumulacji, liczby kerneli ani buforów runtime.

Standalone zachował bitową zgodność wyjścia i stanu z dotychczasowym block64 i
przyspieszył skan z 746,54 do 613,14 us, czyli o 17,9%. Pomiar raw512 na RTX
4090 obejmował pięć osobnych procesów:

| Wariant | Czasy target prefill | Mediana | Przepustowość | SHA decode |
|---|---|---:|---:|---|
| block64 global state | 637,434; 641,509; 641,454; 641,312; 641,808 ms | 641,454 ms | 798,2 tok/s | `b415d6ba...4f38` |
| block64 shared state | 614,890; 613,038; 612,566; 612,880; 612,700 ms | **612,880 ms** | **835,4 tok/s** | `b415d6ba...4f38` |

Mediana poprawiła się o 4,46%. Profil `nsys` wykazał spadek łącznego czasu
skanu z 276,127 do 221,455 ms dla warmup i pomiaru, czyli o 19,8%, przy tej
samej liczbie 11 926 uruchomień kerneli. Artefakt używa 40 rejestrów, nie ma
spilli i jest wybierany wyłącznie dla T=128, D=128 oraz block64.
Pełny raw128 decode Native MTP zachował SHA `1512c5c9...aacf`, 34 forwardy
weryfikacji i 94 zaakceptowane tokeny, osiągając 102,1 tok/s.

## Odrzucone carry MTP w F32

Sprawdzono eksperymentalne przechowywanie `h_nextn` w F32 przy zachowaniu
aktywacji dla głowy logitów w F16. Pomiar A/B używał tego samego buildu po
włączeniu B1 i dokładnej atencji, promptu prose oraz trzech powtórzeń.

| Prompt | wariant | MTP tok/s | akceptacja | akceptacja pozycji 1/2/3 | cykl p50 |
|---|---|---:|---:|---:|---:|
| raw128 | F16 | 105,09-105,61 | 96,0% | 100,0 / 97,0 / 90,9% | 36,817 ms |
| raw128 | F32 | 99,27-99,36 | 88,6% | 100,0 / 89,5 / 76,2% | 36,838 ms |
| raw512 | F16 | 70,65-72,31 | 59,3% | 71,4 / 54,3 / 52,1% | 38,526 ms |
| raw512 | F32 | 72,62-72,67 | 60,4% | 73,9 / 57,2 / 50,0% | 38,303 ms |

F32 poprawiło długi kontekst o około 2,8%, ale obniżyło przepustowość raw128 o
około 5,8% i akceptację o 7,4 punktu procentowego. Wariant został całkowicie
wycofany; produkcyjny carry MTP pozostaje w F16.

## Opcjonalny draftowy head NVFP4

`FORGE_MTP_DRAFT_HEAD=nvfp4` tworzy na GPU osobną kopię `output.weight` tylko
dla proposera MTP. Target i verifier zachowują źródłowy Q8_0. Packer Mojo
konwertuje Q8_0 bez pełnego bufora F16: każdy blok 64 wartości dostaje cztery
skale UE4M3 i kody E2M1. Kopia ma 715 161 600 B wobec 1 350 860 800 B Q8_0.
Domyślna wartość `q8` nie wykonuje dodatkowej alokacji ani konwersji.

Golden porównał packed stream bajt w bajt z referencją CPU, a GEMV F32 przeszedł
tolerancję i kontrolę top-1. Profil `nsys` zmierzył 780,97 us na wywołanie headu
wobec 1,435 ms dla Q8_0, czyli przyspieszenie 1,84x. Jednorazowy pack podczas
ładowania zajął 479,90 ms.

Pięć osobnych procesów na RTX 4090 zachowało tokeny sekwencyjnego greedy:

| Prompt | Mediana NVFP4 | Q8_0 | llama.cpp | Akceptacja | Verify |
|---|---:|---:|---:|---:|---:|
| raw128 | 110,759 tok/s | 101,7 tok/s | 111,0 tok/s | 98,0% | 33 |
| raw512 | 78,495 tok/s | 76,0 tok/s | 87,65 tok/s | 62,2% | 45 |

Wariant pozostaje opcjonalny: poprawia oba workloady, ale raw512 nadal nie
osiąga wyniku llama.cpp. Serwer z `max_active=2` przeszedł preflight przy puli
weights 20,5 GiB, `ctx=1024` i 40 stronach KV. Automatyczny podział puli na tej
karcie był zbyt mały już dla źródłowego headu Q8_0, dlatego ten ciasny wariant
konfiguracji wymaga jawnego `--weights-pool-gb`.
