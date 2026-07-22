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

Końcowy wariant retained przechowuje checkpoint stanu dla każdej warstwy
DeltaNet podczas pierwszego skanu. Commit wybiera już obliczony stan odpowiadający
zaakceptowanemu prefiksowi, zamiast uruchamiać drugi skan 48 warstw. Decyzja o
długości zaakceptowanego prefiksu, wybór wiersza korekcyjnego i commit stanu
verifiera należą do trwałego grafu GPU. CPU uruchamia cykl, synchronizuje jego
wynik i odczytuje dwa słowa sterujące; obliczenia modelu i sampling greedy są na
GPU.

`--speculative mtp` ustawia maksymalny budżet K=3 i adaptacyjnie porównuje tempo
K=2 oraz K=3. Dostępne są też jawne `mtp:2` i `mtp:3`. Każda próba benchmarku
porównuje pełną sekwencję tokenów z sekwencyjnym greedy i przerywa się przy
różnicy.

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
- Domyślne `max_active=1` ogranicza pulę; większa jawna wartość przechodzi startup
  preflight. Verifier przechwytuje osobne grafy T=3/T=4 dla każdego slotu.
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
Każda zmiana musi zachować porównanie token po tokenie z sekwencyjnym greedy.

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
