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

Geometria dobrana pomiarem, nie z góry: kafel 8 tokenów, 16 wierszy wyjścia na
grupę roboczą (uzasadnienie obu liczb niżej).

| T | kafel | na token | pętla GEMV | przyspieszenie |
|---:|---:|---:|---:|---:|
| 1 | 1148,6 us | 1148,6 us | 410,8 us | 0,36x |
| 8 | 923,0 us | 115,4 us | 2308,9 us | **2,50x** |
| 32 | 3435,0 us | 107,3 us | 8494,7 us | **2,47x** |
| 64 | 7413,9 us | 115,8 us | 17074,5 us | **2,30x** |
| 128 | 13902,8 us | 108,6 us | 40649,3 us | **2,92x** |
| 192 | 26819,8 us | 139,7 us | 51282,2 us | 1,91x |
| 256 | 50247,5 us | 196,3 us | 68665,1 us | 1,37x |

Koszt na token jest płaski (107-116 us) do 128 tokenów i od 192 rośnie. Stąd
kafel prefillu 128 — i stąd też wiadomo, że powyżej tej granicy problemem nie
jest już odczyt wag.

## Dwie liczby geometrii, obie zmierzone

**Wierszy wyjścia na grupę roboczą: 16.** Kafel tokenów decyduje, ilu tokenów
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
| token po tokenie | 11,769 s | 21,8 tok/s |
| kaflami po 128 | 3,876 s | **66,0 tok/s** |

Przyspieszenie **3,04x**. Uwaga liczy teraz wszystkie zapytania kafla jednym
wywołaniem, zamiast jednego na token, więc całość zyskuje więcej niż samo
mnożenie.

Dekodowanie po prefillu: 20,4 tok/s, czyli powyżej celu 19 tok/s z planu (§7.6).
Cel prefillu to 175 tok/s, więc ta ścieżka jest na 38% drogi.

Co zostało do wyjaśnienia: 109 us na token przy T=128 to 862 GFLOPS wobec
zmierzonego szczytu 3,07 TFLOPS (28%) i wobec 28 us wynikających z samego ruchu
wag. Ani jedno, ani drugie ograniczenie nie jest jeszcze osiągnięte, więc
przyczyna leży gdzie indziej i nie zgaduję jej tutaj.

Obie ścieżki wybierają ten sam token — sprawdza to asercja w samym pomiarze,
bo inaczej porównywałby dwie różne prace.
