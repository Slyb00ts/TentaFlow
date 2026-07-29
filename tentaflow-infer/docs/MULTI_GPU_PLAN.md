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

## 2c. Zmierzone na pełnej warstwie (RX 6900 XT + RX 7900 XT)

Kształty warstwy Bielika 7B, wagi Q8_0, dekodowanie jednego tokenu:

| wariant | czas warstwy | przyspieszenie |
|---|--:|--:|
| sama 7900 XT | 291,6 us | 1,00x |
| pełny TP (2 wymiany) | 245,3 us | 1,19x |
| **TP tylko FFN (1 wymiana)** | **233,9 us** | **1,25x** |

Sufit z sumy pasm (207 + 490 wobec 490 GB/s) to 1,42x.

**TP tylko FFN jest tu SZYBSZY od pełnego TP.** To odwraca zwykłe założenie i
wynika wprost z liczb: jedna zaoszczędzona wymiana to 11,3 us na warstwę — czyli
dokładnie tyle, ile zmierzył `peer_probe` — a podział bloku uwagi wnosi mniej,
bo uwaga to niewielka część warstwy w porównaniu z FFN. Wariant „pomiędzy" nie
jest więc tylko ratunkiem dla wolnych łączy; na tej parze kart jest po prostu
lepszym wyborem.

Wniosek dla planera: `choose_technique` powinien porównywać zysk z podziału
uwagi z kosztem wymiany, a nie zakładać, że pełny TP jest zawsze najlepszy tam,
gdzie łącze go uniesie.

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

- **Klaster kart w jednym procesie** (`cluster.rs`): otwarte karty, ich strumienie
  i zdarzenia, dostęp P2P otwarty między każdą parą. Wymiana 10 KiB ze zdarzeniem
  **11,21 us**, przez hosta 35,2 us. Brak P2P nie jest błędem — jest wejściem dla
  planera.
- **Tensor parallel po wierszach** (`tensor_parallel.rs`): podział macierzy z
  ZMIERZONEJ mocy kart, wynik zbierany na jednej karcie, zgodność bitowa z
  przebiegiem jednokartowym. Zbiórka jest punkt-punkt, więc przy kilkunastu
  kartach trzeba ją przerobić na drzewiastą.
- **Pełna warstwa na dwóch kartach**: TP całej warstwy **1,19x**, TP samego FFN
  **1,25x**. Technika „pomiędzy" wygrywa z pełnym TP na tym łączu — dokładnie to,
  co przewidywała sekcja 2b.
- **Pipeline parallel liczy token na wielu kartach.** `Model` przyjmuje zakres
  warstw, etap nie-pierwszy pomija embedding (i skalowanie embeddingu rodziny
  Gemma), a `prefill_stage` kończy na warstwach — bez głowy logitów. Granicą
  etapu jest strumień rezydualny `pb.h` wystawiony przez `stage_hidden`;
  następny etap normalizuje go po swojemu, więc przez łącze idzie wyłącznie
  rezydual. Sprawdzone `pp_stage_probe` na podziale 2, 3, 4 i 5 etapów
  (Bielik 7B, 40 warstw) oraz na 2 etapach Gemma 12B QAT i IT (48 warstw), na
  RX 6900 XT + RX 7900 XT: **wybrany token zgadza się z jednokartowym**.

**Czego pipeline jeszcze nie robi:**

- **Wynik nie jest bitowo zgodny**, tylko zgodny co do wyboru tokenu (max różnica
  logitu 0,14 na Bieliku, 1,2 na Gemmie). Źródło jest znane: `rmsnorm_residual_f16`
  liczy sumę kwadratów z NIEZAOKRĄGLONEJ sumy f32, a etap kolejny normalizuje już
  zaokrągloną wartość f16 ze swojego bufora `h`. Różni się więc sam skalar `inv`.
  Wyrównanie wymaga zaokrąglenia przed podniesieniem do kwadratu w tamtym kernelu,
  co zmienia liczby także na jednej karcie — decyzja świadomie odłożona.
- **Modele hybrydowe (Qwen3.6 z DeltaNet) nie są objęte.** `prefill_stage` je
  wprost odrzuca: hybrydowy prefill ma osobną pętlę warstw i stan rekurencyjny,
  którego granica etapu musi obejmować oprócz rezydualu.
- **Nie ma sterownika produkcyjnego ani zysku prędkości.** Etapy przechodzą przez
  hosta i wykonują się po kolei, więc druga karta czeka. Zysk daje dopiero
  przepychanie mikrowsadów przez `cluster.exchange` i `wait_for`, które są gotowe
  i zmierzone.

## FFN tensor parallel w pętli warstw silnika

`Model::enable_tp_ffn` rozkłada FFN na dodatkowe karty i wpina go w dekodowanie:
`gate`/`up` po wierszach, `down` po kolumnach, JEDNA wymiana na warstwę
(rozgłoszenie wejścia + redukcja sum cząstkowych). Klaster powstaje przez
`Cluster::attach` wokół karty, na której model już stoi — bez otwierania jej po
raz drugi. Krok dekodowania przestaje wtedy być jednym grafem i idzie jawnym
łańcuchem `run_step_separate`.

Zmierzone na Bieliku 7B Q8_0 (RX 6900 XT jako karta modelu + RX 7900 XT, P2P
niedostępny), 64 kroki dekodowania:

| przebieg | jedna karta | podział | zysk |
|---|---|---|---|
| podział 5024/6240 | 58,1 tok/s | 78,7 tok/s | 1,35x |
| podział 4672/6592 | 58,1 tok/s | 80,0 tok/s | 1,38x |

**Dobór kerneli musi być ten sam co w `Model::gemv`.** Pierwsza wersja liczyła
`gate`/`up` dokładnie w f16, podczas gdy silnik dla Q8_0 w zasięgu dp4a
kwantyzuje aktywację do int8. Podział był więc DOKŁADNIEJSZY od odniesienia i
dawał na logitach błąd względny 2,8e-1 — wyglądający jak usterka, a będący inną
matematyką. Po wyrównaniu doboru (dołożony `gemv_q8_0_dp4a_out_f32`) CAŁY podział
na jednej karcie wychodzi **bitowo identycznie** z silnikiem, na obu kartach
osobno.

**Czego podział nie gwarantuje:** zgodności bitowej przy pracy dwóch kart.
Projekcja `down` sumuje ~11 tys. składników, które w dużej mierze się kasują, więc
inna kolejność dodawania f32 daje na logitach błąd względny o kilka rzędów większy
niż samo epsilon. Zmierzone: podział 32/11232 → 8e-6, po połowie → 7e-3, na
64 krokach względne L2 ok. 1,3e-2 i inny argmax w 0–2 krokach na 64. Błąd rośnie
płynnie z wyrównaniem podziału, a jednokartowy jest zerowy — to odróżnia
nieprzemienność f32 od usterki.

**Kalibracja nie jest tu dobrą miarą podziału.** Mierzy duży przebieg
strumieniowy, a warstwa to wąski GEMV; wskazania wahają się między przebiegami
(2624–5024 wierszy dla karty 0) i to właśnie ta wariancja, nie sam podział,
odpowiada za rozrzut wyniku 1,13–1,38x. Dlatego `upload_ffn_split` przyjmuje
podział NARZUCONY (`TP_SPLIT` w `tp_decode_probe`) — dobór progu po pomiarze
zamiast po kalibracji jest osobnym, jeszcze nieprzeprowadzonym krokiem.

**Zakres:** modele gęste z Q8_0, dekodowanie. Prefill liczy FFN macierzowo na
jednej karcie i zachowuje swoją kopię wag, więc podział kosztuje dodatkową pamięć
równą udziałowi karty modelu. MoE, modele hybrydowe, obrotowy cache KV i tiering
są odrzucane wprost przez `tp_ffn_capable`. Kernel `gemv_q8_0_dp4a_out_f32` i
`cast_f32_f16` zbudowano dla gfx1030 i gfx1100; zestawy NVIDIA wymagają
przebudowy katalogu na tamtym sprzęcie.

### Podział dobierany GEMV-em, nie kopią

Zmierzony przemiat podziału (Bielik 7B Q8_0, 32 kroki, wiersze pośrednie karty
modelu z 11264):

| 2048 | 3072 | 4096 | **4608** | 5120 | 5632 | 6144 | 7168 |
|---|---|---|---|---|---|---|---|
| 69,9 | 73,5 | 77,7 | **80,0** | 78,2 | 75,0 | 72,1 | 66,8 tok/s |

Krzywa ma jedno maksimum i jest stroma: skrajne podziały tracą 17%. Kalibracja
mierzyła wcześniej pasmo kopią D2D i wskazywała 2624–5024 wierszy — czyli od
optimum aż po punkt tracący 13%. `measure_device` mierzy teraz `stream_bytes_per_s`
tym samym GEMV-em, którym karta REALNIE liczy dekodowanie (dp4a dla Q8_0/Q4_K,
`gemv_nvfp4_gguf_f16` dla NVFP4), na kształcie 4096x4096.

Efekt: wskazania zwęziły się z 2624–5024 do 5120–5536, a wynik z 1,13–1,38x do
1,30–1,35x. Zostaje jednak SYSTEMATYCZNE odchylenie w prawo: pomiar daje karcie
modelu ok. 48% wymiaru pośredniego, a optimum leży przy 41%. Powód jest znany i
strukturalny — karta zbierająca płaci dodatkowo rozgłoszenie wejścia, dodawanie
sum cząstkowych i rzut f32→f16, więc powinna dostać MNIEJ niż jej udział w samym
GEMV. Wielkości tej poprawki nie zgaduję; do czasu jej zmierzenia ostatnie ~6%
zbiera się podziałem narzuconym ręcznie.

Sam blok FFN na dwóch kartach, po wyrównaniu doboru kerneli: 328,3 us na jednej
karcie wobec 124,0 us na dwóch, czyli **2,65x**. Że w całym dekodowaniu wychodzi
1,35x, a nie 1,6x wynikające z prawa Amdahla dla ok. 60% udziału FFN, mierzy
narzut rozgłoszenia i redukcji — i to jest następne miejsce do poprawy.

### Narzut wymiany, zmierzony i częściowo usunięty

Blok FFN na dwóch kartach liczy się 2,2–3,0x szybciej niż na jednej, a całe
dekodowanie przyspiesza 1,44x. Różnicę mierzy narzut podziału, rozbity na
składniki (`tp_ffn_block_probe`, RX 6900 XT + RX 7900 XT, P2P otwarte):

| co | koszt |
|---|---|
| para zdarzeń między kartami | ok. 15 us |
| kopia 8 KiB (wejście warstwy) | ok. 15 us |
| liczenie całego bloku na dwóch kartach | ok. 110–147 us |

Pierwotnie warstwa płaciła CZTERY pary zdarzeń: dwie na własną pracę (rozesłanie
wejścia i redukcja) i dwie wyłącznie na zmostkowanie strumienia silnika ze
strumieniem klastra karty modelu. Te drugie zniknęły — karta modelu pracuje w
podziale strumieniem SILNIKA (`Cluster::exchange_on` i `Cluster::order`
przyjmują strumień jawnie), więc wejście i wyjście bloku są uporządkowane z resztą
kroku za darmo. Zmierzone: **80,0 → 83,5 tok/s, 1,38x → 1,44x**, przy identycznych
logitach (ta sama max różnica 0,691915) — zmiana jest czysto porządkowa.

`TieredWeightDevice` nie przekazywał `ordinal` ani `enable_peer_access`, a oba mają
domyślną implementację w traicie, więc BRAK przekazania nie był błędem kompilacji,
tylko cichym zmyśleniem: opakowana karta modelu zgłaszała numer 0 i „ten backend
nie obsługuje P2P". Klaster brał to za prawdę o sprzęcie. Po poprawce P2P jest
otwarte w obie strony; na tej parze kart samo otwarcie P2P nie zmieniło czasu
dekodowania — koszt wymiany siedzi w synchronizacji, nie w przepustowości — ale
`ordinal` zwracany z opakowania to usterka niezależna od wydajności.

Zostały dwie pary zdarzeń na warstwę (ok. 30 us) i kopia wejścia (15 us) przy
110–147 us liczenia. Domyślna kalibracja daje 1,36–1,37x, podział dobrany
pomiarem 1,44x.

### Gdzie siedzi reszta narzutu

Rozbite pomiarem, a nie szacunkiem. Podział `11264,0` daje karcie modelu CAŁY
FFN, więc liczy dokładnie tę samą pracę co przebieg jednokartowy i nie wysyła nic
na drugą kartę — a mimo to jest wolniejszy: **55,4 wobec 58,1 tok/s**, czyli
26 us na warstwę. To nie jest koszt wymiany. To premia za odtwarzanie grafu,
której podział nie dostaje: krok jednokartowy jest przechwycony i odtwarzany
jednym wywołaniem, a krok z podziałem idzie setkami uruchomień kerneli.

Przechwycenie kroku obejmującego dwie karty sprawdzone i NIEMOŻLIWE na tym
sterowniku: wzorzec fork/join strumienia przez zdarzenie, który CUDA opisuje jako
poprawny, ROCm przerywa asercją we własnym runtime (`hip::Stream*` …
`Assertion '__n < this->size()' failed`) — zrzut pamięci zamiast błędu do
obsłużenia. Odzyskanie tych 0,83 ms na token wymagałoby pocięcia kroku na odcinki
przechwytywane osobno (graf na warstwę, między nimi wywołanie podziału), czyli
przebudowy `run_step_separate` — to jest następny krok, nie drobiazg.

Dla porządku: samo przerzucenie CAŁEGO FFN na szybszą kartę (`0,11264`) daje
1,13x. Podział daje 1,44x, więc dzielenie pracy jest wyraźnie lepsze niż jej
przeniesienie — i to jest odpowiedź na pytanie, czy warto było robić TP zamiast
prostego offloadu.

### Jak to włączyć

`--tp-cards <numery>` w `forge run` i `forge serve`. Karta modelu jest zawsze
pierwsza, wymienia się tylko te, które mają ją wesprzeć:

```
forge run model.gguf "prompt" --tp-cards 1
forge serve model.gguf --bind 127.0.0.1:8099 --tp-cards 1
```

Sprawdzone na prawdziwej binarce, nie tylko w sondzie. Bielik 7B Q8_0, 64 tokeny,
RX 6900 XT + RX 7900 XT: **72,0 -> 100,3 tok/s** (liczone z prefillem, stąd więcej
niż 58 -> 83 dla samego dekodowania), z IDENTYCZNYM tekstem wyjściowym co do
słowa. `serve` odpowiada poprawnie przez `/v1/chat/completions`.

Bez tej flagi nic się nie zmienia: podział jest wyłącznie na żądanie operatora.
