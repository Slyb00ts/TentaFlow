# Bielik-7B-NVFP4 na DGX Spark — profil prefillu i granica obecnej ścieżki

Pomiary na GB10 (sm_121a, 48 SM, pamięć zunifikowana 121 GiB),
`forge bench --weights-pool-gb 24`, model `TentaFlow/Bielik-PL-Minitron-7B-NVFP4`.

## Stan

| prompt | prefill tok/s | decode tok/s | zmierzone |
|---|---|---|---|
| 2 048 | **4 938** | 38,0–38,1 | 2026-08-03, mediana z 3 przebiegow |
| 512 | 3 930 | 40,2 | starszy pomiar |
| 4 096 | 3 902 | 35,4 | starszy pomiar |

Wobec startu sesji (2 957 tok/s przy 2048) to **+67%**, przy niezmienionej
generacji. Wiersze 512 i 4096 pochodza sprzed kilku zmian i nie zostaly
powtorzone — nie sa punktem odniesienia dla dzisiejszych bramek.

Odniesienie na tym samym sprzęcie i modelu: **vLLM 0.26 daje 48 tok/s decode**
oraz ~10 000 tok/s prefillu na zimno (mierzone przez HTTP, więc lekko zaniżone
dla vLLM).

Prefill 3 192 tok/s przy 2048 tokenach to 2·7e9·2048 / 0,64 s = **44,7 TFLOPS**.

## Protokol pomiaru — przeczytac przed dopisaniem jakiejkolwiek liczby

GB10 bezczynny stoi na **208 MHz** przy 3003 MHz maksymalnych i pod obciazeniem
wchodzi na ~2561 MHz (54 W, 55 C, wiec ani prad ani temperatura nie ograniczaja).
Rozgrzewka liczona w kilku wywolaniach tego nie podnosi. Skutek jest brutalny:
ten sam ksztalt `(N=4096, K=11264)` zmierzony jako PIERWSZY przypadek w
benchmarku daje **876 us**, a po sekundzie mielenia **617 us**. To 1,42x
roznicy zaleznej wylacznie od kolejnosci przypadkow.

Dwie liczby w poprzedniej wersji tego dokumentu byly wlasnie takim artefaktem
i wyslaly analize w slepa uliczke (patrz "Co bylo zmierzone zle"). Kazdy
benchmark w `bench_fp8_traffic.mojo` wola wiec `_warmup` (4000 pelnych GEMM-ow)
przed pierwszym pomiarem i mierzy kazdy przypadek dwukrotnie.

## Roofline GB10 — dwie liczby, ktore rzadza wszystkim

`bench_fp8_ceiling.mojo` (mma bez ruchu pamieci, rosnacy ILP) i probe
strumieniowy:

| wielkosc | wartosc |
|---|---|
| sufit mma e4m3 `m16n8k32` | **251 TFLOPS** (wysycony juz przy ILP=1) |
| odczyt strumieniowy z pamieci | **~224 GB/s** |
| L2 | 24 MiB |
| SM / zegar pod obciazeniem | 48 / 2561 MHz |

Punkt zagiecia lezy przy 1120 FLOP na bajt. Sufit 251 TFLOPS zgadza sie z
48 SM x 2048 FLOP/cykl x 2,45 GHz, wiec jest to prawdziwy sufit sprzetu, a nie
artefakt probki.

## Model: ile razy wagi przechodza przez pamiec

Kernel dostaje siatke `(N/BN, M/BM)` i CUDA rozdaje bloki x-major, wiec
rezydentne naraz sa WSZYSTKIE kafle kolumnowe jednego wiersza M. Liczba
wierszy M mielonych jednoczesnie to `conc = 48 / (N/BN)`, a stad liczba
przebiegow, ktore B musi odbyc przez pamiec:

```
passes = ceil( (M/BM) / conc )
TFLOPS = 2 * M * BW / passes          (o ile nie ogranicza plateau kernela)
```

Model zgadza sie z pomiarem na calym zakresie (M=1024, BM=128, BN=256,
BW=224 GB/s):

| ksztalt | passes | przewidziane | zmierzone |
|---|---|---|---|
| N=4096, K=9216..16384 | 3 | 152,9 | 150,2–152,9 |
| N=5632, K=4096 | 3,7 | 125,0 | 123,5 |
| N=8192, K=4096 | 5,3 | 86,1 | 88,6 |
| N=11264, K=4096 | 7,3 | 62,6 | 67,7 |
| N=14336, K=4096 | 8 | 57,3 | 60,2 |

Dwa wnioski, oba istotne:

1. **Plateau obliczeniowe tego kernela to ~150 TFLOPS, czyli 60% sufitu.**
   Widac je na `q/o` (B = 16 MiB miesci sie w L2, pamiec robi ledwie 120 GB/s,
   a mimo to wychodzi 146 TFLOPS). Tam nic nie ogranicza od strony ruchu — to
   jest czysta sprawnosc kernela.
2. **Dla `down` oba ograniczenia wypadaja w tym samym miejscu** (pasmo 152,9
   wobec plateau 150). Sama zmiana kolejnosci blokow NIC by tam nie dala.

Dlatego jedyna droga dalej to podniesienie plateau, czyli wlasny GEMM — a nie
swizzle, jak zakladala poprzednia wersja tego dokumentu.

## Ścieżka wykonania

`FORGE_GEMM` bez wartości włącza automatycznie hybrydowy prefill FP8:
projekcje Q/O/gate/up/down NVFP4 i `lm_head` są przepakowywane do FP8 na GPU
(7 GB paczek, 0,17 s). Zmierzony wpływ:

| wariant | prefill tok/s |
|---|---|
| auto (FP8) | 3 002 |
| `fp8mod-ffn` (to samo, wymuszone) | 2 983 |
| `nvfp4` (surowe, bez paczek) | 2 055 |

Decode jest identyczny we wszystkich wariantach (38,1–38,3), bo zostaje na NVFP4.

## Co bylo zmierzone zle

Dwie tezy z poprzedniej wersji nie przezyly powtorzenia pomiaru z rozgrzewka:

- **„`down` to najwolniejszy GEMM prefillu, 107 TFLOPS wobec 142 dla q/o".**
  Nieprawda. Na rozgrzanym ukladzie `(4096, 11264)` robi **150–153 TFLOPS**,
  czyli dokladnie tyle co reszta. Liczba 107 wziela sie z pomiaru na zimnych
  zegarach. Punkt 1 listy „nastepne cele" wskazywal wiec na nieistniejacy
  problem — dokladnie tak jak wczesniej robily to dwa komunikaty o `kv_packs`.
- **„`gate/up` i `down` maja identyczny ruch operandow, a roznia sie 2,3x, i
  tlumaczy to liczba kafli kolumnowych".** Ruch faktycznie jest identyczny, ale
  roznica nie jest zagadka: to model `passes` powyzej. Przy N=11264 rezydentny
  jest jeden wiersz M, wiec B idzie przez pamiec 8 razy zamiast 3.

Odwrotnie, teza o zalamaniu przy szerokim N **potwierdzila sie** w powtorzonym
pomiarze (N=4096 → 140 TFLOPS, N=11264 → 67,7, powtorka 66,2), wiec dzielenie
`gate/up` na kawalki zostaje: zmierzone 1396 us jednym wywolaniem wobec 655 us
w kawalkach, czyli **2,13x**.

Sprawdzone i ODRZUCONE: dzielenie `down` na plastry kolumn miesczace B w L2.
Plastry po 1024 kolumny daja 672 us, po 2048 — 697 us, a jedno wywolanie 617 us.
Waskie plastry wymuszaja pelna wspolbieznosc po M, ale zostawiaja 32 z 48 SM
bezczynnych i to kosztuje wiecej, niz daje oszczedzony ruch.

## Trzy GEMM-y prefillu nie są równe (pomiar historyczny, zimne zegary)

`bench_fp8_modular_tiles.mojo`, M=1024, najlepszy kafel:

| projekcja | kształt (N,K) | FLOP | czas | TFLOPS |
|---|---|---|---|---|
| q/o | 4096, 4096 | 34,4 G | 235,6 µs | 146 |
| down | 4096, 11264 | 94,5 G | 867,0 µs | 109 |
| gate/up | 11264, 4096 | 94,5 G | 2 025,7 µs | **46,6** |

`down` i `gate/up` mają **identyczną liczbę operacji i identyczny ruch operandów**
(oba czytają 44 MiB wag i 184 MB aktywacji po zsumowaniu kafli), a różnią się
2,3-krotnie. Jedyna różnica strukturalna to liczba kafli kolumnowych: 16 wobec 44.

### Co z tego wyciśnięto

Brakujący wariant BN=256 dla `down` (jedyny kształt, gdzie ten kafel daje 41%,
a nie ~4%) — dodany, efekt end-to-end **+6,3% przy 2048 i +7,8% przy 4096**.

## Granica: zajętość 16,67%

`ncu` na kernelu prefillu:

| metryka | q/o (16,8) | gate/up (44,8) |
|---|---|---|
| przepustowość SM | 43,5% | 21,7% |
| aktywne warpy | 16,3% | 16,0% |
| instr./cykl | 0,11 | 0,05 |
| fale na SM | 2,67 | 7,33 |

Ograniczenia zajętości — **oba** dopuszczają jeden blok na SM:

```
rejestry/wątek:            224   -> limit 1 blok
pamięć współdzielona/blok: 98,3 KB -> limit 1 blok
maks. aktywnych warpów:    16,67%
```

224 rejestry biorą się głównie z akumulatorów: kafel warpa 64×64 to 4096 wartości
F32 na warp, czyli 128 rejestrów na wątek zanim policzymy cokolwiek innego.
98,3 KB to 4 etapy × (128+256) × 64 B.

## Zmierzone ślepe uliczki

Każda sprawdzona pomiarem, nie odrzucona z rozumowania:

- **Wektorowy epilog** zamiast pętli po elementach: 2 025,2 vs 2 025,7 µs, czyli
  zero. Kompilator Mojo i tak rozwijał `comptime for`. Hipoteza brała się stąd,
  że `gate/up` zapisuje 11,5 mln wyjść wobec 4,2 mln w `down` — okazała się
  nietrafiona.
- **BM=256**: przekracza pamięć współdzieloną (256+256)×64×4 = 128 KB.
- **Kafel warpa inny niż 64×64** (64×32, 32×64): kernel Modulara odrzuca
  konfigurację w czasie wykonania.
- **Własne kernele `gemm_fp8_wmma_*`**: budowane wyłącznie dla AMD, nie ma ich
  w zestawie sm_121a.

## Rozklad czasu prefillu (nsys, prompt 2048)

Po odjeciu jednorazowego pakowania wag (212 ms: `nvfp4_ct_fp8_pack` 169,6 ms +
`nvfp4_ct_layout_repack` 42,8 ms — oba przy ladowaniu, nie w przebiegu) zostaje
okolo 895 ms realnej pracy:

| skladnik | ms | udzial |
|---|---|---|
| GEMM-y FP8 (4 warianty) | 511 | 57% |
| uwaga `attn_prefill_fa_mma` | 156 | 17% |
| GEMM NVFP4 — projekcje K/V | 83 | 9% |
| `silu_mul` | 46 | 5% |
| normalizacje | 41 | 5% |
| kwantyzacja aktywacji | 37 | 4% |

## Rozklad po zmianach (nsys, 2026-08-03, prompt 2048)

Zmierzone na biezacej wersji, bez jednorazowego pakowania wag:

| skladnik | udzial |
|---|---|
| GEMM-y FP8 (5 wariantow) | **68,6%** |
| uwaga `attn_prefill_fa` | 18,6% |
| `silu_mul_quant` | 7,2% |
| normalizacje | 5,7% |

**`nvfp4_ct_prefill_gemm` nie wystepuje w prefillu w ogole.** K/V sa dzis w
paczkach FP8; jedyne NVFP4 w profilu to jednorazowy `nvfp4_ct_fp8_pack` przy
ladowaniu. Punkt „K/V wciaz na NVFP4 (9%)" byl NIEAKTUALNY, a utrzymywaly go
przy zyciu dwa komunikaty: tekst CLI mowiacy „K/V i warstwy decode pozostaja
NVFP4" oraz pole `kv_packs = 0` wpisane na sztywno w log. Oba poprawione — to
one wysylaly kolejna osobe za nieistniejacym problemem.

Nastepne cele w kolejnosci zysku do ryzyka:

1. **Plateau kernela GEMM: 150 z 251 TFLOPS.** Nie chodzi o zaden pojedynczy
   ksztalt — wszystkie leza na tej samej wartosci. Ogranicza je 8 warpow na SM
   (224 rejestry + 98,3 KB pamieci wspoldzielonej na blok), a kafla warpa
   innego niz 64x64 kernel Modulara nie przyjmuje. Stad wlasny GEMM: 16 warpow
   z akumulatorem 32x64 zamiast 8 warpow z 64x64, przy tej samej pamieci
   wspoldzielonej.
2. **Uwaga** (18,6%) — sufit rejestrow juz zalozony (patrz nizej); dalsze
   zejscie wymagaloby mniejszego akumulatora, a nie ustawienia `ptxas`.

### Sufit rejestrow uwagi — zrobione

`ptxas` dawal `attn_prefill_fa_mojo_f16_hd128` **176 rejestrow**, choc te sama
prace wykonuje w **148 bez ani jednego bajtu spillu**. Przy 128 watkach na blok
176 rejestrow miesci dwa bloki na SM, a 148 — trzy, czyli sufit wynikajacy i tak
z 32 KiB pamieci wspoldzielonej. Polowa zajetosci szla na nic.

Dyrektywe `.maxnreg 152` wstrzykuje builder katalogu z tabeli niosacej pomiar,
wiec przezywa przebudowe. Mediana z trzech przebiegow: prefill **4 856 -> 4 938
tok/s**. HD256 celowo poza tabela: sam akumulator zajmuje tam 128 rejestrow,
wiec kazdy sufit ponizej 252 spilluje (672 B juz przy 200).

Wczesniejszy pomiar `ncu` (ponizej) mowil o zejsciu do 128 rejestrow i czterech
blokach — to bylo za daleko. Pamiec wspoldzielona ogranicza do trzech niezaleznie
od rejestrow, a ponizej 148 zaczynaja sie spille.

`ncu` na `attn_prefill_fa_mma` (pomiar historyczny):

   | metryka | wartosc |
   |---|---|
   | jednostka specjalna (`exp`) | **2,78%** |
   | FMA | 2,81% |
   | jednostka tensorowa | 13,5% |
   | przepustowosc SM | 12,9% |
   | aktywne warpy | **15,2%** |
   | rejestry/watek | **176** -> limit 2 bloki/SM |
   | pamiec wspoldzielona | 0 -> limit 3 bloki/SM |

   Zadna jednostka nie jest wysycona; kernel stoi na opoznieniach przy zbyt
   malej liczbie warpow. Ogranicza go WYLACZNIE zuzycie rejestrow: 128 watkow x
   176 rejestrow to 2 bloki na SM, czyli 8 warpow z 48. Zejscie do ~128
   rejestrow dalo by 4 bloki i podwojenie zajetosci.

   **Wniosek dla FA4:** wielomianowa wykladnicza (technika #2) NIE pomoze tutaj —
   jednostka specjalna jest wysycona w 2,78%, wiec nie jest waskim gardlem.
   Zmierzone przed implementacja, ktora byla by praca bez efektu.

## Wniosek

W obrębie `multistage_gemm_kernel` Modulara pokrętła są wyczerpane. Zajętość
16,67% wynika z rozmiaru rejestrów i pamięci współdzielonej tego kernela, a nie
z naszego doboru kafla.

### Wlasny GEMM — ZBUDOWANY I ODRZUCONY

Napisany w calosci (cp.async, 3 etapy, fragmenty ldmatrix, fuzowany epilog ze
skalami, wycinki kolumn LDY) i **przegrywa z kernelem Modulara na kazdym
ksztalcie i kazdej konfiguracji**, przy wyjsciu bit w bit takim samym:

| ksztalt | kafel warpa | warpy | nasz | Modular |
|---|---|--:|--:|--:|
| q/o (4096,4096) | 64x64 | 8 | 306,0 us | **258,0 us** |
| q/o (4096,4096) | 32x64 | 16 | 327,5 us | **244,7 us** |
| down (4096,11264) | 64x64 | 8 | 997,0 us | **865,6 us** |
| down (4096,11264) | 32x64 | 16 | 1026,8 us | **878,6 us** |

Hipoteza „wiecej warpow" jest **falszywa i wiadomo dlaczego**. Na krok k blok
wykonuje `BM*BN/(16*WN)` odczytow ldmatrix dla A i `BM*BN/(8*WM)` dla B, czyli
ruch A skaluje sie jak 1/WN, a ruch B jak 1/WM. Zejscie WM z 64 na 32 nie
zmienia A i **podwaja** odczyty B z pamieci wspoldzielonej. Kernel jest zwiazany
pasmem pamieci wspoldzielonej, zanim zwiaze go zajetosc — i dokladnie dlatego
Modular nie przyjmuje kafla innego niz 64x64. Rejestry nigdy nie byly tu
ograniczeniem. Wariant 32x32 (32 warpy) w ogole sie nie uruchamia:
1024 watki x 64 rejestry akumulatora to caly plik rejestrow.

Przy IDENTYCZNYM kaflu 64x64 nasz kernel traci 15-19%, czyli roznica lezy w
jakosci implementacji (swizzle pamieci wspoldzielonej zamiast naszego
dopelnienia, parowane ldmatrix.x4 dla B, szeregowanie potoku) — ta sama
przewaga `ptxas`, ktora `CODEGEN_PROOF.md` opisuje dla int8. Oba pliki zostaly
cofniete, sciezka Modulara zostaje.

### Co zostaje

1. **Podniesienie plateau wymagaloby CUDA, nie Mojo.** Precedens jest jeden:
   `kernels/cuda/gemm_i8mma.cu`, jedyny wyjatek od ADR-0001, wziety dokladnie z
   tego powodu. To jedyna niesprawdzona droga do 251 TFLOPS.
2. **Kolejnosc blokow w siatce** — ZDEGRADOWANA z pozycji pierwszej. Model
   `passes` pokazuje, ze dla `down` swizzle nic nie da, bo pasmo i plateau
   wypadaja w tym samym punkcie (152,9 wobec 150). Ma sens dopiero PO
   podniesieniu plateau, i wtedy zastapi dzielenie `gate/up` na kawalki.
3. Nowszy kernel z biblioteki Modulara, jeśli mają wariant dla Blackwella;
   pakiet jest skompilowany (`linalg.mojoc`, bez zrodel), więc bez nich nie da
   się tego sprawdzić z repo.


## Dlaczego sciezka NVFP4 wprost jest wolniejsza od konwersji do FP8

Zmierzone na Bieliku 7B, prompt 2048:

| sciezka | prefill | decode | dodatkowa pamiec |
|---|---|---|---|
| paczki FP8 | 4 899 tok/s | 38,4 | +7,35 GB |
| NVFP4 wprost | 2 064 tok/s | 38,2 | 0 |

`nsys` na sciezce wprost: JEDEN kernel `nvfp4_ct_prefill_gemm` to **85,3% czasu**
(1722 ms / 1120 wywolan, po 1,54 ms). Ta sama praca w FP8 zajmuje okolo 540 ms.

`ncu` na tym kernelu — wbrew oczekiwaniu NIE jest ograniczony zajetoscia ani
pamiecia:

| metryka | NVFP4 wprost | FP8 modular |
|---|---|---|
| przepustowosc SM | **56,4%** | 43,5% |
| rejestry/watek | **80** (limit 6 blokow) | 224 (limit 1 blok) |
| pamiec wspoldzielona | 0 dynamicznej | 98,3 KB |
| jednostka tensorowa | **25,1%** | — |

Rozjazd 56,4% SM wobec 25,1% jednostki tensorowej mowi wszystko: **polowa pracy
SM to nie mnozenie macierzy, tylko rozpakowywanie FP4**. Kernel jest zajety, ale
nie liczeniem.

Samo rozpakowanie nie jest naiwne — uzywa sztuczek bitowych na parach wartosci
(maska znaku, przesuniecie wykladnika). Problem jest ilosciowy: zbyt wiele
operacji pomocniczych na jedna tensorowa.

Zrodlo roznicy jest jednak glebsze niz liczba instrukcji pomocniczych. Petla
rozpakowujaca konczy sie tak:

```mojo
wv = (_e2m1x8(codes) * sc[wp]).cast[DType.float16]()
```

czyli sciezka wprost liczy na MMA **f16 `k16`**, a przepakowana na **e4m3
`k32`** — dwa razy mniej K na instrukcje. Skali per-grupa nie da sie nalozyc
wewnatrz zwyklego MMA FP8, wiec zejscie do f16 nie jest niedbaloscia, tylko
konsekwencja formatu.

Kierunek naprawy w obrebie tej sciezki, w kolejnosci:
1. **Rozpakowywac wprost do ukladu operandu MMA**, zeby nie przestawiac wartosci
   po dekodowaniu.
2. **Skale nakladac w epilogu**, a nie na kazda wartosc.
3. Zredukowac liczbe instrukcji na wartosc (LOP3 zamiast osobnych and/or/shift).

Zadne z tych trzech nie zmieni jednak `k16` na `k32`, wiec pelnego dystansu do
paczek FP8 nie zamkna.

**Korekta.** Zapisalem wczesniej, ze MXFP4 odpada, bo bylby "DRUGA konwersja, w
dodatku stratna". Obie czesci sa nieprawdziwe. Przepakowanie do FP8, ktore
robimy dzis, tez jest konwersja i tez jest stratne: iloczyn wartosci 4-bitowej i
skali E4M3 trzeba zaokraglic do e4m3. Roznica jest taka, ze nasza kosztuje
7,35 GB i daje `k32`, a MXFP4 nie kosztuje pamieci i daje `k64` (zmierzone
1,999x wzgledem FP8). Rozstrzygac ma pomiar jakosci `forge ppl` dla obu
konwersji, nie argument o czystosci formatu.
