# EKS-A7 — czy CPU i GPU liczą prefill jednocześnie (Apple M4)

Pytanie wyszło od ANE: skoro Apple ma osobną jednostkę do mnożeń, czy nie
policzyć na niej prefillu równolegle z GPU. ANE odpada z powodu, który da się
podać jedną liczbą — jedyne wejście do niej to CoreML, a to kosztuje **20–24 ms
na wywołanie** plus ~7 µs na wiersz, przy naszych warstwach liczących 2–20 ms i
dispatchu Metala 0,61 µs. Praca badająca granice współwykonania na Apple Silicon
wyklucza ANE dokładnie na tej podstawie i bada parę CPU+GPU.

Ten dokument mierzy tę parę u nas.

Maszyna: Mac mini M4 (aktywnie chłodzony), 4 P + 6 E, 16 GB. Model: Bielik-7B
MLX 4-bit.

## Punkt wyjścia

| | |
|---|---:|
| prefill GPU, 256 tokenów | 215,6 tok/s = **3,02 TFLOPS** |
| sufit macierzowy GPU (EKS-A2) | 3,94 TFLOPS |
| pasmo pamięci (EKS-A1) | 102,4 GB/s |

Prefill wykorzystuje 77% mocy obliczeniowej GPU, a pasma prawie nie rusza — wagi
czyta raz na kafel, nie raz na token. **Brakuje mocy, nie pasma**, i to jest
warunek, przy którym dołożenie drugiej jednostki ma sens. Dekodowanie jest
odwrotne: 88,0 z 102,4 GB/s, czyli 86% pasma.

## Ile daje CPU

`cblas_sgemm` na kształtach warstwy Bielika, 256 tokenów:

| kształt | czas | |
|---|---:|---:|
| q_proj / o_proj [4096 x 4096] | 5 649 us | 1,52 TFLOPS |
| k_proj / v_proj [1024 x 4096] | 1 381 us | 1,56 |
| gate / up [11264 x 4096] | 15 237 us | 1,55 |
| down [4096 x 11264] | 17 247 us | 1,37 |

Podtrzymywane przez 25 s: 1,52 TFLOPS średnio, 319 rund bez osypywania się.
To **połowa tego, co obecnie robi GPU** — nie margines błędu.

f16 nie pomaga na prędkość: `BNNSMatMul` daje 1,41 TFLOPS f16 wobec 1,25 f32
(1,13x), czyli AMX nie ma podwojonej ścieżki f16, a i tak wypada poniżej
`cblas_sgemm` f32. f16 kupuje wyłącznie połowę pamięci.

## Czy się sumują

Test rozstrzygający: to samo obciążenie CPU puszczone RÓWNOLEGLE z prefillem na
GPU. Punkty odniesienia GPU pochodzą z bramki `no_batch_size_falls_off_a_cliff`,
czyli z czystego prefillu bez dekodowania.

| wsad | GPU sam | GPU + CPU | strata GPU |
|---:|---:|---:|---:|
| 8 | 12 678,9 us/tok | 12 789,4 | 0,9% |
| 16 | 12 011,7 | 12 079,7 | 0,6% |
| 32 | 11 784,0 | 11 807,2 | 0,2% |
| 64 | 5 208,7 | 5 221,6 | 0,2% |
| 128 | 4 769,4 | 4 790,8 | 0,4% |
| 256 | 4 629,6 | 4 719,8 | 1,9% |

CPU w tym samym oknie: **1,47 TFLOPS** wobec 1,52 osobno (−3%).

**Jednostki się sumują.** GPU traci medianowo 0,3%, CPU 3%. Łącznie dostępne
3,02 + 1,47 = **4,49 TFLOPS** tam, gdzie samo GPU daje 3,02.

Kontrola negatywna z tego samego przebiegu: dekodowanie pod obciążeniem CPU
spadło z **20,9 na 17,9 tok/s** (88,0 → 75,3 GB/s), czyli −14%. Dekodowanie jest
ograniczone pasmem, a pamięć jest wspólna — dokładanie mocy obliczeniowej nic
tam nie da i tylko szkodzi. **Podział wolno włączać wyłącznie w prefillu.**

## Czego brakuje w rachunku: wagi

GPU czyta 4 bity wprost w kernelu, Accelerate nie umie. Dwie drogi:

**Rozpakowanie w locie.** Kosztuje więcej, niż wynikałoby z liczby operacji
(1/256 pracy mnożenia), bo jest ograniczone zapisem 184 MB f32 i skalarne:

| | czas | narzut |
|---|---:|---:|
| samo mnożenie | 15 462 us | — |
| rozpakowanie, jeden wątek | 6 557 us | 41% |
| rozpakowanie, `dispatch_apply` | 4 127 us | **27%** |

Zrównoleglenie daje tylko 1,6x, bo wąskim gardłem jest zapis, nie arytmetyka.
Efektywnie **1,21 TFLOPS**, za to zero dodatkowej pamięci.

**Trwała kopia f16 przydziału CPU.** Zero kosztu na wywołanie, pełne 1,41
TFLOPS, ale przy 32% wierszy to +4,5 GB. Na tej maszynie (16 GB, model 4 GB)
zmieści się; na 8 GB nie.

## Wniosek

Przy podziale wierszy tak, żeby obie jednostki kończyły równocześnie:

| wariant | udział CPU | łącznie | sufit prefillu |
|---|---:|---:|---:|
| rozpakowanie w locie | 29% | 4,23 TFLOPS | +40% → 302 tok/s |
| trwała kopia f16 | 32% | 4,43 TFLOPS | +47% → 317 tok/s |

To są SUFITY, bez kosztu bariery synchronizacji na każdym mnożeniu (7 na
warstwę, 40 warstw = 280 barier na kafel) i bez uwagi, której się nie dzieli.
Literatura mierzy w tym samym układzie 1,15–1,38x na poziomie bloku, ale
**1,18–1,25x end-to-end na TTFT** — i ta druga liczba jest uczciwszą prognozą:
215,6 → **~260 tok/s wobec 218 u MLX**.

Znana pułapka: przy leniwym grafie bez jawnych punktów materializacji ten sam
podział daje 0,66x, czyli szkodzi. Nas to nie dotyczy — dispatchujemy zachłannie
do bufora poleceń, nie budujemy leniwego grafu jak MLX. To akurat jest przewaga,
którą mamy za darmo z wcześniejszej decyzji architektonicznej.

## Co wyszło po wdrożeniu

Zaimplementowane jako podział wierszy: GPU liczy `[0, split)` skróconą siatką,
CPU resztę, rozpakowując swój wycinek w locie (zero dodatkowej pamięci).
Pomiar PRZEPLATANY — `MlxDense::set_cpu_share(false)` daje odniesienie w tej
samej sesji i temperaturze, bo maszyna dryfuje i dwa osobne przebiegi
porównywałyby temperatury:

| prompt | samo GPU | z CPU | zysk | MLX (ta sama sesja) |
|---|---:|---:|---:|---:|
| 256 | 216,1 / 216,2 tok/s | 238,6 / 232,4 | **+8,9%** | 218,9 |
| 1024 | 214,8 / 215,0 | 249,1 / 255,1 | **+17,3%** | 216,7 |

Czyli **+7,6% nad MLX przy 256 i +16,3% przy 1024** — pierwszy raz przewaga,
a nie parytet. Dłuższy prompt zyskuje więcej, bo kafle są wtedy pełne (512),
a rozpakowanie amortyzuje się liczbą tokenów.

Dekodowanie bez zmian: 20,8 tok/s przy 87,6 GB/s, bo forma dekodowania nie
kwalifikuje się do podziału i nie ma jak w niego wejść.

### Dwie rzeczy, które wyszły dopiero z pomiaru

**Bariera nie jest tam, gdzie jej szukałem.** Spodziewałem się kosztu za osobny
command buffer. Prawdziwym kosztem okazało się coś innego: CPU czyta aktywacje
`x` własnymi instrukcjami, a te są produkowane przez kernele stojące w
NIEZATWIERDZONYM buforze poleceń. `commit` tylko kolejkuje. Bez czekania na `x`
CPU mnoży to, co akurat leżało w buforze — nie ma awarii, jest inny model.
Bramka na logitach pokazała to jako względną L2 **1,66**; po dodaniu czekania
spadła do **1,15e-2**. To jedyny powód, dla którego ta ścieżka ma własne
czekanie na strumień, mimo że EKS-A3 zabrania powrotów na hosta per warstwa.

**Próg musi patrzeć na tokeny, nie na pracę.** Rozpakowanie kosztuje
proporcjonalnie do `wiersze x kolumny`, a mnożenie do `wiersze x kolumny x
tokeny` — więc udział rozpakowania rośnie, gdy wsad maleje. Pierwsza wersja
progu (sama praca) dzieliła też przy 128 tokenach i dawała tam **-17%**, przy
+10,9% dla 256. Stąd osobny próg `MIN_SPLIT_TOKENS = 256`.

### Zgodność

Podział zmienia arytmetykę (CPU akumuluje w f32 przez BNNS — sprawdzone
detektorem: suma 4096 jedynek wychodzi dokładnie, więc nie f16), więc kontrakt
jest tolerancyjny, ale nie dowolny: największa różnica logitów wobec samego GPU
to **0,24% rozpiętości**, przy naszej udokumentowanej różnicy wobec MLX rzędu
0,4–2,6%. Podział jest więc mniejszym źródłem błędu niż wszystko, co już
uznaliśmy za poprawne. Osobno bramka wymaga, żeby przy marginesie zwycięzcy
powyżej 5% rozpiętości wybór był IDENTYCZNY — zmierzony margines 26,2%, wybór
ten sam.

## Druga runda — profil zamiast szacunków

`nsys` i `perf` nie istnieją na Apple; odpowiednikiem jest `sample`/`xctrace`.
Profil głównego wątku podczas prefillu 1024 tokenów rozkłada się tak:

| | udział |
|---|---:|
| libBNNS — CPU mnoży | **70,6%** |
| jądro — czekanie na GPU | 19,3% |
| nasz kod (rozpakowanie + kodowanie dyspozycji) | 9,5% |

Czyli CPU jest zajęty w 80%, a nie bezczynny. Pierwsze odczytanie profilu było
błędne — `prefill` ma KILKA gałęzi `forward`, więc patrzenie na jedną sugerowało
75% czekania, którego nie ma.

### Rozpakowanie przez tablicę wartości

Szesnaście wartości to CAŁY alfabet wagi 4-bitowej, a grupa dzieli jedną skalę i
jedno przesunięcie — więc wartości grupy da się zbudować raz i potem tylko
czytać, zamiast mnożyć, dodawać i zaokrąglać do half osobno dla każdego z 64
elementów. Rozpakowanie **1345 → 938 us (-30%)**, cała połowa CPU z 1,35 na
**1,40 TFLOPS**.

### Udział nie jest stałą, tylko funkcją wsadu

Przemiatanie na obu końcach daje dwa różne optima:

| wsad | 0,68 | 0,70 | 0,72 | 0,74 | 0,76 |
|---|---:|---:|---:|---:|---:|
| 256 tokenów | — | 230,6 | 247,4 | **256,5** | 240,8 |
| 512 tokenów | 257,0 | **261,9** | 258,5 | 254,2 | — |

Optimum przesuwa się, bo rozpakowanie kosztuje tyle samo przy 256 co przy 512
tokenach, a jest czym je ukryć o połowę mniej — im mniejszy kafel, tym gorsza
efektywna przepustowość CPU i tym więcej ma wziąć GPU. Kafle są ograniczone do
512, więc dwa zmierzone punkty i prosta między nimi pokrywają wszystko, co może
wystąpić. Stała 0,72 z pierwszej rundy była kompromisem między dwoma przypadkami
i przegrywała z każdym z nich osobno.

### Próg pracy

Obniżony z 6 na 3 GiB: k/v przy 512 tokenach (4,3 GiB pracy) opłaca się dzielić
i dają **+1,2%**, podczas gdy te same k/v przy 256 (2,1 GiB) nadal zostają
całe, bo granica kosztowałaby piątą część ich czasu GPU.

### Wynik drugiej rundy

| prompt | pierwsza runda | druga runda | MLX (ta sama sesja) | przewaga |
|---|---:|---:|---:|---:|
| 256 | 238,6 tok/s | **257,2** | 218,9 | **+17,5%** |
| 1024 | 256,1 | **261,7** | 216,6 | **+20,8%** |

Wobec punktu wyjścia (samo GPU, 215,6 / 214,9) to **+19,3%** i **+21,8%**.

Dekodowanie nadal nietknięte: 21,1 tok/s przy 89,0 GB/s. Zgodność poprawiła się
przy okazji — największa różnica logitów 0,0531 zamiast 0,0814, czyli 0,16%
rozpiętości.

### Sprawdzone i odrzucone

**Wsad 128.** Nawet z tańszym rozpakowaniem 5291,8 wobec 4766,1 us/token bez
podziału. Próg zostaje na 256.

**Większy udział CPU przy 512.** 0,66 daje 247,3 wobec 261,9 — CPU nie jest
stroną, która czeka, mimo 19,3% czekania w profilu. Te 19,3% to synchronizacja
PRZED podziałem, na pracę, której CPU nie umie przejąć (uwaga, normy, k/v przy
krótkim kaflu), a nie czekanie po nim.

## Trzecia runda — kafel i kolejność

**Rozmiar kafla był dobrany przed istnieniem ścieżki CPU.** `PREFILL_CHUNK`
zostało ustawione na 512 z pomiaru samego GPU. Ale rozpakowanie amortyzuje się
liczbą tokenów w kaflu, więc dołożenie CPU przesunęło to optimum. Mediany z
trzech przebiegów, prompt 2048:

| kafel | 512 | 1024 | 2048 |
|---|---:|---:|---:|
| tok/s | 241,7 | **269,5** | 235,7 |

1024 wygrywa o 11,5%; przy 2048 wraca ten sam problem, dla którego stała była
512 (kafel aktywacji przestaje mieścić się w cache), tylko przy dwa razy
większym progu.

**Rozpakowanie nie potrzebuje aktywacji.** Wagi są statyczne, więc rozpakowanie
wycinka może się dziać ZANIM zaczekamy na `x` — w oknie, w którym CPU i tak stoi
i patrzy, jak GPU liczy. Wcześniej działo się po tym czekaniu, czyli na ścieżce
krytycznej. Profil przed i po, prefill 1024 tokenów:

| | przed | po |
|---|---:|---:|
| libBNNS — CPU mnoży | 70,6% | **77,9%** |
| jądro — czekanie na GPU | 19,3% | 17,6% |
| nasz kod (rozpakowanie + kodowanie) | 9,5% | **4,4%** |

Udział produktywnego mnożenia wzrósł o 7 punktów, a nasz własny narzut spadł
ponad dwukrotnie.

### Wynik trzeciej rundy

Pomiar przeplatany z MLX, w jednym stanie termicznym:

| prompt | nasze | MLX | przewaga |
|---|---:|---:|---:|
| 1024 | 270,8 / 243,1 tok/s | 202,6 / 216,9 | **+22%** |
| 2048 | 254,7 / 246,3 | 207,3 / 212,4 | **+19%** |

**Ostrzeżenie metodyczne.** Po kilku godzinach obciążenia maszyna zwolniła o
około 20%: prompt 256 na NIEZMIENIONYM kodzie dawał 257 tok/s rano i 199-216
wieczorem, a MLX spadł z 218,9 na 202-217. Liczby bezwzględne z różnych godzin
tej sesji NIE są porównywalne; porównywalne są wyłącznie pomiary przeplatane, i
tylko takie są tu podstawą wniosków. Jedna próbka potrafiła odbiec o 17% (236,0
przy trzech kolejnych 261,8 / 260,6 / 262,6), więc pojedynczy przebieg nie
rozstrzyga niczego poniżej kilkunastu procent.

### Sprawdzone i odrzucone w tej rundzie

**Większy udział CPU przy kaflu 1024.** 0,67 i 0,70 wyszły nierozróżnialne
(268,3 / 266,3 wobec 267,3 / 264,5), 0,64 wyraźnie gorsze. Rampa kończy się
więc na 512 i powyżej trzyma 0,70 — nie ma tam czego dopasowywać.

**NEON do rozpakowania.** Prototyp `vqtbl1q_u8` z dwiema tablicami bajtowymi
jest 2,66x szybszy od skalarnego i zgodny co do bitu (0 różnic na 13 mln
elementów). NIE wdrożony: po przełożeniu kolejności rozpakowanie zeszło do 4,4%
czasu wątku i jest w większości schowane za czekaniem, więc zysk byłby poniżej
progu, przy którym da się go w ogóle zmierzyć na tej maszynie (rozrzut ~5%).
Prototyp zostaje opisany tutaj, żeby nie trzeba go było wymyślać drugi raz.

## Nierozstrzygnięte

Uwaga nadal nie jest dzielona. Rachunek mówi, że przy 1024 tokenach to około
2,4% całej pracy, a przy 2048 około 4,8% — czyli mniej, niż wcześniej
sugerowałem, i podział wymagałby zupełnie innego mechanizmu niż podział wierszy
GEMM.

Wsad 128 i mniejsze nie korzystają z CPU w ogóle, bo rozpakowanie się nie
amortyzuje. Trwała kopia f16 wycinka CPU zdjęłaby ten koszt i otworzyła krótsze
kafle, ale kosztuje 4,5 GB i została świadomie odrzucona.

## Źródła

- FusionML: Prefill, Not Decode — https://arxiv.org/html/2607.22785v1
- Disaggregated Inference on Apple Silicon (SqueezeBits) — https://blog.squeezebits.com/disaggregated-inference-on-apple-silicon-npu-prefill-and-gpu-decode-67176
- hybrid-ane-mlx-bench — https://github.com/AtomGradient/hybrid-ane-mlx-bench
