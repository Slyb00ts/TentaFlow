# Zrównoleglenie na wiele kart — plan oparty na pomiarze

Stanowisko: dwie Radeon AI PRO R9700 (`gfx1201`, 32 GiB każda), model
ThinkingCap-Qwen3.6-27B (`qwen35`): **gęsty** (bez MoE), ale **hybrydowy** —
48 warstw Gated-DeltaNet + 16 warstw uwagi + 1 blok MTP.

## Dlaczego to się opłaca — liczby, nie intuicja

Dekodowanie jest ograniczone CZYTANIEM WAG: 17 GB na token przy 551 GB/s to
~31 ms, i faktycznie mierzymy 34 ms. Podział wag na dwie karty daje 8,5 GB na
kartę, czyli ~15,5 ms.

Koszt synchronizacji zmierzony na tym sprzęcie (`tests/cluster_peer.rs`):

| | wartość |
|---|--:|
| peer access między kartami | **jest** |
| wymiana ukrytego stanu tokena (10 KiB) | **5,58 us** |
| 65 warstw x 2 wymiany na token | **0,73 ms** |

Czyli 0,73 ms narzutu wobec ~15,5 ms oszczędności: **komunikacja zjada 4,5%
zysku**. Sam transfer nie jest więc przeszkodą — i pomiar Etapu A to potwierdza:
tym, co ogranicza zysk, jest zakres podziału i narzut uruchamiania kerneli, a
nie łącze między kartami.

## Etap A — wykonany i zmierzony

`--tp-cards` dzieli FFN modelu hybrydowego: `gate`/`up` po wierszach, `down` po
kolumnach, jedna wymiana wejścia i jedna redukcja sum cząstkowych na warstwę.
Dostępne w `run`, `serve` i `bench` (ten ostatni ma też `--tp-split` do
narzucenia udziałów).

Pomiar z tego stanowiska, prompt 512 tokenów, 128 tokenów dekodu, mediana z 3 prób:

| model | 1 karta | 2 karty | zysk |
|---|--:|--:|--:|
| Qwen3.6-27B NVFP4 | 30,0 tok/s | **38,4 tok/s** | +28,0% |
| Qwen3.6-27B Q4_K_M | 28,3 tok/s | **35,3 tok/s** | +24,7% |
| NVFP4 + MTP | 73,2 tok/s | 72,7 tok/s | bez zmian |

Dzielone są: FFN (`gate`/`up` po wierszach, `down` po kolumnach), obie duże
projekcje wejściowe DeltaNet (`in_proj` NVFP4 i `gate_proj` Q8_0 — różne formaty,
wspólny udział wierszy) oraz głowa logitów Q8_0 po wierszach słownika.

Prefill jest nietknięty (nadal jedna karta): ~1195 tok/s w obu przypadkach —
patrz etap C, gdzie podział po tokenach został sprawdzony i odrzucony.

### Dlaczego +20%, a nie +100%

Podział obejmuje FFN, czyli 9,78 GB z 17,36 GB wag — pozostałe 44% (projekcje
uwagi i DeltaNet) czyta nadal jedna karta. Do tego krok dekodu NIE jest czystym
odczytem pamięci: przy ~1270 uruchomieniach kerneli na token i zmierzonym
narzucie rzędu 4,5 us na uruchomienie sama dyspozycja zajmuje ~5,7 ms z 33,9 ms
kroku, a ta część nie maleje od dołożenia karty. Rachunek domyka się na 27,8 ms
wobec zmierzonych 27,8 ms.

Sprawdzone i ODRZUCONE jako wyjaśnienie: utrata odtwarzania grafu HIP. Krok
hybrydowy bez grafu (`FORGE_HYBRID_DECODE_GRAPH=0`) daje 29,5 wobec 30,0 tok/s,
czyli graf wart jest 1,7%, a nie brakujących kilkudziesięciu procent.

### Co naprawiono po drodze

- Sonda kalibracyjna mierzyła dla NVFP4 kernel, którego model nigdy nie
  uruchamia (wariant f16 zamiast grupowego Q8_1/dp4a), i brała JEDNĄ serię
  pomiarową. Na dwóch IDENTYCZNYCH kartach dawało to podziały od 7872/9536 do
  10048/7360. Po poprawce (właściwy kernel + najkrótsza z pięciu serii) rozrzut
  spadł do 1,5%, a wymuszony podział równy przestał być szybszy od liczonego.
- Plan podziału wyceniał tylko `down`, choć karta rezerwuje jeszcze `gate` i
  `up` — trzykrotne zaniżenie. Liczył też pojemność osobno dla każdej warstwy z
  odczytu sprzed ładowania, więc 65 warstw po kolei uznawało całą pulę za wolną.
  Wolne miejsce było ponadto odczytywane PRZED sondami kalibracyjnymi, które
  biorą bufory z tej samej puli i ich nie zwracają. Suma tych trzech: podział z
  MTP kończył się brakiem pamięci zamiast mniejszym udziałem karty modelu.
- Próg dp4a wybierany był z szerokości KAWAŁKA karty, a nie całej macierzy —
  macierz szersza od progu szła bez dp4a na jednej karcie, a jej połowy z dp4a,
  czyli podział zmieniał matematykę modelu.
- Podział sięgał po wolniejsze kernele niż ścieżka jednokartowa. Wpięcie
  grupowego Q8_1 dla `gate`/`up` i nowego `gemv_nvfp4_gguf_q8_1_out_f32` dla
  `down` dało 33,3 -> 35,2 tok/s. Nowy kernel jest bramkowany goldenem: liczy
  bit w bit to samo co wariant f16, różni je wyłącznie szerokość zapisu.
- Ogon redukcji to było uruchomienie na kopię, dodawanie i zawężenie osobno —
  130 uruchomień na token. Nowy `add_f32_out_f16` domyka redukcję i zawężenie
  jednym kernelem, a karta modelu liczy wprost z bufora silnika zamiast z kopii:
  35,2 -> 36,0 tok/s.

### Zgodność wyników

NVFP4 zachowuje SHA 128 tokenów `0bf2b86b…` z jedną kartą, także z MTP. Q4_K_M
NIE — i to jest oczekiwane, a nie defekt: podział kolumnowy sumuje iloczyn w
innej kolejności niż jedna karta, więc ostatnie bity się różnią i przy dłuższej
generacji któryś argmax musi w końcu zmienić stronę. Wygenerowany tekst na
realnym prompcie jest w obu przypadkach identyczny. Bit-w-bit zgodność jest
własnością podziału WIERSZOWEGO (wiersze są niezależne), nie kolumnowego.

## Etapy pozostałe

**B. TP dla projekcji uwagi i DeltaNet.** `q`/`k`/`v` kolumnowo, `o` wierszowo.
DeltaNet dzieli się po GŁOWICACH — stan rekurencyjny jest per głowica i niezależny,
więc karta trzyma stan swoich głowic bez wymiany w skanie. To obejmuje pozostałe
44% wag i jest jedyną drogą, żeby dekodowanie zbliżyło się do 2x.

**C. TP w prefillu — podział po tokenach SPRAWDZONY I ODRZUCONY.**
Zaimplementowany i zmierzony: pełne macierze FFN na karcie wspierającej, ogon
tokenów liczony tam w całości (więc bitowo zgodnie), prefiks na karcie modelu.
SHA nie drgnął, ale przepustowość też nie: 1199 tok/s na jednej karcie wobec
1197 na dwóch, i żaden udział z zakresu 0,4-1,0 tego nie odwrócił.

Profil `rocprofv3` mówi dlaczego. Karta modelu wykonuje realnie MNIEJ pracy
(902,6 -> 705,9 ms kerneli), karta wspierająca dokłada 307,2 ms — ale ŁĄCZNA
praca rośnie z 565 do 670 ms samego GEMM NVFP4. Obie karty strumieniują PEŁNE
macierze FFN, więc koszt odczytu wag się DUBLUJE, a dzieli się tylko arytmetyka.
Karta wspierająca kończy swoją połowę później, niż karta modelu kończy swoją, i
zaoszczędzone ~98 ms na prefill schodzi na czekanie i wymianę. Kod usunięty —
kosztował 9,3 GiB VRAM na karcie wspierającej i czas ładowania za zero.

Wniosek dla prefillu: musi to być podział po WAGACH (każda karta czyta połowę
macierzy), a nie po tokenach. To z kolei wymaga GEMM z wyjściem f32 dla
projekcji `down`, bo sumy cząstkowe dwóch kart zapisane w f16 zmieniałyby wynik
prefillu, czyli cały strumień tokenów. Bez tego kernela podział prefillu nie ma
bitowo zgodnej ścieżki.

**D. Podział ścieżki weryfikacji MTP.** Dziś MTP omija podział całkowicie: krok
spekulacyjny to weryfikacja T=3 tokenów osobnym, segmentowanym łańcuchem, do
którego `TpFfn` nie jest wpięty. Stąd 73,7 wobec 73,0 tok/s. Wymaga wariantu
batchowego podziału, nie GEMV.

**E. Pipeline parallel.** Podział po warstwach zamiast po wagach. Nie skraca
opóźnienia pojedynczego żądania tak jak TP, ale skaluje się na więcej kart i
zdejmuje limit VRAM dla większych modeli. Sensowne jako WARSTWA NAD TP przy 4+
kartach.

**F. Expert parallel dla MoE.** Eksperci są z natury niezależni, więc to
najprostsza forma podziału — ale ten checkpoint nie ma MoE, więc bez modelu
testowego byłby to kod niesprawdzony na realnych danych.

## Kolejność

B -> C -> D -> E -> F. B daje najwięcej, bo to jedyne pozostałe 44% wag w
ścieżce dekodu. C jest drugie, bo prefill już wygrywamy z llama.cpp i podwojenie
go przesuwa przewagę z 1,3x na ~2,5x. D odblokowuje podział dla najszybszego
trybu, jaki mamy. E i F nie mają dziś czym być zweryfikowane na tym stanowisku.

**Bramka jakości: wynik podziału musi zgadzać się z jednokartowym co do
kolejności redukcji, jaką narzuca geometria.** Podział wierszowy ma być bit w
bit; kolumnowy wolno różnić się ostatnimi bitami sumowania, ale nie doborem
kerneli ani matematyką.
