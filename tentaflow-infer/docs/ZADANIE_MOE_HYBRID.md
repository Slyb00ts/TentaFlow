# Zadanie: MoE i hybryda w słownictwie — karta, nie kod

Krok 4 z §8 `ARCHITEKTURA_DOCELOWA.md`. Ten sam dokument nazywa go **jedyną
pozycją, która jest prawdziwą pracą projektową**, i zabrania pisać go na ślepo:
błąd w tej ścieżce nie objawia się awarią, tylko płynnym, złym tekstem. Dlatego
najpierw karta, potem wzorzec, potem wykonane porównanie.

Kroki 1–3 są zamknięte i zmierzone, więc to jest następna rzecz w kolejce:

| krok §8 | stan |
|---|---|
| 1. wspólna warstwa stanu | KV ✅, admission i continuous batching zostają |
| 2. fuzja jako pass | ✅ zmierzone: +0,8% przy dekodowaniu |
| 3. formaty wag | ✅ 22 kwantyzacje, bramka per format |
| **4. MoE i hybryda w słownictwie** | **ten dokument** |
| 5. spekulacja jako pass | czeka na 4 |
| 6. serwer przechodzi, silnik chudnie | czeka na 5 |

## 1. Dlaczego to jest praca projektowa, a nie przeniesienie

Dziś obie rodziny istnieją WYŁĄCZNIE po stronie CUDA, w `forge-engine`:

| rodzina | plik | linie |
|---|---|---:|
| MoE | `model/arch/moe.rs` | 741 |
| hybryda (DeltaNet) | `model/arch/hybrid/{core,prefill,decode,verify}.rs` | 4 467 |

Dopóki tak jest, „wszystko na każdej architekturze" jest nieprawdą dla wszystkich
modeli poza gęstymi — a Qwen3.6, DeepSeek V4 i cała rodzina MoE to nie jest
przypadek brzegowy, tylko większość tego, co dziś się wdraża.

Przeniesienie nie wystarczy, bo obie rodziny wnoszą do słownictwa rzeczy, których
`Op` dziś nie umie wyrazić:

- **MoE**: które wagi mnożyć, jest DANĄ policzoną na urządzeniu (top-k routera),
  a nie stałą modelu. Dochodzi rezydencja ekspertów, bo stos ekspertów nie mieści
  się w VRAM.
- **Hybryda**: stan REKURENCYJNY przenoszony między krokami, z checkpointem i
  wycofaniem, bo spekulacja musi go umieć cofnąć.

## 2. Decyzja, którą trzeba podjąć NAJPIERW

**Ile z tego wchodzi do słownictwa.** Reszta karty zależy od tej odpowiedzi.

**(A) Drobno.** `Router`, `TopK`, `ExpertMatMul`, `ScaleAdd` jako osobne `Op`.
Pass widzi wnętrze FFN i może w nim fuzjować.

**(B) Jedna operacja na rodzinę.** `MoeFfn { layer, step }` i
`DeltaNet { layer, step }` — dokładnie tak, jak dziś wygląda `Attention { layer,
step }`. Routing, top-k, rezydencja ekspertów i stan rekurencyjny zostają po
stronie wykonawcy.

**Rekomendacja: (B)**, z czterech powodów, z których trzy są już w tym repo
zmierzone, a nie wymyślone:

1. **Precedens stronicowania.** §7.2 karty CUDA zadało dokładnie to pytanie dla
   KV i odpowiedź brzmiała: stronicowanie zmieściło się CAŁE po stronie
   wykonawcy, a słownictwo nie zyskało ANI JEDNEGO pola. Rezydencja ekspertów
   jest tym samym problemem — pamięć, która nie mieści całości — więc ma tę samą
   odpowiedź, dopóki ktoś nie pokaże, że nie ma.
2. **Wybrani eksperci są daną urządzenia.** Ścieżka `_gidx` w silniku istnieje po
   to, żeby identyfikatory top-k NIGDY nie wracały na hosta („zero host readback,
   no synchronize"). Model, który nazwałby te identyfikatory, musiałby je
   przeczytać — czyli słownictwo kasowałoby optymalizację, którą druga ścieżka
   zbudowała.
3. **Postać wagi należy do wykonawcy.** To już rozstrzygnięte (§7 karty CUDA):
   Metal chce trójki afinicznej, CUDA bloków źródła. Stos ekspertów jest tą samą
   sprawą w większej skali — jak leży `[n_experts * inter, hidden]` i co z tego
   jest rezydentne, wie ten, kto mnoży.
4. **Koszt (B) jest zmierzony i mały.** Jedyne, co (B) odbiera, to fuzja WEWNĄTRZ
   FFN. Zmierzone w kroku 2: fuzja przy dekodowaniu jest warta 0,8%, bo
   dekodowanie czyta całą macierz i tak. Oddajemy więc ułamek procenta za
   granicę, która trzyma trzech wykonawców.

Czego (B) NIE przesądza: `Step` może potrzebować pola na hybrydę, bo stan
rekurencyjny ma inny cykl życia niż strona KV. To jest pytanie do kroku 4b, nie
do 4a.

## 3. Kamień milowy 4a — MoE, bo routing jest arytmetyką

MoE idzie pierwsze i to nie jest kolejność alfabetyczna: **routing da się
odtworzyć na wzorcu dokładnie**, a stan rekurencyjny nie — jego błąd ujawnia się
o krok później i wymaga, żeby porównywalny był też sam stan. Jedna rodzina naraz.

Zakres: `Op::MoeFfn { layer, step }`, wykonawca hostowy, wykonawca CUDA, jedna
sekwencja. NIE: rezydencja (cały stos rezydentny), NIE spekulacja, NIE wsad.

Kernele istnieją w komplecie — `moe_router_f16`, `moe_gate_sqrtsoftplus_f16`,
`moe_scale_add_f16`, `moe_scale_add_gidx_f16`, `moe_sigmoid_f16_to_f32` — więc
praca to znowu WPIĘCIE, tak jak przy `CudaExec`. Jeśli okaże się inaczej, to jest
wynik wart zapisania, a nie powód, żeby dopisywać kernele w tym kroku.

### Czego wymaga, a czego nie mamy

**Małego checkpointu MoE.** To jedyna twarda przeszkoda i trzeba ją nazwać, a nie
obejść. Lokalnie jest `DeepSeek-V4-Flash` (156 GiB) — za duży, żeby wzorzec
hostowy policzył nim cokolwiek w rozsądnym czasie, a wzorzec jest tu całą
bramką. Potrzebny jest MoE rzędu kilku GiB (np. Qwen3-30B-A3B w niskim kwancie
albo cokolwiek mniejszego z tą samą strukturą routera).

Do czasu, aż taki checkpoint będzie, 4a da się napisać i zabramkować
HERMETYCZNIE, tak jak tabela formatów w `forge-kernels/tests/format_table.rs`:
router o znanych wagach, znany top-k, eksperci o znanych macierzach, wynik
policzony wzorcem w f32. To sprawdza routing, wybór i akumulację — czyli
wszystko, co w tym kroku jest nowe. Checkpoint dokłada tylko „wynik ma być
językiem", i to jest jedyna rzecz, na którą trzeba poczekać.

## 4. Kamień milowy 4b — hybryda, i gdzie mieszka jej stan

`DeltaNet` niesie stan między krokami, więc pierwsze pytanie nie brzmi „jaka
operacja", tylko **gdzie ten stan leży**.

Odpowiedź jest już wskazana precedensem: obok KV, w `forge-state`. Powód nie jest
estetyczny — spekulacja musi umieć ten stan zacheckpointować i wycofać (silnik
robi to dziś przez `deltanet_commit_checkpoint_segmented_f32` i
`deltanet_commit_recompute_segmented_shared_d128_f32`), a `forge-state` jest już
tym miejscem, gdzie mieszka stan sekwencji dzielony przez obie ścieżki. Stan
rekurencyjny w wykonawcy oznaczałby, że krok 5 (spekulacja jako pass) nie ma go
jak cofnąć bez zaglądania do wykonawcy.

Kernele DeltaNet są liczne i dojrzałe (ponad dwadzieścia wariantów w
`launchers/deltanet.rs`, z persistent scan i układem `ValueKey`), więc tu również
nie chodzi o pisanie matematyki.

**Ostrzeżenie z pomiaru, nie z przeczucia:** wzorzec hostowy dla DeltaNet będzie
BARDZO wolny — skan rekurencyjny w f32 po tokenie. Prefill 626 tokenów wzorcem
gęstym kosztuje dziś 24 sekundy; skan rekurencyjny nie zrównolegli się tak jak
mnożenie. Bramka 4b musi być zaprojektowana na krótkie sekwencje od początku,
zamiast odkryć to po napisaniu.

## 5. Rzeczy, które w tych dwóch rodzinach już raz kosztowały płynny, zły tekst

Wszystkie dały poprawnie wyglądające zdania i żadna nie dała awarii:

- **Bias routera wchodzi PRZED wyborem top-k, ale NIE do wag.** DeepSeek V4
  (`noaux_tc`). Pomylenie tego zmienia wybór ekspertów w sposób, który wygląda
  jak inna, też sensowna odpowiedź.
- **Renormalizacja top-k jest własnością architektury, nie stałą.** `norm_topk`.
- **Bramka eksperta współdzielonego to sigmoid per token**, a jej brak znaczy
  wagę 1,0 — nie znaczy „nie ma eksperta współdzielonego".
- **Routing haszowany (`tid2eid`) zastępuje wybór, ale nie wagi.**
- **Dwie ścieżki MoE muszą dawać ten sam wynik.** Silnik ma `_gidx` (bez powrotu
  na hosta) i wariant z odczytem — i sam deklaruje je jako bitowo zgodne dla
  ekspertów routowanych. Jeśli nowa ścieżka dostanie kiedyś dwie formy, to jest
  gotowy wzorzec testu.

## 6. Czego NIE robić

- **Nie rozszerzać `Op` „na zapas".** Wariant dochodzi wtedy, gdy jest wykonawca,
  który go wykonuje, i test, który to pokazuje. Ta reguła kosztowała już raz:
  `FusedNormSilu` przeszedł do słownictwa bez pomiaru, kosztował 2 GiB
  zdublowanych wag i został usunięty.
- **Nie brać obu rodzin naraz.** MoE i DeltaNet dzielą tylko to, że nie są gęste.
- **Nie zaczynać od `forge-engine`.** Nic tam nie kasujemy przed krokiem 6.
- **Nie ufać samemu benchmarkowi.** Identyczne SHA tokenów dowodzą powtarzalności,
  a nie poprawności — tego nauczył błąd z permutacją RoPE.

## 7. Konwencje

Jak w `ZADANIE_CUDA_EXECUTOR.md` §8: angielski w kodzie i commitach, format
`[typ]: opis`, żadnej atrybucji AI, commit i push po każdym kroku, zapadka lintu
zielona, nic nie jest zrobione, dopóki nie zostało uruchomione.
