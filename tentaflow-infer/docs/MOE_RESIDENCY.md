# Rezydencja ekspertów MoE: VRAM / RAM / NVMe

Model Mixture-of-Experts czyta na token tylko `top_k` z `n_experts` bloków każdej
warstwy, ale wszystkie muszą być osiągalne. To otwiera możliwość, której model
gęsty nie ma: trzymać w najszybszej pamięci wyłącznie tych ekspertów, którzy są
naprawdę używani, a resztę odsunąć dalej.

Dokument opisuje, jak to jest zrobione, i podaje zmierzone koszty. Sprzęt:
Radeon RX 6900 XT (gfx1030, 16 GiB, PCIe 4.0 x16), model odniesienia
`allenai/OLMoE-1B-7B-0924-Instruct` Q4_K_M (4,2 GB, 16 warstw, 64 ekspertów na
warstwę, top-8).

## Trzy warstwy, dwa mechanizmy

| Warstwa | Adresowalna przez kernel | Jak ekspert się tam dostaje |
|---|---|---|
| VRAM | tak | migracja według popularności |
| przypięty RAM hosta | tak (UVA, PCIe) | migracja według popularności |
| NVMe | **nie** | stronicowanie na żądanie |

Rozróżnienie jest istotne, bo z niego wynika cała reszta konstrukcji.

**VRAM ↔ RAM.** Obie warstwy kernel czyta wprost, więc przeniesienie eksperta
niczego nie umożliwia — tylko przyspiesza. Może się więc odbywać rzadko, w tle,
i być dowolnie zachowawcze. Runda przeglądu zapada co 128 tokenów dekodowania i
przenosi najwyżej 8 ekspertów w całym modelu.

**RAM ↔ NVMe.** Dysku żaden kernel nie zaadresuje, więc trafienie w eksperta
leżącego na dysku MUSI go najpierw ściągnąć — a żeby wiedzieć, w kogo trafiono,
trzeba odczytać wybór routera na hoście. Warstwa z choćby jednym ekspertem na
dysku traci przez to ścieżkę dekodowania bez odczytu wstecznego. To jest cena za
to, że model w ogóle się mieści, i nie da się jej uniknąć.

## Tablica wskaźników zamiast sklejonego stosu

Wcześniej eksperci jednej projekcji byli jednym tensorem `[n_experts*rows, cols]`,
a kernele `_gidx` liczyły offset jako `ids[sel] * rows_per_expert`. Taki układ
wyklucza rezydencję warstwową: nie da się przenieść jednego eksperta, nie ruszając
sąsiadów.

Stos jest więc rozbity na osobne bloki, a kernel dostaje **tablicę wskaźników**
indeksowaną numerem eksperta:

```
w = wtab[ids[sel]]        # zamiast: w = w_base + ids[sel]*rows_per_expert*stride
```

Wybór nadal zapada na urządzeniu, więc ścieżka bez stronicowania zachowuje zero
odczytów wstecznych. Odczyt tablicy jest jednolity dla całego bloku, czyli jeden
zależny dostęp na blok. Dodatkowo blok z VRAM i blok z pamięci hosta wyglądają dla
kernela identycznie — mieszanie warstw nie wymaga ani jednej gałęzi w kodzie GPU.

Zgodność bitowa obu wariantów (i tego z pamięci hosta) jest sprawdzana w
`crates/forge-kernels/tests/moe_expert_table.rs` przeciwko dotychczasowym kernelom
okna wierszy.

## Sloty, nie realokacje

Pula wag jest areną bump — nie zwalnia pojedynczych bloków, więc migracja przez
realokację jest niemożliwa. Zamiast tego rezydencja operuje na **stałym inwentarzu
slotów**: uchwyty buforów powstają raz przy ładowaniu i nigdy nie zmieniają
adresu, a migracja przenosi wyłącznie bajty i przepisuje wpis tablicy.

Wszyscy eksperci jednej projekcji mają ten sam rozmiar, więc zamiana zawartości
dwóch slotów jest zawsze legalna.

Eksmisja na rzecz eksperta z dysku jest darmowa: wagi są tylko do odczytu, więc
kopia na dysku pozostaje aktualna. Ofiara, która nigdy na dysku nie była, jest
tam dopisywana raz — przy pierwszym wyparciu.

## Podział jest proporcjonalny, nie „kto pierwszy"

Pierwsza wersja rozdzielała pamięć w kolejności ładowania i to był błąd
projektowy, nie strojenie: pierwsze warstwy wychodziły w całości rezydentne,
ostatnie w całości na dysku, więc **każdy token trafiał gwarantowanym chybieniem
w każdą z ostatnich warstw**. Przy budżecie 61% rezydencji ostatnie stosy
dostawały zero slotów i model po prostu się nie ładował.

Teraz udział liczy się raz, przed alokacją, i jest ten sam dla każdego stosu:

```
vram_dla_ekspertow = VRAM − (wagi warstw, których zrzucić się nie da) − zapas
udzial_rezydentny  = (vram_dla_ekspertow + budżet_hosta) / bajty_ekspertów
```

Każdy stos zachowuje przy tym minimum `2 * top_k` slotów rezydentnych. Bez tego
warstwa nie miałaby dokąd ściągnąć wybranych ekspertów, a przy dokładnie `top_k`
kolejny token wypierałby to, co właśnie zostało wczytane.

Wagi inne niż eksperci są odejmowane z góry, bo nie mają dokąd się wynieść — to
one, a nie eksperci, wywalały ładowanie, gdy eksperci zabrali całą pamięć.

## Popularność

Kernel routera zlicza wybory ekspertów atomowo w rezydentnym liczniku — to jedyne
miejsce w systemie, które wie, kto jest gorący, a koszt to `top_k` atomowych
dodań na warstwę na token. Licznik jest odczytywany i zerowany raz na rundę.

Popularność jest wykładnicza (EMA, współczynnik 0,75), bo rozkład trafień dryfuje
razem z treścią rozmowy — suma od startu zamroziłaby układ na pierwszym temacie.

Zamiana wymaga przewagi 1,25×. Bez tego progu dwaj sąsiedzi w rankingu
przerzucaliby się w kółko. Limit 8 zamian na rundę jest **globalny**: limit per
projekcja przepuściłby przy 61 warstwach setki przeniesień naraz.

## Stronicowanie z dysku

Chybienia całej warstwy — `top_k` ekspertów razy trzy projekcje — są znane naraz,
zaraz po odczycie wyboru routera. Idą więc jednym zgłoszeniem przez 8 wątków
`pread`, prosto do przypiętej pamięci slotu (bez bufora pośredniego). NVMe oddaje
pełną przepustowość dopiero przy głębokiej kolejce; po kolei płaciłoby się sumę
opóźnień zamiast najdłuższego z nich.

Cache stron systemu jest włączony celowo — wolny RAM ponad budżetem przypiętym
staje się dzięki temu darmową czwartą warstwą, a `O_DIRECT` by go odciął.

Prefill ściąga sumę ekspertów całego kawałka jednym zgłoszeniem, o ile suma mieści
się w slotach; inaczej wraca do stronicowania per token (i wtedy musi drenować
strumień przed każdym, bo nadpisuje pamięć czytaną przez kernele w locie).

## Pomiar

OLMoE-1B-7B Q4_K_M, prompt 15 tokenów, 200 tokenów wyjścia, greedy, RX 6900 XT.

| Układ | VRAM / RAM / NVMe (eksperci) | Czas | Przepustowość |
|---|---|---|---|
| całość w VRAM | 3072 / 0 / 0 | 2,26 s | 95,3 tok/s |
| VRAM + RAM | 1419 / 1653 / 0 | 5,73 s | 37,5 tok/s |
| VRAM + RAM + NVMe | 864 / 1008 / 1200 | 7,94 s | 27,1 tok/s |

**Wyjście wszystkich trzech układów jest identyczne** co do znaku, przy aktywnych
rundach migracji (3 rundy w przebiegu 200-tokenowym). To jest właściwy test
poprawności: rezydencja nie ma prawa zmienić ani jednego tokena.

Interpretacja: 39% modelu na dysku kosztuje 3,5× względem pełnego VRAM i 1,4×
względem układu bez dysku. To jest cena za uruchomienie modelu, który się nie
mieści — nie za przyspieszenie takiego, który się mieści. Rezydencja nie włącza
się sama: bez `--weight-host-gb` i `--weight-spill-dir` brak pamięci pozostaje
błędem ładowania.

## Czego tu nie ma

- **Eksperty NVFP4 compressed-tensors.** Rezydencja wymaga wagi o jednym buforze
  bajtów; format trzymający osobno pakiety i skale jest odrzucany przy ładowaniu.
  To blokuje DeepSeek V4 Flash, niezależnie od wsparcia dla samej architektury.
- **Awans prosto z dysku do VRAM.** Ekspert z dysku wchodzi do pamięci hosta i
  dopiero stamtąd może awansować w kolejnej rundzie.
- **Wyprzedzające ściąganie.** Wybór routera warstwy `L` zależy od wyjścia
  warstwy `L-1`, więc nie ma czego przewidywać bez osobnego modelu.
- **Pomiar na modelu, który naprawdę wymaga dysku.** OLMoE mieści się w VRAM w
  całości; warstwy są wymuszane budżetem, właśnie po to, żeby istniał punkt
  odniesienia do porównania tekstu. Model bez takiego punktu odniesienia można
  zmierzyć, ale nie da się na nim udowodnić poprawności.
