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

### Checkpoint: Qwen3-30B-A3B Q4_K_M, i dlaczego akurat ten

Pobrany do `.runtime/models/qwen3-30b-a3b-gguf/` (18,56 GiB, jeden plik z
oficjalnego repo `Qwen/Qwen3-30B-A3B-GGUF`). Wybrany świadomie:

- **`qwen3moe`, czyli MoE BEZ hybrydy.** `qwen35moe` też jest zarejestrowany,
  ale to MoE **razem z** DeltaNet — czyli obie rodziny naraz, czego §6 zabrania.
- **Najprostszy wariant routingu, jaki mamy w rejestrze**: bez eksperta
  współdzielonego, bez biasu routera (`noaux_tc` DeepSeeka), bez routingu
  haszowanego. Zostaje samo jądro: logity routera, top-k, renormalizacja
  (`norm_topk_prob = true`), SwiGLU wybranych ekspertów, akumulacja z wagami.
  Wszystkie cztery pułapki z §5 są więc na tym modelu WYŁĄCZONE — wchodzą
  dopiero z DeepSeekiem, i to jest właściwa kolejność.
- **Wzorzec go udźwignie**, mimo 30B: `HostExec` trzyma bloki źródła i dekoduje
  wiersz na żądanie, więc pamięć to rozmiar checkpointu, a nie f32. A MoE aktywuje
  ~3B parametrów na token, czyli mniej niż dzisiejszy gęsty Bielik 7B, na którym
  wzorzec liczy prefill sześciu tokenów w 24 s.
- Q4_K_M, czyli format najlepiej sprawdzony w tej ścieżce.

**Drugi checkpoint, na 4b:** `FreedomAISVR/Qwen3.6-35B-A3B-MXFP4-MOE-Fast-GGUF`
(20,26 GiB) w `.runtime/models/qwen36-35b-a3b-mxfp4-gguf/`. Jego tag to
`qwen3_5_moe`, czyli nasz `qwen35moe` — hybryda — więc jest DOKŁADNIE tym, czego
4a brać nie może i czego 4b potrzebuje. Leży od razu, żeby krok 4b nie zaczynał
się od pobierania.

Warto zapisać, dlaczego to nie jest ten sam model o innej nazwie. „Qwen3-30B-A3B"
i „Qwen3.6-35B-A3B" brzmią jak dwa rozmiary jednej rodziny, a są dwiema
architekturami: pierwsza to uwaga i MoE, druga przeplata uwagę z DeltaNet. Wzięcie
drugiej jako pierwszej znaczy wpuścić naraz błąd routingu i błąd stanu
rekurencyjnego, a oba objawiają się tak samo — płynnym, złym tekstem — więc nie
byłoby jak rozstrzygnąć, który to.

Przy okazji 4b da MXFP4 pierwszy PRAWDZIWY checkpoint: dziś ten format jest
sprawdzony wyłącznie hermetycznie w `format_table.rs`, na bajtach o zawężonym
zakresie kodów.

### 3a. Co ten plik naprawdę wymusza — odczytane z niego, nie założone

Metadane i kształty tensorów pobranego pliku, bo „MoE" to nie jest cała lista:

```
general.architecture = qwen3moe        block_count = 48
embedding_length = 2048                expert_count = 128
head_count = 32, head_count_kv = 4     expert_used_count = 8
key_length = 128                       expert_feed_forward_length = 768
vocab = 151936

blk.0.attn_q.weight        [2048, 4096]        Q4_K
blk.0.attn_output.weight   [4096, 2048]        Q4_K
blk.0.attn_q_norm.weight   [128]               F32
blk.0.ffn_gate_inp.weight  [2048, 128]         F32
blk.0.ffn_gate_exps.weight [2048, 768, 128]    Q4_K
blk.0.ffn_down_exps.weight [768, 2048, 128]    Q6_K
```

Wynikają z tego **cztery** rzeczy do zrobienia, a MoE jest dopiero czwartą:

1. **`heads * head_dim ≠ hidden`.** 32 × 128 = 4096, a hidden to 2048. `Dense`
   zakłada dziś, że Q i O są kwadratowe (`quant(..., h, h, ...)`), i — co warto
   pochwalić — nie zgaduje: `dense.rs:97` ODMAWIA z komunikatem „hidden 2048 nie
   jest iloczynem 32 głowic po 128". Czyli ten checkpoint zatrzyma się przy
   wczytaniu, a nie policzy źle. To jest pierwsza rzecz do zdjęcia i najmniejsza.
2. **Ważony QK-norm per głowica** — patrz niżej.
3. **Router jest F32, nie f16.** Ścieżka MoE silnika żąda f16 wprost („MoE router
   must be f16"). Nowa ścieżka albo przyjmie f32, albo zwęzi go raz przy wgraniu
   — ale to musi być DECYZJA, bo router wybiera ekspertów, więc jego
   zaokrąglenie zmienia wybór, a nie tylko wynik.
4. **Stos ekspertów jest tensorem TRÓJWYMIAROWYM** `[.., .., n_experts]`, w
   dodatku o mieszanych formatach: gate i up w Q4_K, down w Q6_K. `PackedWeight`
   ma dziś `rows`/`cols`, więc stos wchodzi spłaszczony jako
   `[n_experts * moe_inter, hidden]` — tak samo, jak nazywa go silnik. Mieszane
   formaty w jednej warstwie nie są niespodzianką (Q4_K_M robi to samo), ale
   dyspozycja musi patrzeć na format WAGI, nie warstwy.

Co się NIE okazało przeszkodą: słownik 151 936 mieści się w limicie wyboru
(`SAMPLE_MAX_VOCAB` = 262 144).

### 3b. Ważony QK-norm

Rodzina Qwen3 normalizuje Q i K **per głowica**, z UCZONĄ wagą
(`attn_q_norm.weight`, `attn_k_norm.weight`, obie `required: true`, f32 o
szerokości `head_dim`). Dzisiejszy `Dense` jej nie ma, bo Bielik ani llama jej nie
używają — jego lista ról kończy się na `AttnO`.

Kamień 4a rozpadł się więc na trzy, w tej kolejności, każdy z własną bramką:

- **4a-0: kształty. ZROBIONE.** Q i O przestały być kwadratowe
  (`DenseShape::attn_width`). Przy okazji wyszły dwie rzeczy o samym loaderze:
  brakująca rola kończyła się PANICEM z indeksowania mapy, a rola, której model
  NIE CZYTA, nie kończyła się niczym. `check_roles` trzyma teraz obie strony
  przed pierwszym wgraniem i raportuje oba braki naraz.
- **4a-1: QK-norm. ZROBIONE.** `Op::HeadNorm`, osobna operacja, nie szerokość w
  `RmsNorm`. Role opcjonalne, bo llama ich nie ma. Bramka hermetyczna na obu
  wykonawcach wobec formuły wypisanej w teście — jej pierwsza wersja NICZEGO nie
  rozstrzygała, bo przy jednakowo rozłożonych głowicach norma per głowica i norma
  po całej aktywacji zgadzają się co do kilku procent.
- **4a-2: sam MoE.** ← tutaj jesteśmy.

Stan pośredni jest osiągnięty i sprawdzony: checkpoint przechodzi kształt i normy
i zatrzymuje się DOKŁADNIE na FFN — „brakuje ról [FfnGate, FfnUp, FfnDown];
niesie role [FfnGateExps, FfnGateInp, FfnUpExps, FfnDownExps]".

Kernela nie trzeba pisać ani szukać osobnego: silnik robi tę normę przez zwykły
`rmsnorm_f16_at`, licząc GŁOWICE JAKO WIERSZE o szerokości `head_dim`
(`dense.rs`, gałąź `QkvWeights::Fused`). Uwaga na fałszywy trop: istnieje też
`rmsnorm_head_f16`, ale on jest BEZ WAGI — to druga norma Q DeepSeeka V4, nie ta.

Po stronie słownictwa to jest nowa operacja, a nie parametr do `RmsNorm`:
`Op::RmsNorm` normalizuje dziś po `shape.hidden` i tak ma zostać. Rozszerzenie go
o dowolną szerokość dałoby modelowi możliwość opisania normalizacji po kształcie,
którego żaden kernel nie liczy — czyli dokładnie to, przed czym ostrzega §6.

### Gdyby checkpointu zabrakło

4a da się napisać i zabramkować HERMETYCZNIE, tak jak tabelę formatów w
`forge-kernels/tests/format_table.rs`: router o znanych wagach, znany top-k,
eksperci o znanych macierzach, wynik policzony wzorcem w f32. To sprawdza
routing, wybór i akumulację — czyli wszystko, co w tym kroku jest nowe.
Checkpoint dokłada „wynik ma być językiem", czyli jedyne kryterium, którego nie
da się spełnić przez przypadek.

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
