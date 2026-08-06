# Architektura docelowa — mierzona kosztem dokładania, nie elegancją

Pytanie brzmi: co będzie najtańsze w utrzymaniu, gdy dojdą nowe modele, nowe
kwantyzacje i nowy sprzęt. Nie „co jest ładne". Miarą jest więc jedna liczba:

> **ile miejsc trzeba ruszyć, żeby dołożyć jedną rzecz** — i czy kompilator
> wymienia te miejsca sam, czy trzeba je pamiętać.

Poniżej wnioski z tej sesji, w której dołożyliśmy jeden format (GGUF), dwie
kwantyzacje (Q4_K, Q6_K) i podeszliśmy do drugiego backendu.

## 1. Co zadziałało — i dlaczego warto to powtórzyć

### Format: jedna implementacja, zero zmian w modelu

Dodanie GGUF-a do ścieżki Metalowej nie wymagało **ani jednej gałęzi** w modelu.
Zadziałało, bo warstwa formatów opisuje wejście DANYMI:

- `TensorSource` — skąd wziąć bajty
- `ModelDescriptor` — która nazwa pełni którą rolę
- `Checkpoint` — jedno wejście, ukrywa katalog kontra plik

Model pyta o rolę, dostaje bajty. To jest wzorzec do skopiowania wszędzie
indziej.

### Wybór wariantu: dane z przypiętym pomiarem

`variant.rs` — uporządkowana lista form, każda z predykatem i liczbą, która ją
tam postawiła — złapał realny klif przy pierwszym uruchomieniu i przyjął drugą
oś (szerokość kodu) bez przebudowy. Selekcja jako dane działa.

### Kwantyzacja: cztery formaty okazały się jedną formułą

MLX affine, Q4_1, Q4_K i Q6_K liczą `q * skala + przesunięcie`. Różnią się
segregacją: ile wag dzieli skalę, gdzie leżą bity, czy parametry są obok danych.
Dodanie Q6_K to był **jeden konwerter i jeden parametr kernela**, nie nowa
rodzina.

Ale uwaga na przesadę w drugą stronę: Q8_0 nie ma przesunięcia, Q5_K trzyma
piąty bit inaczej. Formuł jest kilka, nie jedna — i nie wolno ich zlewać po
cichu.

## 2. Co nie zadziałało — i co to mówi o projekcie

### Cztery razy wziąłem własność WAGI za stałą MODELU

Szerokość kodu, rozmiar grupy, typ wagi normy, kolejność wierszy Q/K. Za każdym
razem model zakładał jedną wartość dla wszystkich wag, a Q4_K_M ma dwie.

**Wniosek projektowy:** każda właściwość podróżuje z tym, co opisuje, a warstwa
wykonawcza SPRAWDZA ją przy użyciu, zamiast ufać wołającemu. Model niosący jak
najmniej właściwości to model, który najmniej może pomylić.

### Cztery błędy z tej sesji dawały płynny, błędny tekst

Permutacja Q/K, zgubione przesunięcie aktywacji, mieszane grupy, normy f32.
Żaden nie objawił się awarią — wszystkie dawały poprawnie wyglądające zdania.

**Wniosek projektowy:** ta klasa błędów nie znika przez staranność. Znika przez
to, że niezgodność jest **niereprezentowalna albo sprawdzana**. Stąd jedno
miejsce wiązania buforów wagi, stąd bramka „wynik ma być językiem", stąd flaga
`stores_original_rope_order` w źródle zamiast w pamięci programisty.

### Model trzymający bufory to model dla tej karty

3918 linii na jedną architekturę gęstą, bo `dense.rs` (CUDA) i `mlx_dense.rs`
(Metal) opisują tę samą kolejność warstw. Przyczyną jest jeden import.

## 3. Docelowy układ

```
forge-types      DType, KSZTAŁTY, błędy, opis możliwości sprzętu
forge-quant      dekodery kwantyzacji + referencja CPU + repack     (zero HAL)
forge-formats    GGUF / safetensors / MLX / NVFP4 + rejestr architektur
forge-graph      SŁOWNICTWO OPERACJI, passy, plan kroku             (zero HAL)
forge-hal        jedyny styk ze sprzętem
forge-kernels    rejestr wariantów + wykonawcy per backend
forge-state      KV stronicowany, prefix cache
forge-model      architektury: emitują operacje, nie uruchamiają ich
forge-sched, forge-spec, forge-io, forge-server, forge-cli
```

To jest §5.1 planu z **jedną istotną zmianą**, którą uzasadniam niżej.

## 4. Klucz: operacje jako DANE, nie jako trait

Stałem przed wyborem i wybór ma konsekwencje na lata.

**Wariant A — trait z metodą na operację.** Model woła `exec.op_matmul(...)`.
Prosty, bez alokacji. Ale: dodanie operacji zmienia trait, więc **każdy backend
przestaje się kompilować**, nawet jeśli go nie dotyczy. I model musi zależeć od
`forge-kernels`, żeby ten trait zobaczyć.

**Wariant B — operacje jako typ danych.** Model emituje `Op::MatMul { out, w, x }`,
backend ma jeden `execute(&Op)`.

Wybieram **B**, z czterech powodów, z których trzy wynikają wprost z tej sesji:

1. **Kierunek zależności.** Przy B `forge-model` zależy WYŁĄCZNIE od
   `forge-graph` — nie od kerneli, nie od HAL-a. Granica sprzętowa przestaje być
   regułą do pilnowania, a staje się faktem: model fizycznie nie ma czym nazwać
   bufora. Przy A model musi znać kernele.
2. **Kompilator wymienia miejsca do zmiany.** Nowa operacja to nowy wariant
   `enum` i jeden brakujący `match` na backend — kompilator je wypisze. Nowa
   metoda w traicie też, ale przy B backend, który danej operacji nie obsługuje,
   zwraca `Unsupported` w jednym miejscu zamiast implementować zaślepkę.
3. **Passy.** Plan nazywa D1 pierwszym długiem: „forward pisany ręcznie, brak
   IR, fuzja jako praca ręczna". Ciąg operacji jako dane da się przepisać przed
   wykonaniem — fuzja, zmiana kolejności, autotuning — **bez dotykania modeli**.
   Przy traicie każda taka zmiana to zmiana w każdym modelu.
4. **To ten sam wzorzec, który już tu wygrywa.** Format opisany danymi zadziałał.
   Wybór wariantu opisany danymi zadziałał. Nie ma powodu, żeby akurat operacje
   były wyjątkiem.

Koszt B: jedna warstwa dyspozycji i trochę więcej maszynerii na start. Przy
jednym backendzie A wygląda taniej — i to jest dokładnie ta pułapka, w którą
wpadł ten projekt raz już przy `mlx_dense`.

**Wejście najmniejszymi drzwiami:** `Vec<Op>` bez żadnych passów, wykonywany po
kolei. To już daje kierunek zależności i wymienialność backendów. Passy dochodzą
później, nie zmieniając ani modeli, ani backendów.

## 5. Jak wtedy wygląda dokładanie

| co dokładasz | gdzie piszesz | czy kompilator pilnuje |
|---|---|---|
| **format** | jedna `impl TensorSource` | tak |
| **kwantyzacja** o znanej formule | jeden konwerter do `QuantDesc` | tak, przez wzorzec CPU |
| **kwantyzacja** o nowej formule | konwerter + wariant kernela | tak |
| **model** | jedna funkcja emitująca operacje | tak |
| **sprzęt** | `impl Executor` + kernele + wpisy w rejestrze | tak, przez brakujące `match` |
| **wariant kernela** | wpis w rejestrze Z POMIAREM | bramka klifu |

Żadna z tych pozycji nie dotyka pozostałych. Dziś dokładanie kwantyzacji ruszało
model, kernele i loader naraz.

## 6. Czego świadomie NIE unifikować

- **Systemów budowania kerneli.** CUDA buduje PTX wcześniej, Metal kompiluje MSL
  przy starcie. To dwie różne rzeczy i wspólny „system budowania" byłby warstwą
  bez treści.
- **Prefillu i dekodowania.** Uderzają w różne ściany — obliczeniową i pamięciową
  — i chcą różnych decyzji. Zmierzone: CPU pomaga w jednym o kilkanaście procent,
  w drugim szkodzi o 14%.
- **Jednego uniwersalnego `Tensor`.** Nieprzezroczyste uchwyty plus nazwane sloty
  wystarczyły dla modelu gęstego i są dużo prostsze. Uogólniać dopiero, gdy druga
  architektura tego zażąda.
- **Wszystkich kwantyzacji do jednej formuły.** Cztery pasują, dwie nie. Cicha
  konwersja Q8_0 do formy afinicznej dałaby model, który mówi.

## 7. Kolejność, która nie wymaga wielkiego wybuchu

1. **`DenseShape` → `forge-types`.** Odblokowuje wszystko inne, bo dziś to on
   trzyma wykonawcę w `forge-model`. Plan i tak mówi, że kształty tam mieszkają.
2. **`forge-graph` z samym `enum Op` i wykonawcą sekwencyjnym.** Bez passów.
3. **`dense` emituje operacje**; `MetalExec` staje się `impl Executor` w
   `forge-kernels`. Wtedy `forge-model` przestaje zależeć od HAL-a i od kerneli —
   sprawdzalne zapadką. ZROBIONE: zapadka `hal_boundary` dla `forge-model/src`
   spadła z 3 na 0, a wykonawca dochodzi jako WYTWÓRNIA, bo nie może powstać
   przed odczytaniem checkpointu — dopiero ten mówi, ile warstw dostanie cache
   i dla jakiego typu skompilować kernele.
4. **Drugi wykonawca tego samego kontraktu.** ZROBIONE dla jednej sekwencji.
   Wzorzec hostowy (`HostExec`) liczy te same operacje w zwykłym Ruście i
   zgadza się z tą samą wyrocznią mlx-lm co Metal, nie dzieląc z nim ani jednej
   linii poza kontraktem. Kontrakt z jednym wykonawcą jest kontraktem na papierze; ten
   przestał nim być. Wzorzec jest przy okazji jedyną bramką na cały przebieg,
   którą da się uruchomić na maszynie bez akceleratora.

   **CUDA — TAK, dla jednej sekwencji.** `CudaExec` liczy to samo słownictwo na
   GGUF-ie Q4_K_M, zgadza się ze wzorcem hostowym na 0,013–0,039% rozpiętości
   logitów przy identycznych tokenach i kontynuuje polski prompt poprawnie
   (`crates/forge-model/tests/cuda_vs_reference.rs`, DGX Spark GB10). Trzeci
   wykonawca rozstrzygnął przy okazji dwie rzeczy, których dwaj pierwsi nie
   mogli: postać wagi jest sprawą WYKONAWCY, nie modelu (Metal chce trójki
   afinicznej, CUDA bloków źródła — trzymanie jednej z nich w modelu wybierało
   przeciwko drugiemu i było stratne dla Q6_K), a permutacja wierszy RoPE należy
   przez to do bajtów źródła. Szczegóły i odstępstwa:
   `docs/ZADANIE_CUDA_EXECUTOR.md`.

   **Szacunek „to kasuje 2822 linie" był natomiast zły.** Po przeczytaniu
   `forge-engine/src/model/arch/dense.rs`: kolejność warstw to jego niewielka
   część. Reszta to `prefill_forward_lanes` (871 linii, prefill wsadowy z
   doklejonymi wierszami dekodowania), TRZY osobne łańcuchy dekodowania
   (`run_step_rot`, `run_step_separate`, `run_step_fused`), bramkowanie fuzji po
   dwudziestu kilku formatach wag, `batched_decode` i `mixed_prefill_decode_step`.
   Dzisiejsze `Op` nie wyraża ani stronicowanego KV, ani lane'ów wsadu, ani sum
   częściowych pod tensor parallel, ani szerokości spekulacji — a scalony
   łańcuch to nie inna kolejność, tylko FUZJA tej samej, czyli krok 6, nie 4.

   Dlatego krok 4 rozpada się na: (a) rozszerzenie słownictwa o lane'y i
   stronicowane KV, (b) fuzję jako pass nad `Vec<Op>` zamiast trzech ręcznych
   łańcuchów, (c) dopiero wtedy chudnięcie pliku silnika. Żadnego z nich nie
   wolno napisać na ślepo: ten dokument w całości jest o tym, że błąd w takiej
   ścieżce nie objawia się awarią, tylko płynnym, złym tekstem — więc każdy z
   nich potrzebuje karty, wzorca i wykonanego porównania, a nie przeglądu.

   (a) jest zrobione. `Op` niesie `Step` — lane'y i tokeny na lane — a
   stronicowane KV zmieściło się CAŁE po stronie wykonawcy: lane mówi, w którym
   slocie siedzi i od której pozycji, a strony, lista wolnych i tablica stron
   nie mają w słownictwie ani jednego pola. Cztery sekwencje naraz kosztują 6%
   na sekwencję i dają 3,8x łącznie (35,7 -> 133,9 tok/s, Bielik 7B Q4_K_M na
   DGX Spark). Przy okazji wyszła rzecz, której nie dało się zgadnąć: kafel
   prefillu jest ZŁYM kernelem dla wsadu dekodowania i przy trzech lane'ach był
   wolniejszy niż trzy osobne przebiegi — szczegóły w
   `docs/ZADANIE_CUDA_EXECUTOR.md`.
5. **`forge-quant` wydzielony z `forge-formats`**, z wzorcem CPU jako wyrocznią.
6. **Passy nad `Vec<Op>`** — fuzja i autotuning, bez dotykania modeli.

Kroki 1–3 są mechaniczne i sprawdzalne istniejącymi testami. Krok 4 jest tym, po
którym widać zysk; jego pierwsza część jest zrobiona, reszta czeka na
słownictwo, którego dziś nie ma. Kroki 5–6 są ulepszeniami, nie warunkami.

## 8. Jak z dwóch ścieżek zrobić jedną

Dziś w repozytorium liczą DWIE rzeczy i dzielą tylko kernele:

| | `forge-engine` | `forge-model` + wykonawcy |
|---|---|---|
| formaty wag | **24** | 22 |
| rodziny architektur | dense, MoE, hybrid, DeepSeek | dense, MoE, hybrid |
| radix, continuous batching, spekulacja, TP | **jest** | nie ma |
| Apple / Metal | **zero wystąpień** | jedyna, która tam liczy |

Kierunek zejścia nie jest wyborem. Silnik nie przeniesie się na Metal ani na
żaden następny backend, bo jego model JEST kodem wołającym kernele jednej
rodziny kart — „przeniesienie" znaczyłoby napisanie go drugi raz, czyli
dokładnie ten błąd, który ten dokument opisuje w §2. Nowa ścieżka wyraża to
samo danymi i ma trzech wykonawców. Więc: nowa ścieżka rośnie o to, co silnik
ma, silnik chudnie, a produkcja przechodzi DOPIERO po zrównaniu w pomiarze.

Kolejność, w której każdy krok odblokowuje następny:

1. **Wspólna warstwa stanu.** ZROBIONE dla KV: `forge-state` trzyma stronicowany
   cache i drzewo radix, oba wyjęte z `forge-engine` bez zmiany ani jednej linii
   logiki, a `CudaExec` porzucił własne stronicowanie na ich rzecz — te same
   slaby, te same identyfikatory stron, ta sama arytmetyka (0,019% prefillu i
   0,572% kroku wobec wzorca, bez zmiany po przesiadce). Zostaje admission i
   continuous batching (`server.rs`), które idą tą samą drogą: też nie zależą od
   tego, jak model liczy warstwę, tylko od stron i tokenów. Na Apple wspólne
   stronicowanie wymaga stronicowanych wariantów dwóch kerneli MSL, bo
   `MetalExec` trzyma dziś cache jako jedną ciągłą połać na warstwę i deklaruje
   przez to jeden lane.

   Przesiadka pokazała jedną rzecz wartą zapisania: `KvCache::grow` liczy tokeny
   PRZYROSTOWO, a mapowanie stron woła się raz na warstwę — czterdzieści razy na
   krok. Ręczna wersja była idempotentna przypadkiem, wspólna musi być celowo, i
   jest na to asercja, która sama powiedziała, co się stało.

   Krok 4 dołożył drugą: wspólna warstwa stanu miała już zwartą mapę
   `warstwa → slab` i nikt jej nie wołał, bo wykonawca brał `KvCache::new` z
   mapą tożsamościową. Dla stosu, w którym uwagę ma co czwarta warstwa, kosztuje
   to CZTEROKROTNIE większą pulę na tę samą pojemność (zmierzone: 1,28 GiB wobec
   320 MiB). Wniosek nie dotyczy tej jednej mapy: **wspólny kod, którego druga
   ścieżka nie woła, nie jest wspólny** — jest drugą implementacją czekającą, aż
   ktoś napisze ją od nowa.
2. **Fuzja jako pass** (§7.3 `ZADANIE_CUDA_EXECUTOR.md`). ZROBIONE dla dwóch
   par, ZMIERZONE, i pomiar okazał się ważniejszy niż sam pass.

   `forge-graph::fuse` przepisuje `RmsNorm`+`MatMul` oraz `MatMul`+`Residual`
   na operacje scalone, a wykonawca, który nie ma scalonego kernela, rozkłada
   je z powrotem na składowe — dzięki temu pass jest niezależny od backendu i
   wzorzec hostowy pozostaje wyrocznią dla ścieżki scalonej. Bielik 7B Q4_K_M,
   DGX Spark, dekodowanie jednej sekwencji, mediana z trzech przebiegów jednej
   sesji: **35,8 tok/s bez fuzji, 36,1 z nią**.

   Czyli fuzja przy dekodowaniu jest oszczędnością URUCHOMIEŃ, a nie pasma —
   0,8%. I to jest odpowiedź na „szesnaście uruchomień na warstwę wobec
   trzech": ta różnica jest realna, ale kosztuje mniej, niż zakładaliśmy, więc
   nie ona blokuje porównanie obu ścieżek. Przy dekodowaniu obie i tak czytają
   całą macierz na krok.

   Trzecia para, `RmsNorm`+gate+up+`SiLU`, została USUNIĘTA po pomiarze:
   dawała 0,0% ponad pozostałe dwie, bo scalenie gate i up przenosi te same
   bajty, a kosztowała kopię obu macierzy na warstwę — ~2 GiB na Q4_K_M i
   3,7 GiB na Q8_0, na którym po prostu nie mieściła się w puli. Wniosek
   ogólniejszy niż ta jedna operacja: **scalenie, które nie zmniejsza ruchu
   bajtów, nie ma czego przyspieszyć przy dekodowaniu**, a scalenie kupione
   drugą kopią wag musi mieć pomiar, dokładnie jak przepakowanie NVFP4→FP8 w
   `PLAN_ARCHITEKTURA.md`.
3. **Formaty wag.** ZROBIONE — i „24 wobec 2" było nieprawdą już w chwili
   pisania. Tabela w wykonawcy (`block_formats!`) dyspozycjonuje **22
   kwantyzacje**: całą rodzinę legacy, K-kwanty, I-kwanty, MXFP4 i NVFP4
   w układzie GGUF-a. Dodanie kolejnej to jeden wiersz, a `match` nie ma
   gałęzi „reszta", więc format spoza tabeli odbija się przy wgraniu.

   Brakowało natomiast drugiej połowy — bramki. Osiemnaście z tych
   dwudziestu dwóch było nazwą kernela i niczym więcej: nigdy nie zostały
   URUCHOMIONE. To najgorszy możliwy stan akurat dla tej tabeli, bo wiersz
   wskazujący zły kernel zwraca liczby, a nie błąd, i formaty, z którymi
   można go pomylić, mają często ten sam rozmiar bloku.

   Bramka jest hermetyczna, bo checkpoint per format się nie skaluje (jedno
   pobranie na format plus kwantyzator, którego nie mamy):
   `forge-kernels/tests/format_table.rs` mnoży TE SAME BAJTY dwa razy —
   kernelem, który wybiera tabela, i przez `forge-formats::dequantize_to_f32`,
   który dekoduje wszystkie 22 i nie dzieli z Mojo ani jednej linii. Idzie
   przez publiczny kontrakt, więc trzyma też ścieżkę wgrania i sprawdzenie
   geometrii bloku, i obejmuje wszystkie trzy rodziny tabeli, bo wiersz może
   być dobry w jednej i zły w drugiej.

   Wszystkie 22 zgadzają się, najgorszy przypadek 0,384% rozpiętości wiersza.
   Q4_K, Q6_K i Q8_0 mają w GEMV 0,25–0,38%, bo idą ścieżką int8 — to ta sama
   liczba, co zgodność formy wsadowej z wektorową.

   Czego ta bramka NIE obejmuje i trzeba o tym wiedzieć: bajty kodów są
   maskowane do sześciu bitów, żeby pole skali każdego układu wyszło skończone
   bez znajomości, gdzie który układ je trzyma. Górne dwa bity kodu sprawdzają
   więc tylko cztery formaty mające prawdziwe checkpointy.
4. **MoE i hybrid w słownictwie.** ZROBIONE. Karta:
   `docs/ZADANIE_MOE_HYBRID.md`. Obie rodziny weszły JEDNĄ operacją każda —
   `Op::MoeFfn` i `Op::DeltaNet` — dokładnie jak `Attention`, którego
   stronicowanie zmieściło się całe po stronie wykonawcy. MoE poszło pierwsze,
   bo routing da się odtworzyć na wzorcu dokładnie, a stan rekurencyjny wymaga,
   żeby porównywalny był też sam stan.

   Przeszkoda, którą karta nazwała — brak małego checkpointu MoE — okazała się
   nie istnieć: Qwen3-30B-A3B (18,6 GiB, Q4_K_M) aktywuje ~3B parametrów na
   token, więc wzorzec liczy go SZYBCIEJ niż gęstego Bielika 7B. Hybrydę wziął
   Qwen3.6-35B-A3B MXFP4, który wnosi naraz DeltaNet, eksperta współdzielonego z
   bramką sigmoid, bramkowaną uwagę i częściowe RoPE.

   Cztery rzeczy warte zapamiętania ponad samą implementację:

   - **Granica operacji ma leżeć tam, gdzie granica STANU.** `Op::DeltaNet`
     obejmuje dziewięć kroków, bo jedno jej wywołanie to jedno posunięcie okna
     splotu i macierzy — czyli dokładnie ta jednostka, którą krok 5 będzie
     cofać. Rozbita na kawałki wymagałaby czterech slotów aktywacji istniejących
     dla jednej architektury i wycofania bez jednej rzeczy do wycofania.
   - **Stan rekurencyjny jest TAŃSZY niż uwaga, nie droższy.** Karta ostrzegała,
     że wzorzec dla DeltaNet będzie bardzo wolny. Zmierzone: 11,9 s na pięć
     tokenów wobec 24,9 s gęstego Bielika na sześciu. Stan ma stały rozmiar,
     więc token na pozycji 5000 kosztuje tyle co na 5; kosztem są projekcje.
   - **„Kernele istnieją w komplecie" było prawdą dla 4a i nie dla 4b.** Stosy
     ekspertów MXFP4 nie miały kernela adresowanego na urządzeniu — istniały
     tylko Q4_K i Q6_K, akurat te dwa, których używa Qwen3-30B. Checkpoint
     wczytywał się w całości i zatrzymywał na pierwszym routowanym mnożeniu.
   - **Postać wagi należy do wykonawcy, ale ROLA wiersza do modelu.** Bramkowana
     projekcja Q leży w pliku podwójnie szeroka i przepleciona per głowicę;
     rozdzielenie jej przy wczytaniu daje słownictwu dwie zwykłe macierze
     zamiast operacji, której zadaniem jest rozbieranie tensora.

   Zmierzone wobec wzorca: Qwen3-30B-A3B 0,433% / 0,204%, Qwen3.6-35B-A3B
   0,356% / 0,178%, ten sam token po obu stronach. Metal odmawia obu rodzin w
   jednym miejscu każdej — kerneli MSL nie ma, a napisanie ich na ślepo na
   maszynie, która ich nie uruchomi, dałoby płynny, zły tekst dokładnie tam,
   gdzie nikt nie patrzy.
5. **Spekulacja jako pass plus kontrakt proposera.** `forge-engine::speculation`
   ma już typowany `Proposer` i statystyki akceptacji, więc to przeniesienie,
   nie wymyślanie.
6. **Serwer przechodzi, silnik chudnie.** Dopiero tutaj kasujemy cokolwiek.

Reguła na cały ten czas: żaden krok nie kończy się deklaracją, tylko pomiarem
wobec wzorca hostowego (poprawność) i wobec silnika (wydajność), na tym samym
checkpoincie.

### Drugą połowę tej reguły przez cztery kroki płacono tylko obietnicą

Poprawność była mierzona za każdym razem. Wydajność nie była mierzona ANI RAZU
wobec silnika — i różnica, gdy wreszcie padło pytanie, wynosiła **czterokrotność**
na prefillu. Bielik 7B Q4_K_M, GB10, prompt 512 tokenów: silnik 5535 tok/s, nowa
ścieżka 1256. Dekodowanie było równe od początku (35,8 wobec 34,2), więc nic w
poprzednich pomiarach tego nie pokazywało.

Bisekcja przez osobne drzewo robocze wykluczyła regresję: przed krokiem 4 było
947 tok/s, po nim 939. Nie zepsuliśmy niczego — po prostu nigdy nie zbudowaliśmy
tego, co silnik ma. Złożyły się na to dwie rzeczy i obie były jedną linią decyzji:

- **Tabela formatów wysyłała prefill na przenośny kafel f16**, gdy dla Q4_K i
  Q8_0 istniały warianty na całkowitoliczbowych jednostkach macierzowych.
  939 → 1256 tok/s po zmianie dwóch wierszy. Wiersz tabeli, który wskazuje
  wolniejszy kernel, nie różni się niczym od dobrego — liczy to samo.
- **Prefill mnożył przez postać z dysku.** Postać blokowa jest właściwa dla
  dekodowania, gdzie kosztem są przenoszone bajty; przy prompcie kosztem jest
  arytmetyka, a superbloki dekodują się w pętli wewnętrznej. Silnik od dawna
  trzymał DRUGĄ postać wagi (e4m3, skala na wiersz) i przełączał się na nią przy
  szerokości promptu — nowa ścieżka nie miała jej wcale.

Po dołożeniu drugiej postaci (`cuda_exec/fp8.rs`): **5230 tok/s wobec 5535
silnika, czyli 95%**, przy 1478 tok/s tej samej ścieżki bez niej. Kosztuje to
1,7× większy rozjazd z wzorcem na prefillu (0,326% → 0,557% rozpiętości, ten sam
token i ta sama czołówka) — dokładnie tę samą wymianę robi silnik, bo to te same
kernele.

Trzy rzeczy z tego, ważniejsze niż sam mnożnik:

- **Paczka powstaje przy PIERWSZYM szerokim mnożeniu, nie przy wczytaniu.**
  Wykonawca nie zna roli wagi — `put_quant` widzi wiersze, kolumny i format —
  więc pakowanie przy wczytaniu znaczyłoby pakowanie wszystkiego, ze stosami
  ekspertów włącznie, których prefill i tak idzie kernelami adresowanymi na
  urządzeniu i nie ma dla nich gęstego GEMM-u. Czekanie na użycie odpowiada na
  to pytanie dokładnie i bez tabeli ról do utrzymywania.
- **Szybsza ścieżka silnika jest zbudowana pod KSZTAŁTY jednego modelu.**
  `gemm_fp8_modular` żąda skompilowanego z wyprzedzeniem kernela na każdą parę
  `(wiersze, kolumny)`; w katalogu stoją dokładnie kształty Bielika. Poza nimi
  zostaje kafel `gemm_fp8`, który obsługuje każdy kształt. Liczba 5535 nie jest
  więc własnością silnika, tylko własnością modelu, dla którego ktoś te kernele
  wpisał — i to samo dotyczy teraz nowej ścieżki.
- **Mieszance i hybrydzie druga postać nie pomogła wcale** (83 i 38 tok/s przed i
  po). Ich wąskim gardłem nie są projekcje gęste. Qwen3-30B: silnik 87,1 tok/s,
  nowa ścieżka 83 — obie ścieżki są tam tak samo wolne, więc to dług całej bazy,
  a nie tej ścieżki. Dla Qwen3.6-35B punktu odniesienia nie ma: silnik przewraca
  się na nim na `CUDA_ERROR_STREAM_CAPTURE_UNSUPPORTED`.

Dwie rzeczy zmierzone przy okazji, obie odwracające założenie:

- **Wyspecjalizowany kernel nie jest szybszy od przenośnego.** `gemm_fp8_modular`
  wymaga skompilowanego z wyprzedzeniem kernela na parę `(wiersze, kolumny)`;
  przenośny `gemm_fp8` obsługuje każdy kształt. Bielik 512 tokenów: 5309 wobec
  5266 tok/s, czyli 0,8%. Cały katalog kształtów kupuje tu tyle co nic, a nowa
  ścieżka nie ma przez to ograniczenia do modeli, których kształty ktoś wpisał.
- **e4m3 kosztuje 3–5% na danych nieustrukturyzowanych i nie maleje z długością
  iloczynu skalarnego** (`format_table.rs`: 5,0% przy K=256, 5,0% przy 512, 3,6%
  przy 1024), a na prawdziwym checkpoincie 0,56%. Różnica jest w DANYCH, nie w
  K: skala na wiersz plus cztery bity wykładnika nie mają czego wykorzystać w
  losowym szumie. Bramka tabeli formatów trzyma więc te cztery formaty do innej
  liczby na kaflu i mówi wprost, że opisuje reżim fikstury, a nie modelu.

`forge-kernels/examples/prefill_bench.rs` mierzy to powtarzalnie: rozgrzewka,
którą się wyrzuca, bo to ona płaci za paczki, potem powtórzenia i mediana.
Liczba z testu poprawności jest ZIMNA i nie nadaje się do porównań (1505 tok/s
na tych samych wagach, dla których ustalona przepustowość to 5230).

### Mieszanka: pętla po tokenach kosztowała ośmiokrotność

Druga postać wagi nie ruszyła mieszanki ani hybrydy, i to był właściwy wynik —
ich prefill nie stał na arytmetyce projekcji, tylko na LICZBIE URUCHOMIEŃ.
`op_moe_ffn` szedł token po tokenie, bo kernele czytające numer eksperta na
urządzeniu biorą jeden wiersz aktywacji: przy prompcie 512 tokenów i ośmiu
ekspertach to `512 · 8 · 3` uruchomień NA WARSTWĘ, każde czytające całą macierz
eksperta dla jednego wiersza. Silnik robi to tak samo i ma tę samą liczbę.

Zamiast tego krok się PRZESTAWIA: wybory sortują się po ekspercie, każdy ekspert
mnoży swój blok wierszy jednym GEMM-em, a odpowiedzi wracają na miejsca swoich
tokenów. Liczba uruchomień przestaje zależeć od liczby tokenów i zaczyna od
liczby ekspertów, a ekspert, którego nikt nie wybrał, nie kosztuje nic — gdzie
wcześniej kosztował raz na każdy token, który go nie wybrał.

Zmierzone, prompt 256 tokenów: **Qwen3-30B-A3B 83 → 685 tok/s** (8,3×, wobec
87,1 silnika, czyli 7,9× silnika) i **Qwen3.6-35B-A3B 38 → 67** (1,8×; tu zostaje
sekwencyjny DeltaNet). Wzorzec: 0,306% / 0,260% dla mieszanki, 0,545% / 0,570%
dla hybrydy.

Trzy rzeczy warte zapamiętania:

- **Nowe kernele prawie nie były potrzebne.** GEMM-y formatów przyjmują
  przesunięcie bajtowe wagi, więc plaster jednego eksperta jest zwykłym
  mnożeniem — wystarczyło przepuścić to przesunięcie przez `gemm_by_kind`.
  Doszedł jeden kernel scalający i rozszerzenie sigmoidu bramki na cały krok.
- **Jedna synchronizacja na warstwę jest ceną, a nie wadą.** Rozmiar bloku
  każdego eksperta jest siatką jego uruchomienia, a siatki nie da się podać na
  urządzeniu — więc wybory wracają na hosta. To `rows·top_k` liczb całkowitych
  wobec dwunastu tysięcy uruchomień, których nie ma.
- **Bramka złapała regres, którego nikt nie szukał.** Wydłużenie promptu
  porównywanego z wzorcem do 20 tokenów — powyżej progu grupowania — pokazało
  2,931% rozjazdu z prawdziwą zamianą w czołówce. Winne nie było grupowanie,
  tylko druga postać wagi w projekcjach DeltaNet: **rekurencja składa swoje
  wejście przez całą sekwencję, więc cztery bity mantysy stracone na wejściu
  wracają pomnożone.** Po ich wyłączeniu 0,545% — a przepustowość 69 wobec 68
  tok/s, czyli e4m3 nie dawało tam NIC. Uwaga takiej dźwigni nie ma, i dlatego ta
  sama wymiana jest tam dobra, a tutaj zła.

Skoro szeroki prompt włącza inną trasę niż wąski, DŁUGOŚĆ PROMPTU W TEŚCIE
ODNIESIENIA JEST CZĘŚCIĄ BRAMKI. Pięciotokenowy prompt zostawiał grupowanie,
wsadowego eksperta współdzielonego i e4m3 w DeltaNet bez żadnej wyroczni —
a wszystkie trzy dawały poprawną angielszczyznę.

### Kontrakt zmienia się na WSZYSTKICH platformach naraz

Rozszerzenie `Op` o lane'y złamało `MetalExec` i nikt tego nie zauważył, bo
ścieżka Metalowa była zabramkowana systemem operacyjnym: jedyna maszyna, która
by to zobaczyła, była tą jedną z Makiem. Przy trzech wykonawcach i celu „to samo
na każdej platformie" taka dziura kosztuje tyle, ile trwa zauważenie.

Dlatego `forge-kernels` ma cechę `metal-check`, która TYPUJE wykonawcę Metalowego
wszędzie — bez backendu HAL-a i bez linkowania frameworków Apple:

```bash
cargo check -p forge-kernels --features metal-check
```

Zmiana `Op`, `Executor` albo `WeightStore` nie jest skończona, dopóki to nie
przechodzi. Nie zastępuje budowy na Macu — kernele MSL kompilują się dopiero
tam — ale łapie całą warstwę rustową, czyli to, co się właśnie zepsuło.
