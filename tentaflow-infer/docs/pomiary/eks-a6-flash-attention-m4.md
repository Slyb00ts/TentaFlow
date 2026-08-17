# EKS-A6 — uwaga blokowa na jednostkach macierzowych (Apple M4)

Punkt wyjścia: uwaga zajmowała 52 ms z 1226 ms prefillu przy 256 tokenach i
431 ms z 5067 ms przy 1024. Liczyła 0,41 TFLOPS, bo każdy wątek szedł całym
wierszem klucza skalarnie. Przy 1024 tokenach to było jedyne miejsce, w którym
przegrywaliśmy z MLX — bez uwagi wychodziło 220,9 tok/s wobec 213,9 MLX.

## Co wzięliśmy z FlashAttention-4, a czego nie

FA4 jest zbudowany wokół **asymetrii Blackwella**: rdzenie tensorowe
przyspieszyły o rząd wielkości, a jednostka wykładnicza nie, więc softmax
przestał być „tym, co jest między dwoma mnożeniami", a stał się wąskim gardłem
wymagającym potokowania.

**Na M4 ta asymetria nie istnieje.** EKS-A2 zmierzył `simdgroup_matrix` na
3,94 TFLOPS wobec 3,07 dla zwykłego FMA — przewaga 1,28x, nie rząd wielkości.
Przepisanie harmonogramu FA4 jeden do jednego optymalizowałoby więc coś, czego
tu nie ma.

| technika FA4 | wzięta? | dlaczego |
|---|---|---|
| kafelkowanie + maksimum przyrostowe | tak | podstawa, przenośna |
| **warunkowe przeskalowanie (próg τ)** | tak | tu warta WIĘCEJ niż na Blackwellu |
| `exp2` z wpisanym `log2(e)` | tak | darmowe |
| wielomian zamiast jednostki wykładniczej | **nie** | premisa nie zachodzi (1,28x) |
| ping-pong dwóch kafli Q, warpy wyspecjalizowane | nie | brak prymitywów; grupy SIMD to nie to samo |
| TMEM, `tcgen05`, tryb 2-CTA, DSMEM | **nie** | nie istnieją |

**Warunkowe przeskalowanie jest tu ważniejsze niż w oryginale.** Fragment
`simdgroup_matrix` w Metalu jest NIEPRZEZROCZYSTY — nie da się go pomnożyć
przez skalar. Każde przeskalowanie akumulatora oznacza przepuszczenie go przez
pamięć grupy roboczej: zapis, mnożenie, odczyt, dwanaście barier. Na Blackwellu
akumulator siedzi w pamięci tensorowej i przeskalowanie jest tanie; tutaj próg,
który je opóźnia, decyduje o tym, czy jednoprzebiegowa wersja w ogóle się opłaca.

## Droga i pomiary

Najpierw wersja DWUPRZEBIEGOWA: najpierw maksimum i suma po wszystkich kluczach,
potem wynik przy ustalonym maksimum. Kosztuje Q·Kᵀ dwa razy, ale nie wymaga
przeskalowywania w ogóle. Prosta i od razu poprawna.

| zmiana | 256 tok. | 1024 tok. |
|---|---:|---:|
| uwaga per-token (punkt wyjścia) | 209,4 tok/s | 195,0 tok/s |
| blokowa, dwa przebiegi | 214,2 | 199,4 |
| + softmax rozdzielony na 128 wątków | 213,5 | **206,2** |
| + jeden przebieg, τ=8 | 215,6 | 197,8 |
| + jeden przebieg, τ=14 | **215,6** | **213,2** |

Dwie rzeczy warte odnotowania:

**Softmax na jednym wątku na wiersz to była jedna grupa SIMD z czterech.**
Pozostałe trzy stały, a łańcuch `exp2` był czterokrotnie dłuższy niż musiał.
Rozdzielenie na cztery wątki na wiersz dało +11 tok/s przy 1024.

**Jeden przebieg z τ=8 był GORSZY od dwóch przebiegów** (197,8 wobec 206,2).
Próg okazał się za niski: przy długim kontekście maksimum rośnie na tyle często,
że przeskalowania — a każde to pełny obieg akumulatora przez pamięć — kosztowały
więcej niż oszczędzony drugi przebieg Q·Kᵀ. Dopiero τ=14 (współczynnik 16384,
wciąż mieszczący się w half dla prawdopodobieństw) odwrócił bilans. To jest
dokładnie ta liczba, której FA4 nie podaje, bo u nich zależy od innego sprzętu.

**Odrzucone:** trzymanie fragmentów Q w rejestrach przez całą pętlę (16 dodatkowych
fragmentów na linię wypycha zajętość: 195,0 wobec 199,4 przy 1024) oraz
wektoryzacja odczytu kluczy w starej formie per-token (bez zmiany).

## Wynik wobec MLX

Pomiar PRZEPLATANY — na maszynie po długiej sesji obie ścieżki zwalniają
termicznie (MLX przy 1024 spadł z 213,9 na 200,9 w ciągu godziny), więc dwa
osobne przebiegi porównują temperaturę, a nie kod. Mediana z trzech rund,
naprzemiennie:

| prompt | MLX | nasz | stosunek |
|---|---:|---:|---:|
| 256 | 218,5 tok/s | 215,6 tok/s | **98,7%** |
| 1024 | 200,9 tok/s | 199,4 tok/s | **99,3%** |

Czyli parytet w granicach kilku procent, a nie wyprzedzenie. Uczciwie: nie
przebiliśmy MLX — dogoniliśmy go.

## Zgodność

Forma blokowa nie ma własnej wyroczni: jest przypięta do formy per-token, która
jest przypięta do MLX. Największa różnica na token wychodzi 3,3e-4, czyli na
poziomie zaokrąglenia wyjścia do half. Test zawiera też kontrolę samego
porównania — wyniki muszą ZALEŻEĆ od pozycji, bo inaczej maska przyczynowa nie
działa, a obie formy myliłyby się tak samo.

Pułapka złapana po drodze: bufor wyników Q·Kᵀ jest po pętli używany ponownie do
zapisu wyjścia, a to drugie potrzebuje ośmiu zapytań po `dim` kanałów. Przy
bloku kluczy 32 obie potrzeby wychodzą równe (1024 floaty) i wszystko działało;
przy bloku 16 zapis wychodził poza tablicę. Rozmiar jest teraz maksimum z obu.
