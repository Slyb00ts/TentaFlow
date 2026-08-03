# EKS-A4 — batchowy dequant-matmul wobec pętli GEMV (Apple M4)

Pytanie: ile faktycznie daje kafel tokenów w mnożeniu przez wagę 4-bitową,
czyli w operacji, która zajmuje prefill niemal w całości.

Maszyna: Apple M4, 10 rdzeni GPU. Stałe z EKS-A1: przepustowość pamięci
102,4 GB/s, szczyt FMA f32 3,07 TFLOPS.

Kształt: `down_proj` Bielika w pełnym rozmiarze — 4096 wierszy, 11264 kolumn,
grupa 64, 4 bity. Waga zajmuje 23 MB, więc nie mieści się w cache i jej odczyt
jest realnym ruchem, a nie trafieniem w L2.

Uruchomienie: `cargo test -p forge-kernels --features metal --test
msl_qmm_vs_mlx -- --ignored --nocapture`.

## Wynik

Geometria dobrana pomiarem, nie z góry: kafel 8 tokenów, 2 wiersze wyjścia na
grupę SIMD, 32 wiersze na grupę roboczą (uzasadnienie każdej liczby niżej).

| T | kafel | na token | pętla GEMV | przyspieszenie |
|---:|---:|---:|---:|---:|
| 1 | 1003,8 us | 1003,8 us | 326,7 us | 0,33x |
| 8 | 643,3 us | 80,4 us | 2210,0 us | **3,44x** |
| 32 | 2368,4 us | 74,0 us | 8354,0 us | **3,53x** |
| 64 | 4647,6 us | 72,6 us | 16725,3 us | **3,60x** |
| 128 | 9255,3 us | 72,3 us | 33332,0 us | **3,60x** |
| 192 | 13918,6 us | 72,5 us | 50089,9 us | **3,60x** |
| 256 | 18639,9 us | 72,8 us | 66742,2 us | **3,58x** |

Koszt na token jest płaski przez cały zakres — degradacja przy dużych kaflach,
którą miała wcześniejsza wersja, zniknęła wraz z blokowaniem rejestrowym.

## Dwie liczby geometrii, obie zmierzone

**Wierszy wyjścia na grupę SIMD: 2.** To był największy pojedynczy skok:
102,9 na 72,3 us na token. Przy jednym wierszu każda wczytana aktywacja karmi
dokładnie jedno mnożenie, więc pętla wykonuje mniej więcej tyle odczytów, co
FMA, i nie ma jak zbliżyć się do jednostek arytmetycznych. Dwa wiersze dzielą
każdy odczyt między dwa mnożenia. Cztery i osiem już się nie mieszczą:

| wierszy na grupę SIMD | T=128 |
|---:|---:|
| 1 | 102,9 us/token |
| 2 | **72,3 us/token** |
| 4 | 130,1 us/token |
| 8 | 447,3 us/token |

Po tej zmianie szerokość grupy roboczej przestała mieć znaczenie (16, 32 i 64
wiersze dają 72-74 us), bo redundantne odczyty aktywacji już nie są problemem.

**Wierszy wyjścia na grupę roboczą: wcześniej 16.** Kafel tokenów decyduje, ilu tokenów
obsłuży jeden odczyt WAG; liczba wierszy decyduje, ile wierszy wyjścia obsłuży
jeden odczyt AKTYWACJI. Przy dużych kaflach to drugie zaczyna dominować.

| wiersze | T=32 | T=128 | T=512 |
|---:|---:|---:|---:|
| 4 | 2,19x | 2,09x | 0,72x |
| 8 | 2,32x | 2,21x | 0,81x |
| 16 | **2,47x** | **2,43x** | 1,25x |
| 32 | 2,43x | 2,15x | 1,55x |

32 wiersze wygrywają dopiero przy T=512, czyli poza naszym punktem pracy, i
płacą za to przy 128. Wybrane 16.

**Kafel tokenów: 8.** Większy kafel dzieli odczyt wag na więcej tokenów, więc
„powinien" pomagać. Nie pomaga — akumulatory przestają się mieścić w rejestrach:

| kafel | T=32 | T=128 |
|---:|---:|---:|
| 8 | **107 us/token** | **109 us/token** |
| 16 | 204 us/token | 210 us/token |
| 32 | 192 us/token | 199 us/token |

## Co z tego wynika

**Kafel jest dla prefillu, forma wektorowa dla dekodowania.** Przy jednym
tokenie kafel liczy osiem kolumn, z których siedem wyrzuca, i jest przez to
trzykrotnie wolniejszy. To nie jest wada do naprawienia, tylko powód, dla
którego oba kernele zostają.

**Powyżej stu kilkudziesięciu tokenów kafel przestaje wygrywać.** Przy T=512
siatka ma 64 kafle w osi tokenów, a każdy z nich czyta całą wagę I cały swój
wycinek aktywacji; wycinek dla ośmiu tokenów to 180 KB, więc przy tylu grupach
aktywacje przestają się mieścić w cache i zaczynają być czytane z pamięci
tyle razy, ile jest wierszy wyjścia. Stąd reguła: **prefill idzie kawałkami po
128 tokenów**, a nie jednym wywołaniem na cały prompt.

## Dwie rzeczy zmierzone po drodze, obie sprzeczne z oczekiwaniem

**Pierwsza wersja kafla była wolniejsza od pętli — przy każdym T.** Przy T=1,
gdzie oba kernele wykonują tę samą pracę, kafel potrzebował 1351 us wobec
294 us. Ta sama praca i czterokrotna różnica nie jest kwestią przepustowości.
Przyczyną była pętla po tokenach o długości ustalanej w czasie wykonania
(`t < tail`): przy zmiennej liczbie przebiegów kompilator nie rozwija pętli i
tablica akumulatorów przestaje być rejestrami. Poprawka nie dotyczy
arytmetyki — liczba przebiegów jest teraz stała, a ogon kafla wskazuje na
ostatni token i liczy wynik, którego nikt nie zapisuje.

**Odczyt aktywacji ośmioma wartościami naraz zamiast po jednej pogorszył
wynik**, z 2,92x na 1,29x przy T=8. Wektorowy odczyt jest tu typową „oczywistą
optymalizacją", która nie działa, i dlatego został wycofany, a nie zostawiony
z komentarzem, że powinien pomagać.

## Trzecia rzecz, która nie zadziałała: aktywacje w pamięci grupy roboczej

Rachunek ruchu pamięci przy T=128 wychodzi na 1,1 GB w 13,9 ms, czyli 79 GB/s
przy suficie 102 — i dwie trzecie tego to AKTYWACJE, czytane osobno przez każdą
z 16 grup SIMD. Postawienie ich raz w pamięci grupy roboczej powinno więc ściąć
ruch trzykrotnie.

Pierwsza wersja wyszła 188 us na token wobec 109 wyjściowych. Winne były
dzielenia całkowite w pętli ładującej (`i / halfs_here`), a nie sam pomysł: po
ich usunięciu kernel schodzi do 102,9 us i przestaje tracić przy dużych
kaflach — koszt na token jest płaski aż do 256 tokenów.

I mimo to **na modelu jest wolniej**: 59,2 wobec 67,5 tok/s prefillu. Osiem
kilobajtów pamięci grupy roboczej obniża zajętość, a w modelu ten kernel dzieli
układ z kilkunastoma innymi, podczas gdy w mikrobenchmarku ma go dla siebie.
Wycofane — i opisane, żeby nie wracać do tego z tym samym rachunkiem w ręku.
Wniosek ogólniejszy: sam rachunek ruchu pamięci nie wystarcza, bo redundantne
odczyty aktywacji i tak są wyłapywane przez cache.

## Jednostki macierzowe

Blokowanie rejestrowe doszło do 1,29 TFLOPS, czyli 42% szczytu FMA. Reszta to
mieszanka instrukcji — i tu wchodzi `simdgroup_matrix`, zmierzony w EKS-A2 na
3,94 TFLOPS wobec 3,07 dla zwykłego FMA.

Trudność jest jedna: wagi są 4-bitowe, a jednostka macierzowa nie umie ich
czytać. Rozwiązaniem jest **dekwantyzacja bloku wagi RAZ do pamięci grupy
roboczej**, po czym ten sam rozpakowany blok karmi wszystkie fragmenty tokenów.
Przy bloku 32 tokenów koszt rozpakowania nibbli dzieli się przez 32 zamiast być
płacony na token.

Blok i podział na grupy SIMD są **przemierzone**, nie dobrane z góry (T=128):

| blok BMxBNxBK | us/token |
|---|---:|
| 32x32x32 | 50,0 |
| 64x32x32 | 41,3 |
| 32x32x64 | 43,0 |
| 64x64x32 | 47,0 |
| 32x128x32 | 39,1 |
| **32x64x32** | **39,6** |

| podział grup SIMD (tokeny x wiersze) | us/token |
|---|---:|
| 2x2 (128 wątków) | 39,7 |
| 1x2 (64 wątki) | 49,4 |
| 2x1 (64 wątki) | 49,0 |
| 2x4 (256 wątków) | 41,0 |
| **1x4 (128 wątków)** | **39,2** |

Mniej grup SIMD to więcej fragmentów na grupę, czyli więcej mnożeń na jeden
odczyt — i mimo to 64 wątki przegrywają wyraźnie: brak równoległości kosztuje
więcej, niż daje lepszy stosunek.

Trzy rzeczy, które przy tym kernelu NIE pomogły, wszystkie sprawdzone:
dopełnienie kroku wiersza wag przeciw konfliktom banków (39,7 wobec 39,6),
transponowane wystawianie wagi z ciągłym zapisem (40,6), oraz zapis wyniku
wprost z jednostki macierzowej zamiast przez pamięć grupy roboczej (40,9) —
to ostatnie dlatego, że `simdgroup_store` do pamięci urządzenia rozrzuca osiem
wierszy co `n_rows`, a droga przez pamięć wspólną ten zapis skleja.

Blok 32x64x32, cztery grupy SIMD w układzie 1x4, akumulator f32:

| T | macierzowo | blokowo | pętla GEMV |
|---:|---:|---:|---:|
| 1 | 1375,7 us/token | 1016,8 us/token | 344,3 us |
| 8 | 176,7 us/token | **79,7 us/token** | 266,1 us/token |
| 32 | 58,5 us/token | 74,1 us/token | 263,6 us/token |
| 128 | **38,7 us/token** | 72,2 us/token | 262,3 us/token |
| 256 | 37,4 us/token | 73,0 us/token | 270,3 us/token |

Poniżej pełnego bloku forma macierzowa przegrywa — przy 8 tokenach zostawia
trzy czwarte bloku pustych. Stąd trzy formy z ZMIERZONYMI zakresami: wektorowa
dla jednego tokenu, blokowa poniżej 32, macierzowa od 32 w górę.

Aktywacje, rozpakowane wagi i wynik bloku dzielą JEDNĄ tablicę 4 KB w pamięci
grupy roboczej: w pętli po K żyją tam pierwsze dwie, po niej trzecia. Osiem
kilobajtów zamiast czterech kosztowałoby zajętość, co ten sam dokument mierzy
wyżej przy odrzuconym kafelkowaniu aktywacji.

Cena: **forma macierzowa nie jest bitowo zgodna** z wektorową, bo jednostka
sumuje po swojemu. Trzyma ją więc ten sam próg liczbowy co MLX — nie dalej od
prawdy w f64 niż sama wyrocznia — a nie równość bitowa.

## Zgodność

Kafel i forma wektorowa dają **identyczne bity** na tej samej pozycji —
kolejność sumowania jest w obu ta sama z konstrukcji. Sprawdza to
`the_batched_form_agrees_with_the_vector_form_bit_for_bit`, a poprawność
arytmetyki wobec MLX i prawdy w f64 —
`batched_matmul_is_no_further_from_the_truth_than_mlx`.

## Wynik na całym modelu

Kernel to jedno, a model to drugie: w warstwie są też uwaga, normalizacje i
obroty, a one też przeszły na kafel. Bielik-7B 4-bit, prompt 256 tokenów,
`cargo test --release -p forge-model --features metal --test generate_vs_mlx --
--ignored --nocapture`:

| ścieżka | czas | przepustowość |
|---|---:|---:|
| token po tokenie | 11,541 s | 22,2 tok/s |
| kaflami po 128 | 1,614 s | **158,6 tok/s** |

Przyspieszenie **7,15x**. Uwaga liczy teraz wszystkie zapytania kafla jednym
wywołaniem, zamiast jednego na token, więc całość zyskuje więcej niż samo
mnożenie.

Dekodowanie po prefillu: 19,1 tok/s, czyli powyżej celu 19 tok/s z planu (§7.6).
Cel prefillu to 175 tok/s, więc ta ścieżka jest na 91% drogi.

Gdzie jest sufit teraz: 38,7 us na token przy T=128 to 2,38 TFLOPS wobec
zmierzonych 3,94 TFLOPS dla jednostki macierzowej, czyli 60%. Model liczy przy
tym 2,26 TFLOPS, czyli **96% czasu prefillu to same mnożenia** — uwaga,
normalizacje, obroty i reszta warstwy mieszczą się w pozostałych czterech
procentach. Dalsze przyspieszenie prefillu musi więc przyjść z tego kernela,
a nie znikąd indziej.

Obie ścieżki wybierają ten sam token — sprawdza to asercja w samym pomiarze,
bo inaczej porównywałby dwie różne prace.
