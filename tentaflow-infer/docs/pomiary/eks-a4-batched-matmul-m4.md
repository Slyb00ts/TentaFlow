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

| T | kafel | pętla GEMV | przyspieszenie |
|---:|---:|---:|---:|
| 1 | 1142,7 us | 344,4 us | 0,30x |
| 8 | 719,2 us | 2097,9 us | **2,92x** |
| 32 | 3839,6 us | 8402,3 us | **2,19x** |
| 128 | 16345,6 us | 34150,8 us | **2,09x** |
| 512 | 193624,7 us | 140221,2 us | 0,72x |

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

## Zgodność

Kafel i forma wektorowa dają **identyczne bity** na tej samej pozycji —
kolejność sumowania jest w obu ta sama z konstrukcji. Sprawdza to
`the_batched_form_agrees_with_the_vector_form_bit_for_bit`, a poprawność
arytmetyki wobec MLX i prawdy w f64 —
`batched_matmul_is_no_further_from_the_truth_than_mlx`.
