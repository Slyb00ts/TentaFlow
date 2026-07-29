# Dwie różne karty jako jeden silnik — plan

Cel postawiony wprost: **nie chodzi o to, żeby działało jak najwolniejsza karta,
tylko żeby sumowało ich moc.** Ten dokument jest planem wykonawczym opartym na
pomiarach z tej maszyny, a nie na ogólnych zaleceniach.

## 1. Zmierzone fakty, na których stoi cały projekt

| | RX 6900 XT (gfx1030) | RX 7900 XT (gfx1100) | stosunek |
|---|--:|--:|--:|
| VRAM | 16 368 MiB | 20 464 MiB | 1 : 1,25 |
| odczyt DRAM (ciągły) | **336 GB/s** | **735 GB/s** | 1 : 2,19 |
| int8 `dot4` | **97 TOPS** | 43 TOPS | **2,26 : 1** |
| WMMA int8 / f16 | brak jednostki | 98 TOPS / 102 TFLOPS | — |

Połączenie (zmierzone `hipMemcpyPeer`, obie strony):

| pomiar | wartość |
|---|--:|
| P2P dostępne | TAK, dwukierunkowo |
| pasmo strumieniowe (64 MiB) | 14,2 GB/s |
| kopia 10 KiB w strumieniu | 6,63 us |
| **wymiana 10 KiB + synchronizacja na zdarzeniu urządzenia** | **11,21 us** |
| wymiana 10 KiB + synchronizacja hosta obu strumieni | 35,2 us |

Liczby z 2026-07-29 pochodzą już z PRODUKCYJNEJ ścieżki HAL
(`crates/forge-cli/examples/peer_probe.rs`), a nie ze spike'u obok silnika —
`Device::enable_peer_access` otwiera P2P, `copy` idzie przez `hipMemcpyAsync`
z adresowaniem współdzielonym, a `record_event`/`wait_event` synchronizują
strumienie DWÓCH RÓŻNYCH kart bez powrotu do hosta.

### Wniosek, który przesądza o architekturze

**Stosunek mocy tych kart ZALEŻY OD RODZAJU PRACY i nie da się go wyliczyć
z parametrów.** Pokazuje to zestawienie tego, co dawałoby rozumowanie, z tym, co
wychodzi z pomiaru ścieżki produkcyjnej:

| miara | 6900 XT | 7900 XT | stosunek |
|---|--:|--:|--:|
| odczyt DRAM | 336 GB/s | 735 GB/s | 1 : 2,19 |
| sama instrukcja `dot4` | 97 TOPS | 43 TOPS | **2,26 : 1** (odwrotnie!) |
| **zmierzony GEMM NVFP4 (ścieżka produkcyjna)** | **1,1 TOPS** | **9,7 TOPS** | **1 : 8,8** |

Trzeci wiersz jest tu najważniejszy i przeczy drugiemu. Z samej instrukcji `dot4`
wynikałoby, że w prefillu wygrywa 6900 XT. W rzeczywistości przegrywa go
ośmiokrotnie, bo 7900 XT liczy tę warstwę na WMMA, a 6900 XT bez jednostki
macierzowej kończy się na wsadzie T=16 i musi robić osiem razy więcej przebiegów
po wagach.

**To jest dowód, że stosunku mocy nie wolno wyprowadzać z parametrów kart —
trzeba go zmierzyć tym, czym karta REALNIE liczy.** Dlatego kalibracja idzie
przez produkcyjne wejście `gemm_nvfp4_gguf_f16`, które rozgałęzia się na `mma`
(NVIDIA), WMMA (RDNA3) i warianty przenośne, i mierzy każdą kartę jej NAJLEPSZĄ
dostępną ścieżką przy NAJWIĘKSZYM wsadzie, jaki ta karta obsłuży.

## 2. Co która technika daje i czego nie daje

| | co dzieli | ruch na warstwę | zysk przy jednym strumieniu |
|---|---|---|---|
| **TP** (tensor parallel) | każdą macierz wag po wierszach | 2 wymiany hidden (10 KiB) | **TAK** — obie karty liczą tę samą warstwę |
| **PP** (pipeline parallel) | warstwy między karty | 1 wymiana hidden na granicę | **NIE** — token i tak przechodzi obie karty po kolei |
| **EP** (expert parallel) | ekspertów MoE | tokeny skierowane do eksperta | tak, gdy eksperci są rozrzuceni |

To jest kluczowe rozróżnienie i łatwo je przeoczyć: **pipeline parallel NIE
przyspiesza pojedynczego strumienia.** Sumę mocy przy jednym żądaniu daje
wyłącznie TP (albo EP w modelu MoE). PP daje pojemność (36 GB zamiast 20) i
przepustowość przy wielu równoległych żądaniach.

Skoro cel brzmi „nie jak najwolniejsza, tylko suma", to **TP jest techniką
pierwszą**, a PP dokładamy dla modeli, które nie mieszczą się na jednej karcie.

### Budżet komunikacji TP — czy to się w ogóle spina

27B, dekodowanie: 65 warstw x 2 punkty wymiany = 130 na token.

- naiwnie (synchronizacja hosta): 130 x 35,2 us = **4,6 ms** wobec ~15 ms liczenia → 30% narzutu, **nie do przyjęcia**
- na zdarzeniach urządzenia (zmierzone): 130 x 11,21 us = **1,46 ms** → **9,7%**, akceptowalne

Dla Bielika 7B (40 warstw, krok dekodowania ~10,8 ms) wychodzi 80 x 11,21 us =
0,90 ms, czyli 8,3%. Oba przypadki mieszczą się w budżecie, ale margines jest
mniejszy, niż zakładał pierwotny szacunek 6,45 us — dlatego liczy się teraz
zmierzona wartość, nie sama kopia bez synchronizacji.

**Warunek konieczny TP: synchronizacja przez zdarzenia HIP między strumieniami
urządzeń, nigdy przez `hipStreamSynchronize` na hoście.** Bez tego cały zysk
znika w narzucie.

### Ile TP realnie da

Dekodowanie 27B jest ograniczone pamięcią i mierzy dziś 36,5 tok/s na 7900 XT
(95% jej sufitu). Przy podziale proporcjonalnym do pasma:

- suma pasm 336 + 735 = **1071 GB/s**
- czas = 19,55 GB / 1,071 = 18,3 ms + 0,84 ms komunikacji = 19,1 ms
- **~52 tok/s, czyli 1,43x wobec samej 7900 XT i 3,2x wobec samej 6900 XT**

To jest dokładnie „średnia ich wydajności" z pytania — a ściślej SUMA, bo praca
dzieli się proporcjonalnie do możliwości.


## 2b. Techniki „pomiędzy" — co robić, gdy łącze nie uniesie pełnego TP

Pełny tensor parallel to jeden punkt na osi, nie jedyny wybór. Poniżej budżety
policzone na ZMIERZONYM łączu tej maszyny (11,21 us na wymianę z synchronizacją,
14,2 GB/s):

| technika | wymian na warstwę | Bielik 7B | Qwen 27B | czego wymaga od łącza |
|---|--:|--:|--:|---|
| pełny TP | 2 | 0,94 ms (8,7%) | 1,55 ms (6,5%) | najniższego opóźnienia |
| **TP tylko FFN** | 1 | 0,47 ms (4,4%) | 0,78 ms (3,2%) | połowy tego, co pełny TP |
| **podział spekulacyjny** | 0 | ~0 | ~0 | praktycznie niczego |
| **sekwencyjny prefill** | 1 duża | 6,4 ms na KV | 10,3 ms na KV | pasma, NIE opóźnienia |
| pipeline | 1 na granicę | pomijalne | pomijalne | najmniej ze wszystkich |

**TP tylko FFN.** Blok FFN to około dwóch trzecich mnożeń warstwy, a przy
podziale kolumnowym `gate`/`up` i wierszowym `down` wystarcza mu JEDNA redukcja.
Uwagę liczą wtedy obie karty redundantnie. Oddajemy jedną trzecią potencjalnego
zysku, płacąc połową komunikacji — czyli dokładnie ten kompromis, o który chodzi
przy łączu na granicy.

**Podział spekulacyjny.** Model draftujący na słabszej karcie, weryfikujący na
mocniejszej. Przez łącze idą SAME identyfikatory tokenów — kilkadziesiąt bajtów
na krok. Pasmo i opóźnienie przestają mieć znaczenie, więc to jedyna technika,
która przyspiesza POJEDYNCZY strumień przez zwykłą sieć. Dodatkowo sama z siebie
pasuje do kart o różnej mocy: draft jest tani, więc trafia na słabszą kartę, a
kosztowna weryfikacja na mocniejszą. FORGE ma już natywne MTP i n-gram, więc
draft jest na miejscu.

**Sekwencyjny prefill.** Prefill dzielony po TOKENACH, nie po macierzach: każda
karta liczy inny fragment promptu. Wymaga wymiany KV fragmentu (2,1 MB na
warstwę), ale to transfer ograniczony PASMEM, który da się nałożyć na liczenie —
opóźnienie łącza prawie nie gra roli. Przy prefillu 512 tokenów trwającym ~210 ms
te 6-10 ms transferu to kilka procent, i to możliwych do ukrycia.

Wybór nie jest konfiguracją: `topology::choose_technique` dostaje zmierzone
łącze, profil warstwy i rodzaj pracy, i schodzi od najbardziej dochodowej
techniki do najbardziej odpornej na słabe łącze. Prefill idzie osobną gałęzią,
bo tam podział po tokenach bije podział po macierzach na wolnym łączu.

## 3. Serce projektu: model możliwości i zamknięta pętla

Podział NIE jest stałą w konfiguracji. Każde urządzenie ma profil:

```
struct DeviceCapability {
    stream_bytes_per_s: f64,   // zmierzone pasmo odczytu (decode)
    matmul_ops_per_s: f64,     // zmierzona przepustowość GEMM (prefill)
    free_bytes: usize,         // ile wag się zmieści
}
```

Dwa źródła:
1. **Kalibracja przy starcie** — krótki mikrobenchmark na każdej karcie
   (odczyt strumieniowy + jeden GEMM w formacie modelu). Ułamek sekundy.
2. **Korekta z obserwacji** — po każdym kroku znamy rzeczywisty czas etapu na
   każdej karcie. Jeśli karta A kończy wcześniej i czeka, jej udział rośnie.
   Wygładzanie wykładnicze, limit zmiany na krok, żeby nie oscylowało.

Podział wierszy `rows_i = round(rows * w_i / suma(w))`, gdzie `w_i` to
przepustowość właściwa dla RODZAJU pracy (pasmo dla decode, ops dla prefillu).
Reszta z zaokrąglenia idzie do karty o największym `w_i`.

**Ograniczenie pamięciowe:** udział karty nie może przekroczyć jej wolnego VRAM.
Przy 16 GB i 20 GB oraz podziale 31/69 model 27B zajmuje 5,7 GB i 12,5 GB —
mieści się z zapasem.

## 4. Kolejność wykonania

| krok | zawartość | dlaczego w tej kolejności |
|---|---|---|
| **M1** ✅ | otwarcie N kart w jednym procesie, kalibracja per karta, P2P przez HAL, kopia między urządzeniami i synchronizacja na zdarzeniach — wszystko zmierzone | bez tego nie da się zrobić niczego |
| **M2** ✅ | `DeviceCapability` + planer + kalibracja + pętla korekty, 10 testów | to jest odpowiedź na „nie jak najwolniejsza" |
| **M3** | TP dla dekodowania: podział kolumnowy qkv/gate/up, wierszowy o/down, redukcja na zdarzeniach | pierwszy realny zysk przy jednym strumieniu |
| **M4** | TP dla prefillu z WŁASNYM podziałem (inny stosunek mocy!) | prefill ma odwrotny stosunek kart |
| **M5** | PP dla modeli ponad 20 GB + mikrobatching | pojemność i przepustowość |
| **M6** | EP dla MoE na bazie istniejącej rezydencji ekspertów | MoE ma już warstwę migracji ekspertów |
| **M7** | TP tylko FFN — połowa komunikacji pełnego TP | dla łączy na granicy opłacalności |
| **M8** | podział spekulacyjny draft/target między karty | jedyna technika działająca przez zwykłą sieć |
| **M9** | sekwencyjny prefill dzielony po tokenach | prefill znosi opóźnienie, potrzebuje pasma |

## 5. Ryzyka nazwane wprost

- **Dwie architektury w jednym procesie.** Rejestr ma już wkompilowane oba
  zestawy artefaktów (`EMBEDDED_GFX1030` i `EMBEDDED_GFX1100`), a `Kernels::load`
  wybiera po architekturze urządzenia — więc to powinno działać, ale wymaga
  sprawdzenia, bo dotąd nikt nie tworzył dwóch `Kernels` w jednym procesie.
- **Kernele różnią się między kartami.** 6900 XT nie ma WMMA, więc ta sama
  warstwa liczy się tam innym kernelem. Wyniki muszą pozostać zgodne — bramką
  jest suma SHA tokenów, tak jak dotąd.
- **DeltaNet ma stan rekurencyjny.** Podział TP warstwy DeltaNet wymaga albo
  replikacji stanu, albo podziału po głowicach. Podział po głowicach jest
  naturalny (48 głowic), ale trzeba sprawdzić, czy skan się na to zgadza.
- **Kolejność redukcji zmienia bity.** Suma częściowych wyników z dwóch kart w
  innej kolejności niż dziś da inne ostatnie bity. Trzeba świadomie wybrać
  deterministyczną kolejność (zawsze urządzenie 0, potem 1) i udokumentować, że
  wynik TP nie musi być bitowo równy jednokartowemu.

## 6. Stan wykonania

**Zrobione i sprawdzone na sprzęcie:**

- **Dwie architektury w jednym procesie działają.** `gfx1030` i `gfx1100`
  otwarte równocześnie, każda z własnym zestawem artefaktów — to było główne
  ryzyko techniczne i jest zdjęte.
- **Planer podziału** (`crates/forge-engine/src/multi_gpu.rs`) z 10 testami:
  podział proporcjonalny, ODWRÓCENIE stosunku dla pracy ograniczonej liczeniem,
  limit VRAM, dokładna suma udziałów przy niewygodnych liczbach, próg
  opłacalności, zbieżność pętli korekty i jej odporność na pojedynczy zakłócony
  pomiar.
- **Kalibracja na realnych kartach, OBIE osie**: pasmo 208 / 505 GB/s (kopia D2D)
  oraz liczenie 1,1 / 9,7 TOPS przez produkcyjne wejście GEMM. Planer dzieli
  dekodowanie **29 / 71%**, a prefill **10 / 90%** — dwa różne podziały z dwóch
  różnych pomiarów, dokładnie tak, jak wymaga tego sprzęt.
- **Adresowanie kart niezależne od producenta**: `DeviceId { backend, ordinal }`
  plus `gpu::enumerate()` listujące karty ze WSZYSTKICH wkompilowanych backendów.
  Dotąd karta była adresowana samym numerem, a backend wybierany pierwszym
  trafieniem — czyli RTX 4090 i RX 7900 XT w jednej maszynie były NIE DO
  zaadresowania równocześnie. To był cichy blokier dla par mieszanych.

**Nie zrobione — i to jest większość pracy:**

M3-M6, czyli same techniki równoległości. Fundament stoi, ale żadna z nich nie
liczy jeszcze ani jednego tokena na dwóch kartach naraz. Do TP brakuje w HAL
kopii między urządzeniami i synchronizacji na zdarzeniach (bez niej, na
synchronizacji hosta, narzut zjada 30% zysku — patrz sekcja 2).
