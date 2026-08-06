# Zadanie: MoE i hybryda w słownictwie — karta, nie kod

Krok 4 z §8 `ARCHITEKTURA_DOCELOWA.md`. Ten sam dokument nazywa go **jedyną
pozycją, która jest prawdziwą pracą projektową**, i zabrania pisać go na ślepo:
błąd w tej ścieżce nie objawia się awarią, tylko płynnym, złym tekstem. Dlatego
najpierw karta, potem wzorzec, potem wykonane porównanie.

Krok jest zamknięty; ta karta jest zapisem tego, co wymusił, a nie planem:

| krok §8 | stan |
|---|---|
| 1. wspólna warstwa stanu | KV ✅, admission i continuous batching zostają |
| 2. fuzja jako pass | ✅ zmierzone: +0,8% przy dekodowaniu |
| 3. formaty wag | ✅ 22 kwantyzacje, bramka per format |
| **4. MoE i hybryda w słownictwie** | **✅ obie rodziny, dwa checkpointy, zmierzone** |
| 5. spekulacja jako pass | następny |
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
- **4a-2: sam MoE. ZROBIONE.** `Op::MoeFfn` jedną operacją, routing i wybór po
  stronie wykonawcy. Bramka hermetyczna (wzorzec 0,000%, CUDA 0,194%) i pełna:
  Qwen3-30B-A3B kontynuuje „The capital of France is" poprawnie i zgadza się ze
  wzorcem na 0,433% w prefillu oraz 0,204% w kroku.

  Trzy rzeczy, których karta nie przewidziała, warte zapisania przed 4b:

  - **Kernele `_gidx` NIE biorą stosu, tylko TABLICĘ WSKAŹNIKÓW** i łuskają
    `table[ids[sel]]`. Podanie im wagi czyta jej bajty jako adresy — nielegalny
    dostęp zamiast złej liczby, i to jedyna łaska w tym. Tablica jest u nas
    zdegenerowana (wszystko w jednym ciągłym stosie), ale to właśnie ta
    pośredniość pozwoli później przesuwać ekspertów: rezydencja stanie się
    przepisaniem tablicy, a nie zmianą wywołania kernela.
  - **Stosy przychodzą trójwymiarowe**, a wiodące wymiary SKŁADAJĄ się w liczbę
    wierszy bez przepakowania, bo źródło jest wierszowe. Szerokość wiersza jest
    natomiast sprawdzana osobno.
  - **Ekspert współdzielony jest ODRZUCANY**, nie pomijany. Policzenie
    checkpointu bez niego to inny model, który dalej mówi. 4b go wnosi.

  Rozstrzygnięte też pytanie o precyzję routera: CUDA zwęża go do f16, bo tego
  chce kernel, a wzorzec trzyma f32 źródła. Na tym checkpoincie zamieniają się
  miejscami dwa remisujące tokeny — i test tego nie wybacza, tylko SPRAWDZA, że
  wzorzec dzieli je mniej niż wynosi mierzony błąd.

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
operacja", tylko **gdzie ten stan leży**. Odpowiedź wskazał precedens: obok KV,
w `forge-state` (`recurrent.rs`). Powód nie jest estetyczny — spekulacja musi
umieć ten stan zacheckpointować i wycofać, a stan schowany w wykonawcy jest
stanem, do którego krok 5 nie ma jak sięgnąć.

### 4a. Czego ten checkpoint naprawdę wymaga — odczytane, nie założone

`FreedomAISVR/Qwen3.6-35B-A3B-MXFP4-MOE-Fast-GGUF`, 20,26 GiB:

```
general.architecture = qwen35moe    block_count = 41 (40 + 1 głowa MTP)
embedding_length = 2048             full_attention_interval = 4
head_count = 16, head_count_kv = 2  expert_count = 256, used = 8
key_length = 256                    expert_ff_length = 512, shared = 512
rope.dimension_sections=[11,11,10,0] ssm: conv 4, state 128, group 16, rank 32

blk.0.attn_qkv.weight      [2048, 8192]      Q8_0    (DeltaNet: q|k|v)
blk.3.attn_q.weight        [2048, 8192]      Q8_0    (uwaga: q|bramka na głowicę)
blk.0.ffn_gate_exps.weight [2048, 512, 256]  MXFP4
blk.0.ffn_gate_inp_shexp.weight [2048]       F32     (bramka sigmoid per token)
```

**Pięć rzeczy naraz**, więc kamień rozpadł się na cztery, każdy z własną bramką:

- **4b-0: checkpoint w ogóle się otwiera. ZROBIONE.** Deskryptor ODRZUCAŁ każdą
  hybrydę MoE niosącą głowę spekulacji, komunikatem o natywnym MTP JEDNEGO
  runtime'u. Ta odmowa stała przed całym plikiem, więc czterdzieści warstw
  trunku było ubocznym łupem, a `build_moe_mtp` tuż pod nią było martwym kodem,
  do którego żaden test nie mógł dojść. Przeniesiona tam, gdzie ten runtime
  naprawdę składa głowę.
- **4b-1: ekspert współdzielony. ZROBIONE.** Wchodzi DO `Op::MoeFfn`, nie obok:
  to jeden blok i jeden akumulator. Jego bramka jest WYMAGANA, a nie opcjonalna —
  brak bramki znaczy wagę 1,0, co jest innym modelem, a `Option` zlałby oba
  zdania w jedno. Szerokość bierze z własnych stosów, nie z `inter`. Zmierzone:
  wzorzec 0,000%, CUDA 0,242%, bramka 0,267.
- **4b-2: bramkowana uwaga i częściowe RoPE. ZROBIONE.** Projekcja Q leży w
  pliku podwójnie szeroka, PRZEPLECIONA PER GŁOWICĘ; podział na dwie wagi robi
  `forge-formats` przy wczytaniu, więc słownictwo dostaje dwie zwykłe macierze
  zamiast operacji od rozbierania tensora. Bramka nakłada się PO uwadze
  (`Op::SigmoidMul`), bo wcześniej skalowałaby pytanie zamiast odpowiedzi. Obrót
  bierze 64 z 256 wymiarów i paruje `j` z `j + rot/2` — inny kernel, nie ten sam
  zatrzymany wcześniej — a `rope_rot` mieszka w KSZTAŁCIE, żeby zapytanie i jego
  klucz nie mogły obrócić się o różne kąty.
- **4b-3: sam DeltaNet. ZROBIONE.** `Op::DeltaNet` jedną operacją, bo granica
  operacji ma leżeć tam, gdzie granica STANU: jedno wywołanie to jedno
  posunięcie okna splotu i macierzy, czyli dokładnie ta jednostka, którą
  spekulacja będzie cofać. Projekcje są bezstanowe i liczą się dla wszystkich
  wierszy kroku naraz; tylko zwijanie idzie token po tokenie, bo rekurencji nie
  da się poszerzyć.

  Rzeczy, których karta nie przewidziała:

  - **Stos ekspertów MXFP4 nie miał kernela `_gidx`.** Istniały tylko Q4_K i
    Q6_K — akurat te dwa, których używa Qwen3-30B. Checkpoint wczytywał się w
    całości (20 GiB, 24 s) i zatrzymywał na PIERWSZYM routowanym mnożeniu. To
    jest wynik warty zapisania: „kernele istnieją w komplecie" było prawdą dla
    4a i nie było dla 4b. Dopisany `gemv_mxfp4_f16_gidx` to dwunastowierszowa
    nakładka na istniejący akumulator wiersza — nowa jest ADRESACJA, nie
    matematyka.
  - **Zerowanie stanu wynika z KROKU, a nie z osobnego sygnału.** Lane
    zaczynający od pozycji zero jest sekwencją, która zaczyna się tutaj — ten
    sam sygnał, którym stronicowany cache nadpisuje od przodu. Osobne wywołanie
    „wyczyść" dałoby się zapomnieć, a slot z połową cudzej rozmowy zwiniętą w
    macierzy mówi płynnie.
  - **Wagi zwykłe idą kontraktem `norm_weights`, nie f16.** Splot, `dt_bias`,
    `a` i norma wyjścia to wektory f32 źródła; podanie ich jako połówek każe je
    czytać parami jako pojedyncze liczby. Pierwsza wersja bramki padła na tym
    właśnie, we własnej fiksturze.
  - **Nowy slot aktywacji kosztuje pulę, a nie tylko pamięć.** `format_table`
    trzyma po jednym wykonawcy na każdy z dwudziestu dwóch formatów naraz, więc
    urósł mu budżet o liczbę SLOTÓW, nie o pracę.

Bramka DeltaNet jest hermetyczna i sprawdza trzy osobne rzeczy: zgodność z
formułą wypisaną w teście (nie przez `forge-formats::deltanet`, bo to jest to,
co składa wzorzec — nieprzetestowana byłaby wtedy KOLEJNOŚĆ), zależność drugiego
tokenu od pierwszego, i to, że restart od pozycji zero odtwarza wynik co do bitu.

### 4b. Wynik, i ostrzeżenie, które się NIE sprawdziło

Qwen3.6-35B-A3B wczytuje się w 9,7 s i kontynuuje „The capital of France is"
poprawnie, 20 tokenów w 1,11 s. Wobec wzorca hostowego: **0,356% rozpiętości w
prefillu i 0,178% w kroku, ten sam token po obu stronach.** Krok liczy się
osobno od prefillu właśnie dlatego, że czyta trzydzieści macierzy stanu, które
prefill zostawił — sama arytmetyka jednego tokenu tego nie sprawdza.

Karta ostrzegała, że wzorzec dla DeltaNet będzie BARDZO wolny, bo skan
rekurencyjny nie zrównolegli się jak mnożenie. **Zmierzone: 11,9 s na pięć
tokenów**, czyli o połowę SZYBCIEJ niż gęsty Bielik 7B na sześciu (24,9 s).
Przewidywanie było oparte na złej wielkości. Rekurencja nie jest kosztem: jej
stan ma stały rozmiar, więc token kosztuje tyle samo na pozycji 5 co na 5000,
podczas gdy uwaga czyta cały kontekst. Kosztem są PROJEKCJE — a tych mieszanka
aktywuje 3B na token, mniej niż Bielik ma gęsto.

To jest powód, żeby bramkę porównawczą trzymać krótką z innego powodu niż
zakładany: nie dlatego, że dłuższa nie zdąży, tylko dlatego, że dłuższa niczego
by nie dodała.

### 4c. Co ten krok zostawił niedokończone, świadomie

- **Cache KV na warstwy rekurencyjne: NAPRAWIONE**, i warto zapisać, dlaczego
  wpis o tym nie przeżył jednego dnia. Zwarta mapa `warstwa → slab` już była w
  `forge-state` (`KvLayerMap::from_attention_mask`) — nieużywana, bo wykonawca
  wołał `KvCache::new`, które zakłada mapę tożsamościową. Brakowało jednej
  rzeczy: kto ma powiedzieć, które warstwy mają uwagę. Odpowiedź jest ta sama,
  co przy mikserze — checkpoint, przez tę samą rolę (`SsmInProj`), więc obie
  odpowiedzi nie mogą się rozjechać. `ExecSpec` niesie maskę, bo cache powstaje
  ZANIM przyjdzie pierwsza operacja.

  Zmierzone na Qwen3.6: strona kosztuje swoje bajty we wszystkich zaalokowanych
  warstwach naraz, więc **20 MiB przy czterdziestu slabach i 5 MiB przy
  dziesięciu**. Pełna pojemność wymagała 1,28 GiB puli, teraz 320 MiB. Bramka
  jest rozstrzygająca, a nie deklaratywna: test biegnie na puli 256 MiB, na
  której stary układ ODMAWIA (`OutOfMemory`, 320 MiB żądane wobec 240
  dostępnych) — sprawdzone przez chwilowe cofnięcie zmiany, nie przez rachunek.

  Zła maska nie jest cicha: `KvAppend` nazwałby warstwę bez cache'u i odbiłby
  się po nazwie, zamiast pisać w cudzy slab.
- **Zwijanie idzie token po tokenie także w prefillu.** Rekurencji nie da się
  poszerzyć, ale splot, projekcje i normy owszem; silnik ma na to osobne kernele
  chunked/persistent. Ten krok mierzy poprawność, nie przepustowość, więc wejdą
  dopiero przy porównaniu obu ścieżek.
- **Metal odmawia obu rodzin.** Kerneli MSL nie ma, a maszyny, która by je
  uruchomiła, też nie — więc odmowa jest w jednym miejscu na rodzinę zamiast
  kernela napisanego na ślepo.

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
