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

Drugi krok domknął to od strony DEKODOWANIA i od strony kerneli.

Zgrupowanie po ekspercie zostawiło 384 uruchomienia na warstwę — po jednym na
eksperta i projekcję — i każde obejmowało kilkanaście bloków. Profil `nsys`
nazwał to wprost: 74,5% czasu w GEMM-ach ekspertów, po 39 µs przy 100 MFLOP-ach,
czyli 2,6 TFLOPS na karcie robiącej ponad sto. To nie był narzut uruchomienia,
tylko pusta karta. Kernel zgrupowany indeksuje KAFEL przez `block_idx.y`; kafel
mówi, do którego eksperta należy i które wiersze obejmuje, więc jedna siatka
obejmuje wszystkich ekspertów. Wzorzec był już w repozytorium — `triplet_bm64`
scala trzy macierze w jedno uruchomienie — więc to rozszerzenie, nie wynalazek.

Dekodowanie dostało to samo w wariancie wektorowym: `block_idx.y` JEST wyborem,
więc warstwa kosztuje pięć kerneli zamiast pięciu na eksperta.

Zmierzone, GB10, prompt 512, mediana z pięciu po rozgrzewce:

| model | pp512 przed | pp512 po | tg128 przed | tg128 po |
|---|---:|---:|---:|---:|
| Qwen3-30B-A3B | 806 | **1478** | 36,1 | **45,4** |
| Qwen3.6-35B-A3B | 72,9 | 72,5 | 19,9 | **24,7** |

Trzy rzeczy, których nie dałoby się przewidzieć bez pomiaru:

- **Jedna wielka siatka pomaga albo szkodzi, zależnie od RODZINY KERNELI, i to
  przeciwstawnie.** Ten sam kernel puszczony kafel po kaflu daje na Qwen3-30B
  194 tok/s wobec 1473 (siatka wygrywa 7,6×), a na Qwen3.6-35B 68,7 wobec 52,8
  (siatka przegrywa 1,3×). Kafle int8 ogranicza to, ile karty pracuje, więc
  siatka je ratuje; kafel MXFP4 rozpakowuje do f16 i ogranicza go pamięć, a tam
  siatka dotykająca naraz 256 ekspertów traci na lokalności więcej, niż zyskuje
  na zajętości. MXFP4 zostaje więc przy dyspozycji per ekspert — i to jest
  wybór z pomiaru, nie preferencja.
- **`n_tokens` w kaflu int8 jest SKOKIEM, nie granicą.** Skale kwantyzacji
  aktywacji leżą blokowo-głównie (`xd_g[stage * n_tokens + token]`), więc kafel,
  któremu poda się koniec jego bloku, czyta skale cudzego eksperta. Rozdzielone
  na `n_tokens` i `t_end`.
- **Wąski krok też nie miał bramki.** Test hermetyczny mieszanki puszczał jeden
  token, a szeroki krok dopiero model 30B — gdzie błąd wyszedł jako NaN bez
  niczego małego do bisekcji. Po dopisaniu szerokiego kroku do fikstury każdy
  kolejny błąd wychodził w sekundę.

Zmieniła się też sama reguła porównania. „Krok po kroku" trzymałem początkowo
przy wyniku trasy zgrupowanej, czyli przy DRUGIM PRZYBLIŻENIU — a reguła remisu
pyta, czy wzorzec rozdziela zamienioną parę bardziej niż błąd, co nie znaczy nic,
gdy to, co nazywa się wzorcem, samo jest przybliżeniem. Obie trasy są teraz
trzymane wprost przy wzorcu f32.

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

### Cztery bity to nie oszczędność pamięci, tylko podwojenie jednostki

Kontrola możliwości w HAL-u mówiła, że GB10 nie ma blokowo-skalowanego FP4, i
było to nieprawdą. `ptxas` z CUDA 13.0 składa
`mma.sync.aligned.m16n8k64...kind::mxf4nvf4.block_scale` dla `sm_121a` i
`sm_120a`; ta sama linia dla `sm_121` bez sufiksu jest odrzucana. To instrukcje
WŁAŚCIWE ARCHITEKTURZE i sonda, która pytała o `sm_121`, musiała odpowiedzieć
„nie ma" niezależnie od tego, co karta potrafi.

Zmierzone tempo wydawania, operandy w rejestrach, 512 bloków po 128 wątków,
mediana z siedmiu przebiegów:

| instrukcja | ms | TFLOP/s |
|---|---|---|
| `m16n8k16.f16` | 1,126 | 122,0 |
| `m16n8k32.e4m3` | 1,126 | 244,2 |
| `m16n8k64.mxf4` | 1,127 | 488,0 |
| `m16n8k64.mxf4nvf4` | 1,134 | 484,8 |

Każdy rodzaj zajmuje ten sam czas. Jednostka macierzowa oddaje JEDNĄ instrukcję
na takt niezależnie od szerokości elementu, więc przepustowość skaluje się
wyłącznie z `k` — a maszyneria skal blokowych, wraz z dobieraniem czterech bajtów
skali na wiersz, jest darmowa. Cztery bity dają więc czterokrotność kafla f16, na
którym stoi reszta katalogu, i dwukrotność drugiej postaci FP8 z poprzedniej
sekcji.

Oba formaty czterobitowe, które ten projekt już czyta z dysku, trafiają w te
instrukcje bez przepakowania: blok `NVFP4Gguf` to 64 wartości, cztery skale E4M3
i 32 bajty — dokładnie `scale_vec::4X` z `ue4m3`; blok `MXFP4` to 32 wartości i
jedna skala E8M0 — dokładnie `scale_vec::2X` z `ue8m0`.

Układ fragmentów wyprowadzony jest pomiarem, nie założeniem, i ma własną bramkę
(`crates/forge-kernels/tests/mma_fp4.rs`), bo źle ułożony fragment nie zgłasza
błędu, tylko zwraca liczby. Bajtowo jest to układ działającego obok
`m16n8k32.e4m3` z dwiema wartościami na bajt, a selektor `{0, 0}` czyta bajty
0-1 (`2X`) albo 0-3 (`4X`) słowa skali od pasów 0-1 (A) i pasa 0 (B) każdej
czwórki. Porównanie jest DOKŁADNE: każda wartość e2m1 to wielokrotność 0,5 o
module najwyżej 6, więc 64 iloczyny zsumowane w f32 wypadają na liczbach
reprezentowalnych niezależnie od kolejności dodawania.

### Kafel FP4: nie instrukcja go ograniczała, tylko czekanie na pamięć

Pierwszy kafel na `kind::mxf4nvf4` był WOLNIEJSZY od ścieżki f16, którą miał
zastąpić — 0,36-0,69x, przy 20-40 TFLOP/s wobec zmierzonego sufitu 488. PTX nie
miał ani jednego dostępu do pamięci lokalnej, a odczyty LDS wyszły bezkonfliktowe
(adres pasa to `const + 36g + t`, a `36g mod 32 = 4g`, więc `4g + t` przebiega
całe 32 banki). Ograniczeniem były dwie inne rzeczy, obie o ruchu, nie o liczeniu.

PIERWSZA to WIELOKROTNE CZYTANIE KAFLA. Waga jest czytana `tokeny / BTOK` razy,
aktywacja `wiersze / BROWS` razy. Dla 5120x5120 i 2048 tokenów kafel 128x128 to
236 MB wagi plus 236 MB aktywacji, czyli 1,73 ms samego ruchu przy 273 GB/s —
przy zmierzonych 2,75 ms. Poszerzenie kafla tokenów do 256 połowi pierwszy
składnik.

DRUGA to BARIERA. Przy jednym rezydentnym bloku na SM wątki stoją na barierze
przez całą latencję pamięci globalnej, a jednostka macierzowa czeka. Pobranie
NASTĘPNEGO kroku `k` do REJESTRÓW przed policzeniem bieżącego usuwa to czekanie
i jest właściwym rozmiarem tej zmiany: `(BROWS + BTOK) * 9 / THREADS` rejestrów
na wątek, czyli 14 przy BM128/BN256.

Te dwie rzeczy razem dały 38,0 -> 83,5 TFLOP/s na projekcji QKV. Trzecia
własność wypadła z nich obu: `KSTEP` musi być MAŁY, bo pobranie żyje w
rejestrach — przy `KSTEP = 4` akumulator zaczynał lądować w pamięci lokalnej,
a to jest zapaść, nie regresja.

Zmierzone na GB10, mediana z siedmiu, kwantyzacja aktywacji JEST wliczona w czas
FP4 (model, który nie umiałby jej zamortyzować na projekcjach warstwy, i tak
musiałby ją zapłacić):

| kształt | tokeny | f16 ms | fp4 ms | fp4 TFLOP/s | zysk |
|---|---|---|---|---|---|
| qkv 5120x5120 | 128 | 0,159 | 0,106 | 63,1 | 1,49x |
| qkv 5120x5120 | 512 | 0,461 | 0,365 | 73,6 | 1,26x |
| qkv 5120x5120 | 2048 | 1,594 | 1,286 | 83,5 | 1,24x |
| o 5120x4096 | 2048 | 1,307 | 0,984 | 87,3 | 1,33x |
| gate/up 17408x5120 | 2048 | 8,257 | 5,225 | 69,9 | 1,58x |
| down 5120x17408 | 2048 | 7,031 | 5,368 | 68,0 | 1,31x |

87 TFLOP/s to nadal 18% sufitu instrukcji, więc kafel ma jeszcze zapas — ale
ścieżka f16 osiągała 68 z własnych 122, czyli 56%, i to jest liczba, wobec
której FP4 musiało wygrać, żeby w ogóle wejść.

CENĄ jest kwantyzacja aktywacji, której ścieżka FP8 nie płaciła: tam czterema
bitami była tylko waga. Na syntetycznym zestawie o aktywacjach rozrzuconych na
trzy rzędy wielkości wewnątrz tokena kosztuje ona 14,2% rozpiętości wyniku
POJEDYNCZEGO GEMM-u, przy błędzie samego kernela 0,04%. Te dwie liczby mierzy
się osobno i mylenie ich jest tu najłatwiejszym błędem: pierwsza jest własnością
FORMATU i nie zmniejszy jej lepszy kafel. Ile z niej zostaje na prawdziwym
checkpoincie — tak jak przy e4m3, gdzie 3-5% na danych syntetycznych zeszło do
0,56% na Bieliku — jest osobnym pomiarem i warunkiem wpięcia tej ścieżki w model.

### Mieszanka MXFP4: 94% promptu szło w osiem bloków na uruchomienie

Prefill hybrydy Qwen3.6-35B stał na 77,6 tok/s wobec 2609 llama.cpp i pierwsza
hipoteza — sekwencyjna pętla po tokenach w DeltaNecie — była BŁĘDNA. Zwinięcie
jej z ośmiu uruchomień na token do trzech na kawałek dało 72,5 -> 77,6, czyli
siedem procent. Profil powiedział dlaczego: 94,1% czasu siedziało w
`gemm_mxfp4_gguf`, w 43 206 uruchomieniach po 283 us.

Ten model ma 256 ekspertów, 8 wybranych na token, 41 warstw. Mieszanka
uruchamiała JEDEN GEMM NA EKSPERTA NA PROJEKCJĘ — 31 488 uruchomień na prompt,
każde o siatce ośmiu bloków (512 wierszy wyjścia dzielone przez 64) na karcie o
kilkudziesięciu multiprocesorach. Ekspert dostaje średnio szesnaście wierszy z
512-tokenowego promptu, więc każde z tych uruchomień było i małe, i samotne.

Wcześniejszy pomiar mówił, że kafel zgrupowany jest dla MXFP4 GORSZY (55,4 wobec
77,6 tok/s), i to prawda — ale mierzył coś innego, niż się wydawało. Q4_K i Q8_0
mają pod sobą CAŁKOWITOLICZBOWĄ JEDNOSTKĘ MACIERZOWĄ (`gemm_q4_k_i8mma_grouped`),
a MXFP4 i Q6_K rozpakowują do f16 i liczą skalarnie. Porównywane były dwa
kształty tego samego braku, a nie dwa kształty.

MXFP4 to blokowo skalowane cztery bity, czyli dokładnie to, co je
`mma...kind::mxf4`. Bajt skali GGML-a JEST bajtem `ue8m0` instrukcji
(`_e8m0_half(e)` razy tablica MXFP4, która jest dwukrotnością e2m1, daje
`2^(e-127)` razy e2m1), a kodowanie półbajtu jest identyczne z e2m1. Różni je
WYŁĄCZNIE wyrównanie: 17 bajtów nie jest wielokrotnością słowa.

Pierwsza wersja przepakowywała stos ekspertów do drugiej postaci wagi, jak
ścieżka FP8 — 1011 tok/s, ale 17 GiB dodatkowej pamięci na 20-gigabajtowy model,
czyli tyle, że testy odniesienia przestawały się mieścić. Składanie pary bloków
w słowa fragmentu W TRAKCIE WPISYWANIA KAFLA DO PAMIĘCI WSPÓŁDZIELONEJ kosztuje
8,7% przepustowości i ZERO pamięci: 923 tok/s przy tych samych pulach co
poprzednio. To jest właściwa strona tego kompromisu.

pp512 na Qwen3.6-35B: 77,6 -> 923,1 tok/s, czyli 11,9x. Q4_K (Qwen3-30B, 1487)
i gęsty Q4_K (Bielik, 5126) nie zmieniły się, bo tamta ścieżka nie była ruszana.

CENA JEST REALNA I TRZEBA JĄ NAZWAĆ. Instrukcja żąda czterech bitów PO OBU
STRONACH, więc aktywacja ekspertów jest kwantyzowana do MXFP4, czego ścieżka f16
nie robiła. Błąd logitów wobec wzorca f32 rośnie z 0,545% do 0,975% rozpiętości.
Argmax i kolejność czołówki są zachowane, więc bramka przechodzi, ale margines
jest mniejszy niż był. Sam kafel jest tu bez winy — w izolacji trzyma się 0,04%
rozpiętości wobec arytmetyki hosta na tych samych kodach; to jest koszt FORMATU.

Skala UE8M0 nie ma mantysy, więc zaokrąglenie wykładnika w górę marnuje do
jednego bitu z dwóch, które ma e2m1, a w dół — przycina szczyt bloku do 6. Który
kandydat jest lepszy, zależy od rozkładu w bloku, więc wybiera go błąd kwadratowy
policzony dla obu. Bez tego wyboru rozpiętość wychodziła 0,806%, ale czwarte
miejsce czołówki było ZAMIENIONE przy separacji pięciokrotnie większej od błędu —
czyli liczba mniejsza, a wynik gorszy. To jest powód, dla którego bramką jest
kolejność, a nie norma.

### Generacja: router liczył projekcję jednym blokiem

Sufit generacji jest tu policzalny i warto go mieć przed sobą. Qwen3.6-35B czyta
na token 3,258 GB (2,710 GB części gęstej plus 8 z 256 ekspertów), więc 68,15
tok/s llama.cpp to **222 GB/s** — praktycznie tyle, ile ta pamięć oddaje.
Ścieżka gęsta FORGE jest już przy około 217 GB/s; cały dystans siedzi w
mieszance.

`moe_router_f16` liczył iloczyny ekspertów I wybór w JEDNYM BLOKU NA TOKEN. Przy
prefillu bloków jest tyle, ile tokenów, i to jest w porządku. Przy generacji
token jest jeden, więc cała projekcja routera — dla 256 ekspertów i 2048 kanałów
milion bajtów wagi — przechodziła przez jeden multiprocesor. To było 29,5% kroku.

Dwie połówki mają przeciwne kształty, więc są teraz dwoma uruchomieniami:
projekcja jest zwykłym GEMV (albo GEMM przy wielu wierszach), a jednym blokiem
na token jest wyłącznie softmax z top-k. Generacja: 24,4 -> 31,1 tok/s na
hybrydzie i 45,2 -> 58,3 na mieszance Q4_K, gdzie ten sam kernel miał ten sam
kształt.

### GEMV ekspertów MXFP4: trzy hipotezy obalone pomiarem

Po rozdzieleniu routera 48% kroku siedzi w `gemv_mxfp4_f16_gidx`. Izolowany
pomiar tego samego launchera i tego samego kształtu (`examples/moe_gemv_bench`)
stawia sprawę jednoznacznie:

| kształt | MXFP4 | Q4_K | Q6_K |
|---|---|---|---|
| gate/up 512x2048 | 43,8 GB/s | 349,0 | 255,2 |
| down 2048x512 | 38,9 GB/s | 222,9 | 84,1 |

Ten sam launcher, ta sama siatka, te same selekcje — więc kształt wywołania jest
bez winy, a różnica należy do formatu. Trzy wyjaśnienia sprawdzono i KAŻDE
OKAZAŁO SIĘ NIEPRAWDĄ:

1. **Tablica wartości w LDS.** `_dot_lut_block32` robił 32 skalarne odczyty
   wspólne na blok szesnastu bajtów. Zastąpienie ich arytmetycznym
   `_e2m1x8_f16` — tym samym, którym zdjęto tę ścianę w GEMV NVFP4 — dało 6%.
2. **Powtórny odczyt aktywacji.** Każda fala czytała cały wektor z pamięci
   globalnej: 4096 fal razy 4 KiB to 16,8 MB wobec 4,46 MB wagi. Zbuforowanie go
   raz na blok, dokładnie jak robi to Q4_K, dało wynik GORSZY (109 zamiast 102
   us) — aktywacja była już obsługiwana z cache'u, a 16 KiB pamięci
   współdzielonej kosztowało więcej, niż oszczędziło.
3. **Szerokość jednostki pracy.** Q6_K liczy 128 wartości na linię na iterację i
   ma około 7 instrukcji na wartość wobec 29 w MXFP4; przejście na tę samą
   jednostkę dało 36,4 zamiast 43,8 GB/s na gate/up i 12,3 zamiast 38,9 na down,
   bo szeroka jednostka zostawia bezczynne linie, a ten kernel traci na tym
   więcej, niż zyskuje.

Co zostaje niesprawdzone: Q4_K różni się jeszcze JEDNĄ rzeczą — liczy w DP4A na
aktywacji skwantyzowanej do int8, gdzie MXFP4 mnoży w f16. Wartości e2m1
pomnożone przez dwa to dokładnie `{0,±1,±2,±3,±4,±6,±8,±12}`, czyli tablica
MXFP4 GGML-a, więc int8 jest tu DOKŁADNY, nie przybliżony. Przeliczenie półbajtu
na int8 kosztuje `prmt` plus maska znaku; czy to wystarczy, żeby dogonić Q4_K,
jest pytaniem otwartym i pomiar go rozstrzygnie.

### SIEDEMNAŚCIE BAJTÓW: blok MXFP4 rozbijał scalanie dostępów

Po trzech obalonych hipotezach rozstrzygnęły to dwie sondy czytające DOKŁADNIE
TE SAME BAJTY w tej samej siatce, różniące się wyłącznie wyrównaniem odczytu:

| wzorzec dostępu | us | GB/s |
|---|---|---|
| krok 17 bajtów (bloki MXFP4, `17b+1`) | 98,0 | 45,5 |
| krok 16 bajtów, wyrównany | 9,2 | **482,7** |

DZIESIĘĆ RAZY, na samym wyrównaniu. Druga sonda — czytająca bajty i nierobiąca
z nimi nic — zajmowała 98,0 us wobec 101,4 us pełnego kernela, więc
dekwantyzacja kosztowała TRZY PROCENT, a nie połowę, jak sugerowały dwie
pierwsze hipotezy. Blok MXFP4 ma siedemnaście bajtów, czyli linia `l` dostaje
adres `17l+1` — i scalacz dostępów nie ma z czego złożyć transakcji.

Waga wchodzi więc do pamięci współdzielonej KAWAŁKAMI WYRÓWNANYMI DO SZESNASTU
BAJTÓW (od wyrównanego dołu okna, stąd jeden kawałek zapasu), a dopiero stamtąd
jest dekodowana. Blok zaczyna się pod dowolnym bajtem kafla, więc szesnaście
bajtów ładunku składa się z PIĘCIU WYROWNANYCH SŁÓW przesunięciem lejkowym —
odczyt niewyrównany wprost z pamięci współdzielonej schodził do przestrzeni
generycznej (`ld.v4.b32` bez kwalifikatora), co jest i wolne, i dla adresu
niewyrównanego niezdefiniowane.

gate/up 43,8 -> 137,8 GB/s, down 38,7 -> 75,4 GB/s, generacja hybrydy
33,2 -> 44,3 tok/s. Wzorzec f32 zgadza się co do cyfry (0,954% / 1,141% /
0,350%), więc składanie słów jest dokładne.

`down` zostaje przy połowie tempa `gate/up`, bo jego wiersz ma szesnaście bloków
na trzydzieści dwie linie — połowa fali nie ma czego liczyć. To jest następna
rzecz do zrobienia, a nie własność formatu.

### Wybór top-k szedł jednym wątkiem, a wyrównanie nie jest regułą uniwersalną

Osiem rund argmaxa po 256 ekspertach na ZEROWYM WĄTKU to kilka tysięcy iteracji
szeregowych: zmierzone 45,7 us na warstwę, czyli 8,8% kroku generacji. Redukcja
drzewiasta przez cały blok robi z tego osiem rund po dziewięć kroków. Remisy
nadal rozstrzyga niższy indeks — to część porównania ze wzorcem, nie szczegół
implementacji. Generacja: 44,3 -> 47,1 tok/s na hybrydzie i 58,3 -> 60,7 na
mieszance Q4_K.

Ten sam kafel wyrównany zastosowany do Q8_0 jest GORSZY: 45,7 zamiast 47,1.
Blok Q8_0 ma trzydzieści cztery bajty, czyli daje linii adres wyrównany do
DWÓCH, a nie — jak siedemnastobajtowy blok MXFP4 — nieparzysty. Dwa bajty
scalaczowi wystarczają, więc koszt kafla i barier przewyższa zysk. Wyrównanie
jest odpowiedzią na NIEPARZYSTY krok, a nie na każdy krok niebędący potęgą dwójki.

### Krótki wiersz dzieli się na dwie linie, a jednostka macierzowa nie zastąpi wektorowej

`down` miał połowę tempa `gate/up` przy tej samej liczbie bajtów. Pierwsza
hipoteza — że kafel o stałej szerokości ściąga dla niego dwa razy więcej, niż
przeczyta — jest ZMIERZONA I NIEPRAWDZIWA: ograniczenie sztaplowania do
faktycznej liczby bloków dało 73,4 zamiast 75,3 GB/s, bo nadmiarowe kawałki
sąsiednich wierszy i tak leżały w L2. Kosztowało za to `gate/up` 6,5% na
dzieleniu przez wartość nieznaną w czasie kompilacji.

Prawdziwym powodem jest zajętość fali: wiersz `down` ma szesnaście bloków na
trzydzieści dwie linie. Kernel jest związany DEKODOWANIEM, a nie pasmem, więc
bezczynna połowa fali to wprost połowa tempa. Blok idzie teraz na dwie linie po
osiem bajtów kodów każda: 75,3 -> 115,6 GB/s, przy `gate/up` bez zmian.

Kuszące było oddać dekodowanie sprzętowi — blokowo-skalowane FP4 MMA rozpakowuje
`e2m1` samo, a przy prefillu `gemm_mxf4_grouped` przenosi te same bajty ekspertów
z prędkością 199 GB/s wobec 126 GB/s formy wektorowej. Przy kroku to NIE DZIAŁA i
różnica jest ilościowa, nie jakościowa: osiem selekcji to osiem kafli, a przy
BM128 daje to trzydzieści dwa bloki na całą kartę. Zmierzone 222 us na wywołanie
wobec 35 us formy wektorowej, czyli 20 GB/s; generacja spadła z 49,3 do 26,4
tok/s. Tamte 199 GB/s pochodzą z wywołania obejmującego wszystkich 256 ekspertów
i nie przenoszą się na osiem. Kafel macierzowy potrzebuje pracy w wymiarze
tokenów, a krok jej nie ma.

### Prefill: sufit to 98 GB/s, a nie 222, i wąskim gardłem jest kafel zgrupowany

Prefill 512 tokenów po ośmiu ekspertach ze stu dwudziestu ośmiu (albo ośmiu z
256) dotyka PRAKTYCZNIE KAŻDEGO eksperta, więc musi przeczytać cały plik: 18 GB
dla Qwen3-30B Q4_K i 19 GB dla Qwen3.6-35B MXFP4. To jest miara, w której
trzeba czytać wynik. llama.cpp robi oba w 183 i 194 ms, czyli oba przy 98 GB/s
— nie przy suficie pamięci. My jesteśmy przy 78 GB/s na hybrydzie i 57 GB/s na
mieszance, i cała różnica siedzi w kaflu zgrupowanym MoE.

Dwa pomiary z tej ścieżki, oba zaskakujące w przeciwną stronę:

Kwantyzacja aktywacji do MXFP4 była 12% prefillu, choć czyta 21 MB. Czytała
blok trzema przebiegami po trzydzieści dwa ładunki SKALARNE, a sąsiednie linie
fali dzieliły trzydzieści dwie wartości, więc każdy ładunek ciągnął własny
sektor: 30 GB/s. Jeden ładunek wektorowy na blok plus rekonstrukcja e2m1
składana z bitów zamiast czytana z tablicy (indeks z rejestru ląduje w pamięci
lokalnej) dały 443 -> 83 us na wywołanie i prefill 1790 -> 1974 tok/s.

Wyrównanie odczytu wagi, które w formie wektorowej dało dziesięciokrotność, w
kaflu dało 0,8%. Kafel czyta z każdego wiersza tylko `34 * KSTEP` bajtów, a
wiersze dzieli cały `blocks_per_row * 34`, więc kosztuje go GŁĘBOKOŚĆ
PRZEJŚCIA, a nie wyrównanie: dwa bloki `k` zamiast jednego dały 1989 -> 2119
tok/s, a cztery — 1904, bo trzydzieści sześć rejestrów zapowiedzi zabiera
zajętość. Optimum jest wewnątrz i trzeba je znaleźć pomiarem.

Ta sama diagnoza czeka na `gemm_i8mma_grouped` (63% prefillu mieszanki, 66
GB/s): przy `BN=64` i czterech liniach na wiersz jedna linia sztapluje CZTERY I
PÓŁ BAJTA Q4_K na przejście. Głębsze przejście wymaga tam przebudowy
podwójnego buforowania, więc nie jest zmianą jednego parametru.

### Wiązaniem kafla zgrupowanego były REJESTRY ZAPOWIEDZI, a nie pasmo

Kafel MXFP4 wydawał 12,6 TFLOP/s przy suficie instrukcji 488 i przenosił wagi
ekspertów z prędkością 122 GB/s przy 215 GB/s, które ta sama karta osiąga w
generacji na gęstym Q8_0. Liczył go blok CZTERECH OSNÓW, w którym każda linia
trzymała osiemnaście słów zapowiedzi kolejnego kafla.

`WARPS_TOK` dzieli kolumnę tokenów między osnowy, więc kształt kafla i jego
arytmetyka zostają te same — zmienia się tylko, ile linii go liczy, a przez to
ile rejestrów zapowiedzi przypada na jedną. Prefill 512 tokenów:

    4 osnowy, k1   1989 tok/s        8 osnów,  k2   2245
    4 osnowy, k2   2119              16 osnów, k2   2426
    4 osnowy, k4   1904              16 osnów, k4   2358
                                     32 osnowy, k2  2470

Widać na tym, że dwie dźwignie są SPRZĘŻONE: przy czterech osnowach głębsze
przejście przegrywa (k4 gorsze od k2), a przy szesnastu nadal przegrywa, bo
zapowiedź wraca do rejestrów. Kafel skończył przy 182 GB/s.

To samo zastosowane do `gemm_q4_k_i8mma_grouped` NIE DZIAŁA i powód jest w jego
sztaplowaniu: `W_ROWS_PER_PASS = NTHREADS / 4` wiąże liczbę wątków z `BN`, więc
szesnastu osnów nie da się dołożyć bez rozszerzenia kafla do `BN=128`, a to
połowi liczbę bloków. Zmierzone 1365 zamiast 1613 tok/s. Ten kafel wymaga
przepisania mapy sztaplowania, nie zmiany parametru.

### Kafel MXFP4 gubił wiersze gorących ekspertów

Tablica kafli zgrupowanych powstawała krokiem `GROUPED_TILE_ROWS = 64`, a kafel
MXFP4 liczy TRZYDZIEŚCI DWA tokeny i nie pętli po nich — pisze swoją szerokość
od `tile_first` i kończy. Blok eksperta dłuższy niż trzydzieści dwa wiersze
tracił ogon: nikt go nie liczył, a `moe_combine` wkładał tokenom to, co akurat
zostało w scratchu. Przy 512 tokenach po ośmiu ekspertach z 256 średnia to
szesnaście wierszy na eksperta, więc dotyczyło to wyłącznie gorących — i
dlatego bramka wzorca, na krótszym prompcie, tego nie łapała.

Krok tablicy jest teraz WYNIKIEM wybranego kafla (`grouped_tile_rows`), a nie
stałą, i bierze się minimum z trzech projekcji warstwy — bo Q4_K_M daje sześć
bitów na `ffn_down` i cztery na pozostałe dwie, więc formaty w jednej warstwie
nie muszą być te same.

Poprawka KOSZTUJE, i to jest uczciwa cena za policzenie tego, co było pomijane:
2510 -> 2343 tok/s. Gorący ekspert dzieli się teraz na dwa kafle, a każdy czyta
całą wagę swoich wierszy. Szerszy kafel usunąłby to podwójne czytanie, ale
liczyłby dopełnienie dla zimnych ekspertów, których jest większość — zmierzone
2181 tok/s przy kaflu 64-tokenowym. Wąski kafel z pasującym krokiem wygrywa.

### Dlaczego gęsty wygrywa, a mieszanka nie — to są dwa różne reżimy

Gęsty prefill czyta wagę RAZ i używa jej dla 512 tokenów, więc na element wagi
przypada 1024 operacje: jest związany JEDNOSTKĄ MACIERZOWĄ. Tam wygrywamy o
1,64x i wygrywamy właśnie kaflami int8/FP4.

Mieszanka rozdaje te same 512 tokenów po ośmiu ekspertach ze 128 albo 256, więc
na eksperta przypada kilkanaście wierszy, a intensywność arytmetyczna spada
około trzydziestokrotnie. Reżim przeskakuje na PASMO PAMIĘCI, i tam lepsza
jednostka macierzowa nie kupuje nic — liczy się wyłącznie to, jak szybko
osiemnaście gigabajtów przechodzi przez magistralę. Dlatego z pracy nad gęstym
przenosi się zajętość, sztaplowanie i wyrównanie, a nie sama arytmetyka.

### Co robi llama.cpp w MMQ, czego nie robimy my

Przeczytane z `ggml/src/ggml-cuda/mmq*.cuh` na masterze (91d2fc387). Sześć
rzeczy, z których pięć jest u nas nieobecnych.

**Instrukcja jest ta sama.** `mma.cuh` woła
`mma.sync.aligned.kind::mxf4.block_scale.scale_vec::2X.m16n8k64.row.col.f32.e2m1.e2m1.f32.ue8m0`
oraz wariant `mxf4nvf4` ze skalami `ue4m3` — dokładnie to, czym liczy nasz
kafel FP4. Na MXFP4 nie przegrywamy sprzętem.

**Iteracja `k` obejmuje CAŁY superblok, nie trzydzieści dwie kolumny.**
`MMQ_ITER_K = 256`, a dla FP4 `MMQ_ITER_K_FP4 = 512`. Stąd
`threads_per_row = MMQ_ITER_K / (4 * QR4_K) = 32`: jedna pełna fala czyta 128
bajtów `qs` jednego wiersza w JEDNYM scalonym dostępie i z tego samego odczytu
rozpakowuje OBA półbajty do pamięci współdzielonej. Nasze sztaplowanie bierze
cztery linie na wiersz po osiem bajtów i powtarza to osiem razy na superblok.

**Kernel jest kompilowany OSOBNO dla każdej szerokości tokenów.** `J` przyjmuje
8, 16, 24, ... 128, a `mul_mat_q_switch_J` wybiera tę, która daje najmniej
kafli. To jest dokładnie ten kompromis, który dziś zmierzyłem — kafel 32-tokenowy
2343 tok/s wobec 64-tokenowego 2181 — tyle że oni go nie zawierają, tylko
specjalizują.

**`occupancy = 1`.** Jeden blok na multiprocesor z wielkim kaflem w pamięci
współdzielonej (I=128 wierszy na 256 kolumn `k`). To inny punkt projektowy niż
nasz: my stroiliśmy w stronę wielu małych bloków.

**Dekompozycja stream-K** (`stream_k = true` dla Q4_K i MXFP4). Blok bierze
ciągły wycinek spłaszczonej przestrzeni pracy `(i, j, k)`, liczy wynik
częściowy, a osobne przejście `fixup` je składa. To jest lekarstwo na nierówne
bloki ekspertów — i wprost na naszą zapaść przy kroku generacji, gdzie osiem
selekcji dawało trzydzieści dwa bloki i 222 us zamiast 35 us.

**Tablicy kafli nie ma w ogóle.** Ścieżka MMQ czyta `expert_bounds` (sumy
prefiksowe) NA URZĄDZENIU, a blok, dla którego `jt*J >= col_diff`, po prostu
wraca. Nasza `moe_grouped` buduje tablicę na hoście i płaci za to synchronizacją
na warstwę — a to właśnie sprzężenie kroku tablicy z szerokością kafla było
źródłem dzisiejszego błędu gubionych wierszy.

### Zmierzone wiązanie kafla zgrupowanego Q4_K: linia L2, a nie kształt

Strojenie kształtu tego kafla jest WYCZERPANE — pięć wariantów, każdy gorszy od
obecnego `[64, 64, 8]` (1639 tok/s): `[64,128,8]` 1593, `[64,128,16]` 1582,
`[32,128,16]` 1365, `[32,128,8]` 1236. To, co na kaflu MXFP4 dało +24%, tu nie
daje nic, i Nsight Compute mówi dlaczego.

Jedno uruchomienie (`--set detailed` plus liczniki bajtów):

    Memory Throughput   79%       Compute (SM)        31%
    L1/TEX Hit Rate     77,5%     L2 Hit Rate         92,8%
    Rejestry/wątek      84        Block Limit Registers  2
                                  Block Limit Shared Mem 5
    Zajętość osiągnięta 33%       spill lokalny          0

    l1tex__t_bytes                                    2,05 GB
    lts__t_bytes                                      1,65 GB
    l1tex__t_sectors_pipe_lsu_mem_global_op_ld  10 692 480 (342 MB)

Trzysta czterdzieści dwa megabajty żądań globalnych wobec 1,65 GB ruchu L2 to
4,8x wzmocnienia. Powód jest w mapie sztaplowania: wątek czyta OSIEM BAJTÓW,
czwórka wątków pokrywa trzydzieści dwa bajty wiersza i skacze o `blocks_per_row
* 144` dalej. Każdy taki dostęp dotyka jednego sektora, ale ciągnie linię L2, z
której zużywamy ćwiartkę. llama.cpp czyta 128 bajtów wiersza PEŁNĄ FALĄ
(`threads_per_row = MMQ_ITER_K / (4 * QR4_K) = 32`) — jedna linia, w całości
zużyta.

Zajętość jest przy tym związana REJESTRAMI (dwa bloki, choć pamięć współdzielona
pozwoliłaby na pięć), ale to wiązanie drugorzędne: przy 79% przepustowości
pamięci i 31% obliczeń więcej bloków tylko mocniej dobijałoby L2.

Wniosek jest jednoznaczny i zgodny z tym, co robi MMQ: trzeba POSZERZYĆ ITERACJĘ
`k` do całego superbloku, żeby czwórka wątków czytała 128 ciągłych bajtów. Przy
etapie 32-kolumnowym jest to niemożliwe — wątek `part` musiałby trzymać dane
etapu, którego akurat nie liczy.

### Pomiary sprzed i po restarcie maszyny NIE SĄ porównywalne

Restart (włączenie liczników) przesunął oba silniki naraz. Ta sama komenda,
ten sam plik, przed i po:

                        llama.cpp pp512   llama.cpp tg32
    Qwen3.6-35B MXFP4    2639,9 -> 2541,9   68,15 -> 63,96
    Qwen3-30B Q4_K       2804,1 -> 2307,3   91,47 -> 86,05
    Bielik 7B Q4_K       3135,0 -> 2642,9   42,72 -> 45,44

Dlatego każde porównanie musi pochodzić z JEDNEGO stanu maszyny. Baseline
odniesienia to teraz `/opt/repos/llama.cpp` 3ef2369 (2026-05-28, ma FP4 MMA).

### Szerokość etapu, a nie szerokość odczytu, była wiązaniem kafla int8

Poszerzenie samego ODCZYTU wagi do 128 ciągłych bajtów nic nie dało: czas 2,29
ms bez zmian, a ruch L2 stanął na 1,64 GB. Licznik sektorów poszedł nawet w
górę (10,7 M -> 18,7 M), bo 32-bajtowy odczyt pod adresem podzielnym przez 16
dotyka dwóch sektorów tam, gdzie ośmiobajtowy dotykał jednego. Wniosek z
poprzedniej sekcji był więc BŁĘDNY w części przyczynowej: L1 już wcześniej
pochłaniał krótkie odczyty chunków i to nie one generowały ruch do L2.

Właściwą przyczynę pokazały dopiero liczniki przestojów. Jednostka macierzowa
pracowała na 6,06% mocy przy 19% aktywności wydawania, a przestoje rozkładały
się tak (instrukcji na wydanie): `long_scoreboard` 3,93, `barrier` 3,65,
`mio_throttle` 2,90, `short_scoreboard` 1,76. Kafel nie był głodny pasma ani
arytmetyki — był głodny PRACY MIĘDZY SYNCHRONIZACJAMI. Przy jednym podbloku na
etap przechodził barierę co 32 kolumny.

llama.cpp stawia CZTERY bariery na 256 kolumn (`MMQ_ITER_K = 256`, kafel wagi
sztaplowany raz na superblok, kafel aktywacji dwa razy po 128 kolumn) i NIE MA
podwójnego buforowania. My mieliśmy osiem barier na te same 256 kolumn.

Etap ma teraz cztery podbloki (128 kolumn) i jeden bufor: dwie bariery na etap,
czyli 32 na 2048 kolumn zamiast 64, przy tym samym budżecie 48 KiB pamięci
współdzielonej i dwóch blokach na SM. Q4_K wchodzi w to naturalnie, bo
superblok ma 144 CIĄGŁE bajty i ośmiu podblokom wystarczy jeden nagłówek —
czwórka wątków bierze po ćwiartce. Q8_0 zostaje przy jednym podbloku: jego blok
ma 34 bajty i własną skalę, więc nie ma czego poszerzać.

Pomiar: mieszanka 1638 -> 2227 tok/s (+36%), czas kernela 2,29 -> 1,34 ms, ruch
L2 1,64 GB -> 989 MB, jednostka macierzowa 6,06% -> 10,46%. Bramka referencyjna
0,310% rozpiętości, ten sam argmax.

Po tej zmianie czołowym przestojem stał się `mio_throttle` (6,04) — kolejka
przed potokiem pamięci współdzielonej. Dwanaście z szesnastu odczytów na etap
było KSIĘGOWANIEM SKAL, nie danymi. Skale trzymane PARAMI — `(d, d*suma)` na
token, `(d*skala, dmin*min)` na wiersz — zbijają to o połowę (10,69 M -> 5,35 M
odczytów, `mio_throttle` 6,04 -> 1,58). Czas się nie zmienił, bo zysk zjadła
zajętość: pary kosztowały 33 rejestry i drugi blok na SM. Zwinięcie pętli
podbloków odzyskuje 16 z nich, ale mierzy się gorzej (2219 wobec 2236) —
rozwinięcie zostaje.

### Dekod: 15,7% czasu karta stoi, a same GEMV-y są blisko sufitu

Osobne rozbicie kroku dekodu (`nsys`, Qwen3-30B Q4_K, 32 kroki) mówi coś
innego, niż mówił prefill. Przepustowości pojedynczych kerneli są DOBRE:

    MoE gate/up Q4_K   96x8    3,51 ms/token   194 GB/s
    MoE down Q6_K     256x8    1,97 ms/token   126 GB/s
    MoE down Q4_K     256x8    0,96 ms/token   177 GB/s
    lm_head Q6_K    18992x1    1,48 ms/token   173 GB/s
    uwaga Q4_K        256x1 i 384x1            144-175 GB/s

Sufit strumienia na tej karcie to około 215 GB/s, więc gate/up jest na 90% —
tam nie ma czego szukać. Za to KROK JAKO CAŁOŚĆ osiąga ~110 GB/s, bo:

  * 15,7% czasu ściany to PRZERWY MIĘDZY KERNELAMI. 29 295 przerw, mediana
    2,24 us, p99 3,01 us, maksimum 14,8 us — rozkład jest płaski, więc to nie
    są synchronizacje, tylko stały koszt uruchomienia. `cuLaunchKernel`
    kosztuje 2,35 us po stronie CPU przy 39 660 wywołaniach.
  * krok emituje ~860 uruchomień na token (143 rmsnorm, 240 GEMV, 72
    `cast_f16_f32`, 71 `residual_add`, 48 `silu_mul`, 48 topk, 48 combine...).

Stąd dwa kolejne kroki, w tej kolejności wartości: GRAFY CUDA dla kroku dekodu
(HAL ma `begin_capture`/`end_capture`/`ExecGraph`, używa ich `forge-engine`, a
ścieżka `CudaExec` — czyli ta docelowa — nie używa ich wcale) i dopiero potem
zrastanie drobnych kerneli. Warunkiem przechwycenia jest, żeby pozycja w
sekwencji i adresy stron KV szły przez BUFOR URZĄDZENIA, a nie przez argument
uruchomienia; inaczej graf trzeba by przechwytywać co krok.

Q6_K na `ffn_down` (126 GB/s przy 177 GB/s wariantu Q4_K tego samego kształtu)
to osobny, mniejszy dług — 0,57 ms na token.

### Stan wobec llama.cpp, jeden stan maszyny, 2026-08-07

`/opt/repos/llama.cpp` 6db1304 wobec FORGE `58e5a887`:

                        llama pp512  FORGE pp512   llama tg32  FORGE tg32
    Qwen3.6-35B MXFP4      2521,9      2337,8         62,86      54,9
    Qwen3-30B Q4_K         2296,9      2239,0         82,30      63,0
    Bielik 7B Q4_K         2625,1      4867,0         43,05      36,2

Prefill mieszanki przeszedł z 0,71x na 0,975x. Dekod stoi na 0,77-0,87x we
wszystkich trzech i to jest teraz największy dług.

### Krok dekodu nagrany raz i odtwarzany

Dwie zmiany, i pierwsza jest warunkiem drugiej.

**Sztaplowanie bez drenażu.** Zapis HAL-a ląduje na strumieniu zastanym, a nasz
jest nieblokujący, więc nie jest uporządkowany względem tego, co już stoi w
kolejce. Dotąd rozwiązywał to pełny `synchronize` przed każdym zapisem — cztery
opróżnienia potoku na krok dekodu. Teraz każdy z pięciu buforów sterujących ma
przypięte lustro, a kopia idzie NA TYM STRUMIENIU: jest uporządkowana z
konstrukcji i jest węzłem grafu, a nie wywołaniem hosta. Zysk sam w sobie
niewielki (+0,6% mieszanka, +1% gęsty), ale bez tego nie da się nic przechwycić.

**Nagranie.** `Executor` dostał `run_step(&[Op])` z domyślną implementacją
„po kolei"; model buduje listę operacji kroku i oddaje ją w całości. `CudaExec`
nadpisuje to nagraniem: pierwszy krok danego kształtu wykonuje się zwyczajnie
(to on tworzy tablice ekspertów, scratch mieszanki i spakowane formy wag),
drugi jest nagrywany, każdy kolejny to jedno uruchomienie grafu.

Trzy warunki i miejsce, gdzie każdy jest egzekwowany:

  * Ciąg operacji musi się powtarzać — klucz to liczba lane'ów kroku
    jednotokenowego. Kafel promptu nigdy nie jest nagrywany.
  * Krok z lane'em na POZYCJI ZERO nie jest ani nagrywany, ani odtwarzany.
    Sekwencja, która się zaczyna, oddaje strony i zeruje stan rekurencyjny —
    to inny ciąg operacji. Nagranie go zerowałoby stan co token, a odtworzenie
    zwykłego w jego miejsce nie zerowałoby nigdy. Bramka hybrydowa złapała to
    natychmiast (`CUDA_ERROR_STREAM_CAPTURE_UNSUPPORTED` na zapisie zerującym).
  * Nagranie NAZYWA bufory. Scratch mieszanki i mixera rekurencyjnego jest
    wymieniany, gdy przyjdzie szerszy krok, więc wymiana kasuje nagrania.

Ten ostatni błąd jest UTAJONY i dlatego test trzyma go licznikiem nagrań, a nie
tokenami: zwolniony obszar zostaje zmapowany, więc nieaktualne nagranie czyta i
pisze do własnego, już nieswojego scratcha i odpowiada poprawnie — do chwili,
gdy tę pamięć dostanie ktoś inny. Sprawdzone: bez kasowania test pada, z nim
przechodzi (`a_recorded_step_does_not_outlive_the_buffers_it_names`).

Pomiar. Bezczynność GPU między kernelami spadła z 15,7% na 4,6%, mediana
przerwy z 2,24 us na 0,10 us (`nsys --cuda-graph-trace=node`). Dekod:
mieszanka 63,0 -> 71,7 tok/s (+13,8%), hybryda 55,1 -> 61,9 (+12,3%), gęsty
36,2 -> 37,4 (+3,3%). Gęsty zyskuje najmniej, bo ma najmniej uruchomień na
token — nie ma mieszanki.

### Stan wobec llama.cpp, jeden stan maszyny, 2026-08-07 (po grafach)

`/opt/repos/llama.cpp` 6db1304 wobec FORGE `64cbbc64`:

                        llama pp512  FORGE pp512   llama tg32  FORGE tg32
    Qwen3.6-35B MXFP4      2513,5      2319,2         62,21      61,9
    Qwen3-30B Q4_K         2301,3      2233,1         81,84      71,7
    Bielik 7B Q4_K         2653,8      5043,8         42,42      37,4

Hybryda zrównała się w dekodzie (0,995x). Zostaje dekod mieszanki (0,88x) i
gęstego (0,88x) oraz prefill mieszanki (0,97x) i hybrydy (0,92x).

### Dekod: co czytanie kodu llama.cpp dało wprost

Trzy zmiany, wszystkie wzięte z porównania z ich kernelami.

**Q6_K ekspertów zszedł na ścieżkę całkowitą.** Q4_K_M dzieli JEDNEGO eksperta
między dwa formaty: sześć bitów na `ffn_down`, cztery na gate i up. Ścieżka
adresowana na urządzeniu (`_gidx`) istniała tylko dla czterech bitów, więc
połowa projekcji `down` szła wariantem f16, który dekwantyzuje superblok przed
mnożeniem — zmierzone 126 GB/s wobec 179 dla czterobitowej połowy TEGO SAMEGO
kształtu, przy suficie strumienia około 215. Doszedł
`gemv_q6_k_dp4a_f16_gidx` (+ wariant wsadowy), matematyka to istniejący
`_dot_q6k_i8`.

**Gate i up mnożą się razem.** llama.cpp zrasta `{MUL_MAT_ID, MUL_MAT_ID, GLU}`
w jeden kernel; my mieliśmy trzy uruchomienia. Osobno KAŻDY blok kwantyzował tę
samą aktywację do pamięci współdzielonej dwa razy — 3,1 MB sztaplowanych
odczytów na 7,1 MB wagi w uruchomieniu, czyli jedna trzecia ruchu była połową,
która nie musiała się wydarzyć. Razem: sztaplowanie raz, oba wyniki spotykają
się w rejestrach, a bramka SiLU jest epilogiem zamiast osobnego uruchomienia.
Dla gęstego FFN taki kernel już istniał (`gemv_norm_silu_q4_k_dp4a_f16`) —
brakowało wariantu z tablicą ekspertów, więc `_dot2_q4k_i8`/`_dot2_q6k_i8`
przyjmują teraz DWA wskaźniki bazowe (gęsty stos podaje ten sam dwa razy).

**`rmsnorm_f16` dostał rozwinięcie, które miał już wariant rezydualny.** Przy
dekodzie liczy JEDEN wiersz kilku kilobajtów na jednym multiprocesorze, więc
jest to opóźnienie, nie pasmo; jedyną pozostałą równoległością jest liczba
odczytów, które wątek trzyma w locie.

Pomiar dekodu: mieszanka 71,7 -> 78,0 tok/s, hybryda 61,9 -> 62,4, gęsty
37,4 -> 37,7. Rozrzut wobec wzorca hostowego nawet się poprawił (krok
0,260% -> 0,240%), bo dp4a liczy iloczyny dokładnie zamiast przez f16.

### Stan wobec llama.cpp, jeden stan maszyny, 2026-08-07 (po zrostach)

`/opt/repos/llama.cpp` 6db1304 wobec FORGE `d6a30f0`:

                        llama pp512  FORGE pp512   llama tg32  FORGE tg32
    Qwen3.6-35B MXFP4      2492,8      2322,8         64,10      62,4
    Qwen3-30B Q4_K         2272,7      2239,2         81,28      78,0
    Bielik 7B Q4_K         2630,8      5147,5         41,48      37,7

Licząc od początku dnia: prefill mieszanki 0,71x -> 0,985x, dekod mieszanki
0,77x -> 0,96x, dekod hybrydy 0,87x -> 0,97x, gęsty prefill 1,82x -> 1,96x.
Zostaje prefill hybrydy (0,93x) i dekod gęstego (0,91x).

### Dekod gęstego: co zmierzono i co z tego wyszło

Trzy hipotezy, dwie obalone, jedna częściowa — zapisane, bo każda kosztowała
przebieg i żadnej nie warto sprawdzać drugi raz.

**`PERSIST_GRID = 384` NIE jest strojeniem pod tę kartę, ale też nie szkodzi.**
Stała nosi komentarz „zmierzone na R9700" — innej karcie, o innej liczbie
multiprocesorów. Na GB10 (48 SM) 384 bloków po 256 wątków to 64 warpy na SM
przy limicie 48, więc trwałość siatki wygląda na pozorną. Przemiatanie
96/144/192/240/288/384 dało 37,1-37,9 tok/s: PŁASKO. Hipoteza obalona,
stała zostaje.

**Wąskie sztaplowanie nie podniosło zajętości.** Kernel rezerwował 20 KiB
pamięci współdzielonej (16 KiB `X_MAX` int8 + skale) niezależnie od tego, że
Bielik potrzebuje 5 KiB. Wariant parametryzowany pojemnością zszedł do 5120 B —
a zajętość nie drgnęła (58,4% -> 56,9%), bo ogranicza ją REJESTR (56 na wątek,
cztery bloki na SM), nie pamięć współdzielona. Wariant zostaje, bo jest
strukturalnie właściwy, ale sam z siebie nic nie dał.

**Rozwinięcie pętli superbloków dało 2,9%.** Profiler naliczył temu iloczynowi
53,14 zatrzymanych instrukcji na wydanie w `long_scoreboard` przy 11,8%
aktywności wydawania: dekodowy GEMV nie jest głodny pasma, jest głodny ŻĄDAŃ W
LOCIE, a jeden superblok naraz trzyma ich cztery-pięć na warp. `DOT_UNROLL = 4`
wydaje wszystkie odczyty czterech superbloków przed konsumpcją któregokolwiek;
kolejność akumulacji jest zachowana, więc wynik jest bitowo ten sam.
`long_scoreboard` spadł 53,14 -> 46,22, czas kernela 65,1 -> 62,3 us, dekod
gęstego 37,7 -> 38,7 tok/s. Kompilator NIE utrzymał pełnej czwórki w rejestrach
(56 rejestrów bez zmian), więc zysk jest częściowy — reszta wymagałaby
przepisania iloczynu na dekompozycję llama.cpp (dwa warpy na wiersz,
`blocks_per_iter` obejmujące kilka superbloków).

### Stan wobec llama.cpp, jeden stan maszyny, koniec dnia 2026-08-07

`/opt/repos/llama.cpp` 6db1304 wobec FORGE `9de9d70`:

                        llama pp512  FORGE pp512   llama tg32  FORGE tg32
    Qwen3.6-35B MXFP4      2510,8      2322,9         61,73      62,1
    Qwen3-30B Q4_K         2282,6      2253,4         82,59      79,0
    Bielik 7B Q4_K         2636,8      5217,9         41,48      39,0

Hybryda WYPRZEDZA w dekodzie (1,006x), gęsty prefill 1,98x. Zostaje prefill
hybrydy (0,93x), dekod mieszanki (0,96x) i dekod gęstego (0,94x) — a dekod obu
jest ograniczony odczytem wag, więc dalsza przewaga wymaga wyższej sprawności
kernela, nie mniejszej liczby bajtów.

## Dekodowy iloczyn Q4_K: szerokość ŻĄDANIA, nie liczba warpów

Poprzednia sekcja kończyła się wnioskiem, że dekod jest ograniczony odczytem
wag i że dalsza przewaga wymaga wyższej sprawności kernela. Wymagała — i
sprawność wzięła się z jednej wielkości, o którą wcześniej nikt nie zapytał:
ILE BAJTÓW BIERZE JEDNO ŻĄDANIE.

**Najpierw pomiar sufitu.** Zwykły kernel przemiatający 4 GiB odczytami
`float4` osiąga na GB10 237 GB/s (192 bloki: 237,4; 4096 bloków: 232,6 — czyli
sufit jest płaski i osiąga się go garścią bloków). Wobec tego dekod llama.cpp
szedł 193 GB/s, a nasz 176. Obaj daleko od sufitu, więc nie było to „obie
implementacje stoją na pamięci".

**Zajętość nie była wąskim gardłem.** `ncu` na naszym GEMV: zajętość
teoretyczna 75%, osiągnięta 46,55%, 97,8% z 46,9 cykli między wydaniami na
`long_scoreboard`. Siatka trwała 288 bloków po 128 wątków to 6 bloków na SM
przy 9 mieszczących się — więc warpów było mniej, niż karta trzyma. Dołożenie
ich POGORSZYŁO wynik monotonicznie (96 bloków: 41,5 tok/s; 288: 40,6; 480:
38,2). Warp tego kernela czyta DŁUGI ciągły odcinek jednej macierzy; więcej
warpów to krótsze odcinki, nie więcej pasma.

**Wąskim gardłem była szerokość odczytu.** `vec_dot_q4_K_q8_1` daje lane'owi
dwa słowa int32 oddalone o szesnaście bajtów — jedna instrukcja warpa dotyka
czterech rozłącznych odcinków superbloku i rusza cztery bajty na lane. Nowe
odwzorowanie: osiem lane'ów na superblok, SZESNAŚCIE KOLEJNYCH BAJTÓW na lane,
cztery superbloki na warp. Te same bajty, ta sama arytmetyka, ćwierć żądań.
Gęsty dekod 39,2 -> 43,3 tok/s, mieszanka 79,0 -> 84,3.

Q6_K tego nie dostanie bez przepakowania: superblok ma 210 bajtów, więc
przesunięcie kolejnego jest podzielne tylko przez 2 i wektorowy odczyt
16-bajtowy nie ma jak być wyrównany. Tam zostaje rozwinięcie po superblokach.

### Stan wobec llama.cpp, jeden stan maszyny, 2026-08-07 wieczorem

`/opt/repos/llama.cpp` 6db1304 wobec FORGE `d2540c894`:

                        llama pp512  FORGE pp512   llama tg32  FORGE tg32
    Qwen3.6-35B MXFP4      2502,5      2326,4         62,52      62,3
    Qwen3-30B Q4_K         2273,6      2280,7         82,23      84,3
    Bielik 7B Q4_K         2650,4      5135,2         42,59      43,3

Cztery z sześciu pomiarów po naszej stronie, jeden remis (dekod hybrydy,
0,996x), jeden zostaje: prefill hybrydy 0,93x.

## Kontrakt lane'ów trzyma się logitów, nie szczęśliwego promptu

`cuda_lanes_match_solo_runs` porównywał CIĄG TOKENÓW przebiegu wsadowego z
ciągiem przebiegu samotnego i żądał równości co do identyfikatora. Obie drogi
liczą INNYM kernelem — komentarz w samym teście to mówił — więc różnią się
zaokrągleniem, a przy porównaniu ciągów jeden remis rozjeżdża całą resztę
przebiegu. Zmierzone: na wersji, na której test przechodził, zmiana JEDNEGO
identyfikatora w promptach wystarczała, żeby przestał. Test mówił więc o tym,
czy dobrano szczęśliwy prompt, a nie o adresowaniu lane'ów.

Teraz wsad dostaje na wejściu token przebiegu samotnego (każdy krok liczy się z
tego samego stanu, więc różnica może być tylko zaokrągleniem TEGO kroku), a
porównywane są logity regułą remisu tej samej postaci co `common::agrees` —
z tą różnicą, że pierwsze miejsce też jej podlega, bo tu obie strony są nasze i
żadna nie jest wzorcem. Zgodne lane'y dają 1,6% rozpiętości, podstawiony cudzy
token daje 14,7%: reguła nie jest poluzowaniem, tylko właściwą miarą.

## Prefill hybrydy: trzy ślepe uliczki i jedna prawdziwa ściana

Prefill hybrydy (0,93x wobec llama.cpp) rozkłada się tak (`nsys`, pp512,
`--cuda-graph-trace=node`): **48% w `gemm_mxf4_grouped`** (240 uruchomień,
mediana 888 us), 17,6% w `gemm_i8mma`, 9,7% w skanie DeltaNet. Poprawa prefillu
to więc poprawa jednego kernela.

`ncu` na nim: 54,4 cykla między wydaniami, z tego **28,3 na `long_scoreboard`
i 18,7 na `barrier`**, przepustowość pamięci 27%, obliczeń 27%. Wygląda to na
podręcznikowy przypadek podwójnego buforowania — i nim nie jest.

**Zajętość jest już maksymalna i nie da się jej podnieść ŻADNYM kształtem
bloku.** Kernel bierze 64 rejestry, a plik rejestrów multiprocesora ma 65536,
więc mieści się dokładnie 1024 wątki — niezależnie od tego, czy to jeden blok
po 1024, dwa po 512, czy trzy po 256. Wariant 768-wątkowy (`bm96_bn32_w24`,
te same akumulatory na wątek) wyszedł na 72 rejestry, czyli JEDEN blok na
multiprocesor i 50% zajętości zamiast 66,7%. Blok 1024/64 jest optymalny, a nie
przypadkowy: wykorzystuje cały plik rejestrów.

**Podwójne buforowanie kafla nie dało nic.** Dwa komplety pamięci współdzielonej
na przemian zdejmują jedną z dwóch barier iteracji (zapis następnego kompletu
nie może wejść na wciąż czytany, jeśli idzie do drugiego). Zmierzone:
2325,3 wobec 2328,1 tok/s. Bariera nie była tym, co trzymało — czas i tak
schodzi na `long_scoreboard`.

**Kernel stoi JEDEN REJESTR od niewystartowania, i nikt tego nie pilnuje.**
`ptxas` nie zna rozmiaru bloku (Mojo nie emituje `.maxntid`), więc alokuje
rejestry swobodnie i wylądował na 64 przypadkiem. Każda zmiana, która doda
choć jeden — podwójny bufor daje 69-72, `KSTEP = 4` dawało 104 — kończy się
`CUDA_ERROR_LAUNCH_OUT_OF_RESOURCES` W CZASIE DZIAŁANIA, bo build przechodzi.
Zweryfikowane lekarstwo: `.maxntid 1024, 1, 1` w PTX sprawia, że `ptxas` celuje
w 64 rejestry sam (0 bajtów zrzutów na obecnym kernelu, 8 na podwójnie
buforowanym) i wariant, który wcześniej nie startował, startuje. Wstrzyknięcie
tej dyrektywy należałoby do `normalized_ptx` w
`scripts/build_kernel_catalog.py`, z rozmiarem bloku zadeklarowanym w katalogu
obok kernela — nie zrobione, bo dziś nic nie przyspiesza, ale to jedyna droga
do jakiejkolwiek zmiany rejestrochłonnej w tym kaflu.

Prawdziwym kosztem jest więc `long_scoreboard`, a jego źródłem `_mxfp4_word`:
blok MXFP4 ma 17 bajtów, więc każde słowo fragmentu składa się z DWÓCH
wyrównanych odczytów i przesunięcia 64-bitowego. To dwa razy tyle żądań, ile
niesie bajtów. Usunięcie drugiego odczytu (pomiar na celowo błędnym wyniku) nie
zmniejszyło rejestrów — podniosło je do 72, bo harmonogram zrobił się luźniejszy
— więc zysk musiałby przyjść z PRZEPAKOWANIA wagi przy wczytaniu do układu
36-bajtowego fragmentu, kosztem 5,9% bajtów. To jedyna niesprawdzona hipoteza,
jaka tu została.

## Dekod Q6_K: rozwinięcie po superblokach nie działa

Q6_K jest po zmianie Q4_K najsłabszym miejscem dekodu gęstego: ten sam
`ffn_down` liczy się 209 us w Q6_K i 121 us w Q4_K, czyli 179 wobec 213 GB/s.
Szerokich odczytów nie da się tam zrobić (superblok ma 210 bajtów), więc
próbowane było to samo, co przy Q4_K pomogło wcześniej: wydanie odczytów
DOT_UNROLL superbloków przed konsumpcją któregokolwiek. Dla dwóch i dla
czterech superbloków: 43,3 -> 43,3 i 43,2 tok/s, liczba rejestrów bez zmian
(56), czyli kompilator i tak nie utrzymał zapowiedzianych odczytów w locie.
Kod wrócił do postaci zwiniętej.

## Q8_0: gdzie leży następny duży zysk i dlaczego go dziś nie ma

Dekod hybrydy dzieli się tak (`nsys`, 64 kroki): 30,3% `gemv_mxfp4_f16_gidx`,
28,4% `gemv_q8_0_f16_v2`, **14,1% JEDNO uruchomienie `gemv_q8_0_dp4a` na krok
(2,36 ms — głowa logitów)** i 12,7% `gemv_q8_0_dp4a` drobnych. Głowa logitów
sama jest 15% kroku i idzie około 140 GB/s, czyli 59% osiągalnego pasma.

Powód widać w PTX jednym poleceniem:

    gemv_q8_0_dp4a_out_f32:  17 x ld.global.b16,  2 x ld.global.v8.b32
    gemv_q4_k_dp4a_persist:  15 x b16, 3 x b32, 2 x v2.b64, 1 x v4.b32, 2 x v8.b32

Blok Q8_0 ma 34 bajty, więc ŻADEN blok nie zaczyna się na granicy szesnastu
bajtów i lane, który go posiada, czyta swoje trzydzieści dwa bajty ładunku
SZESNASTOMA odczytami dwubajtowymi: sześćdziesiąt cztery bajty na instrukcję
warpa tam, gdzie `v4.b32` rusza pięćset dwanaście. To ośmiokrotność.

Wyrównać da się dopiero OSIEM bloków: 8 x 34 = 272 = 17 x 16, a `34k mod 16 = 0`
zachodzi tylko dla `k` podzielnych przez osiem. Grupa ośmiu bloków ma więc
wszystkie pola pod stałymi przesunięciami i ładunek pojedynczego bloku wycina
się z trzech wyrównanych kawałków przesunięciami, które kompilator zna.
Zaimplementowane i zmierzone: 17 x `b16` schodzi do 13 x `v4.b32` + 4 x
`v2.b64`, rejestry rosną 56 -> 66.

**I to nie weszło, bo rozbija się o mapowanie warp-wiersz.** Lane, który bierze
całą grupę, obsługuje osiem bloków, więc warp obejmuje 256 bloków = 8192
kolumny. Głowa logitów tego modelu ma 2048 kolumn, czyli 64 bloki = OSIEM grup:
osiem lane'ów miałoby pracę, dwadzieścia cztery stałyby. Zmierzone jako brak
zmiany (62,5 wobec 62,5 tok/s), bo warunek wejścia w tę ścieżkę nigdy nie
zachodzi na realnym kształcie.

Odczyt grupowy jest napisany i zmierzony w OBU mapowaniach, i OBA są wolniejsze:

    cztery wiersze na warp, wszystkie 32 lane'y zajete   56,1 tok/s
    jeden wiersz na warp, osiem lane'ow zajetych         56,2 tok/s
    stan obecny (lane na blok, 16 odczytow dwubajtowych) 62,5 tok/s

Zbieżność tych dwóch liczb jest odpowiedzią: mapowanie nie ma znaczenia, bo oba
robią TO SAMO — lane obejmuje osiem bloków zamiast jednego, więc wiersz liczy
się CZTEROKROTNIE MNIEJSZĄ liczbą lane'ów (albo tyloma samo, ale przy czterech
razy mniejszej liczbie warpów). Kernel jest związany opóźnieniem, nie
przepustowością instrukcji, a ośmiokrotnie szerszy odczyt nie odrabia
czterokrotnej straty na liczbie żądań w locie. PTX potwierdza, że odczyt
faktycznie się poszerzył (17 x `b16` znika, zostaje 13 x `v4.b32` + 4 x
`v2.b64`), więc zmierzone jest to, co miało być zmierzone.

Wniosek jest ostry i wskazuje jedyne wyjście: **34-bajtowego bloku Q8_0 nie da
się czytać szeroko bez oddania czterokrotnie mniejszej równoległości, a to
kosztuje więcej, niż szerokość przynosi.** Zysk jest osiągalny wyłącznie przez
PRZEPAKOWANIE wagi przy wczytaniu na dwie płaszczyzny — `qs` (32 bajty na blok,
ciągle, więc każdy blok zaczyna się na granicy trzydziestu dwóch bajtów) i `d`
(2 bajty na blok) — o tej samej sumie bajtów. Wtedy lane zostaje przy JEDNYM
bloku, czyta swoje trzydzieści dwa bajty jednym `ld.global.v8.b32` (dokładnie
tak, jak już czyta aktywację), trzydzieści dwa lane'y pokrywają kilobajt ciągły,
a równoległość zostaje nietknięta. To samo dotyczy Q6_K (210 bajtów) i MXFP4
(17 bajtów): jeden mechanizm rozwiązuje wszystkie trzy. Koszt: przepakowanie w
loaderze i przejście na nowy układ we WSZYSTKICH czytelnikach danego formatu.

## Zmierzony sufit przepakowania MXFP4: +39% na prefillu hybrydy

`_mxfp4_word` składa każde z dziewięciu słów fragmentu z DWÓCH wyrównanych
odczytów i przesunięcia 64-bitowego, bo blok MXFP4 ma siedemnaście bajtów.
Ile to kosztuje, dało się zmierzyć bez robienia całej przeprowadzki: kernel
zbudowany z JEDNYM odczytem zamiast pary (wynik celowo błędny, chodzi wyłącznie
o czas i wzorzec dostępu) daje na Qwen3.6-35B MXFP4:

    pp512 obecnie  2328 tok/s     llama.cpp 2502 (0,93x)
    pp512 z jednym odczytem  3234 tok/s   (1,29x)

**+38,9%.** Ten jeden szczegół układu bajtów jest całą różnicą między jedynym
pomiarem, w którym przegrywamy, a przewagą o niemal trzydzieści procent.
(Pomiar był w ogóle możliwy dopiero po dopisaniu `.maxntid` — bez niego wariant
brał 72 rejestry i nie startował.)

Przepakowanie na płaszczyzny daje dokładnie ten wzorzec dostępu, tylko z
poprawnym wynikiem: para bloków to 32 ciągłe bajty `qs` pod adresem podzielnym
przez 32 (słowa 1..8 wyrównane do czterech) plus dwubajtowa para skal z
płaszczyzny `e`. Suma bajtów bez zmian.

Układ, który NIE rusza żadnej arytmetyki wskaźników na zewnątrz kernela:
płaszczyzny na EKSPERTA (czy szerzej — na stos wierszy), czyli `[qs][e]` w
obrębie tego samego bloku pamięci. Krok eksperta zostaje `rows * bpr * 17`, więc
`wtab[e]`, okna wierszy i `_at` liczą się jak dziś; zmienia się wyłącznie to,
jak kernel liczy adres bloku — `qs` pod `base + blk * 16`, skala pod
`base + rows * bpr * 16 + blk`.

Do przejścia (nie zaczęte): `put_quant` w `cuda_exec/mod.rs` dzieli bajty przy
wgraniu, `Quantized` niesie przesunięcie płaszczyzny, i piętnaście miejsc
adresowania w czterech plikach Mojo — `gemm_fp4.mojo` (`_mxfp4_word`),
`gemv2.mojo` (siedem miejsc, w tym sztaplowanie do LDS), `decode_fused.mojo`,
`gemm.mojo` — plus fikstury MXFP4 w `tests/golden.rs`, które budują bloki
siedemnastobajtowe ręcznie. Trzynaście kerneli w katalogu czyta ten format.
Ta sama przeprowadzka, tym samym mechanizmem, czeka Q8_0 i Q6_K.

## Przeniesienie sklejania do LDS nie działa: kafel stoi na ścianie rejestrów

Zysk z przepakowania MXFP4 (+39%) bierze się z tego, że `_mxfp4_word` robi DWA
wyrównane odczyty na cztery użyteczne bajty — nie z niewyrównania jako takiego,
bo oba odczyty SĄ wyrównane. Nasuwa się więc tańsza droga niż przeprowadzka
formatu: wgrać do pamięci współdzielonej SUROWE bajty źródła (odczytami
wyrównanymi i w pełni scalonymi), a sklejanie przenieść na stronę LDS, gdzie
drugi odczyt kosztuje kilkanaście cykli zamiast kilkuset.

Zrobione i zmierzone. Strona globalna faktycznie się wyprostowała — z pary
odczytów na słowo zostaje ciąg wyrównanych `ld.global.b32` — ale wynik jest
GORSZY: 2286 wobec 2323 tok/s. Powód widać w `ptxas`: **80 bajtów zrzutu**.
Kernel bierze 64 rejestry przy 1024 wątkach, czyli cały plik rejestrów
multiprocesora, więc każda dodatkowa praca na odczyt nie ma gdzie zamieszkać i
ląduje w pamięci lokalnej. Uproszczenie sklejania do przesunięć 32-bitowych i
policzenie cofnięcia okna z samej parzystości niczego nie odzyskało.

Wniosek domyka temat: **zysku nie da się wziąć przez PRZENIESIENIE pracy —
tylko przez jej USUNIĘCIE.** Odczyt musi być pojedynczy i bez składania, a to
znaczy, że zmienić się musi POSTAĆ ZAPISANEJ WAGI, nie kernel. To samo wyszło
przy Q8_0 od drugiej strony: tam szersze odczyty wymagały oddania
czterokrotnej równoległości. Trzy formaty, trzy różne obejścia, ta sama
odpowiedź — płaszczyzny.

## Silnik i wykonawca grafowy rozeszły się na mieszance ekspertów

Optymalizacje MoE z lipca i sierpnia trafiały do `CudaExec`, a `forge-engine`
zostawał przy pierwszej, poprawnościowej wersji. `bench`, `serve` i `run`
uruchamiają WYŁĄCZNIE silnik — `CudaExec` nie ma z nich ani jednego wywołania —
więc każdy pomiar zrobiony przez `forge-model` opisywał kod, którego użytkownik
nie dostaje. Rozjazd urósł do 24× na prefillu i 1,6× na dekodowaniu, zanim
ktokolwiek go zobaczył, bo model gęsty (Bielik) odtwarzał się co do procenta i
niczego nie sygnalizował.

Trzy rzeczy istniały w `forge-kernels` z testami i nie miały wołającego poza
`CudaExec`: rozdzielony router (`moe_topk_f32`), wsadowa rozsyłka wyboru
(`gemv_gidx_batch`, `gemv_silu_gidx_batch`) i zgrupowany GEMM
(`gemm_grouped_experts`). Wpięcie ich w silnik dało na RTX GB10 dla
Qwen3-30B-A3B Q4_K: prefill 94,8 → 2330,6 tok/s, dekodowanie 41,8 → 68,1 tok/s,
przy wyjściu bitowo niezmienionym na każdym kroku.

Wniosek na przyszłość: pomiar ze ścieżki grafowej NIE jest pomiarem produktu,
dopóki `forge-engine` nie zniknie. Każdą liczbę podawaną jako wynik FORGE
trzeba brać z `forge bench`.

## Budżet rezydencji ekspertów liczył sklejony tensor tyle razy, ilu jest ekspertów

`byte_len(name.replace("{expert}", "0")) * n_experts` jest poprawne dla
safetensors, gdzie każdy ekspert ma własny tensor, i błędne dla GGUF, gdzie
`blk.N.ffn_gate_exps.weight` niesie już cały stos. Qwen3.6-35B-A3B raportował
4 177 920 MiB ekspertów wobec 58 GiB VRAM i 1,4% rezydencji zamiast 16 320 MiB
i 100%. Nie to było wąskim gardłem prefillu, ale każda decyzja o stronicowaniu
opierała się na liczbie 256 razy za dużej.

## Prefill hybrydy odrzuca MoE i schodzi do prędkości dekodowania

`hybrid_batched_prefill_capable` ma warunek `!self.weights.is_moe()`, a droga
layer-major wymaga `LayerFfn::Dense`. Model hybrydowy z mieszanką ekspertów nie
przechodzi przez żadną z nich i liczy prefill token po tokenie — tą samą
warstwą co dekodowanie. Stąd Qwen3.6-35B-A3B MXFP4 ma prefill 6,4 tok/s przy
dekodowaniu 6,3: to nie zbieżność, to ta sama pętla. Profil: 92,6% czasu GPU w
`gemm_mxfp4_gguf_impl`, router wołany 20480 razy zamiast raz na warstwę.

Wpięcie tu `moe_prefill_ffn` nie wystarczy. Bufory routera i grupowania są
wymiarowane na `MAX_PREFILL_CHUNK`, a arena layer-major dobiera segmenty do
T2048/T4096, więc segment dla modelu MoE musiałby być osobno ograniczony.
Zgrupowany kafel MXFP4 jest też innym wywołaniem niż `gemm_grouped_experts`
(`gemm_mxf4_grouped`, kwantyzowana para aktywacji) i wg pomiaru w
`launchers/moe.rs` przegrywa 1,3× z rozsyłką per ekspert — dla tego formatu
grupowanie ma dotyczyć WIERSZY, nie siatki.

## Sufit dekodowania Qwen3-30B-A3B na GB10

Na token: 1,18 GB wag ekspertów (8 z 128, trzy projekcje, 48 warstw), 0,51 GB
projekcji uwagi, 0,26 GB głowy. Razem 1,94 GB, co przy zmierzonych 237 GB/s
daje 8,2 ms i 122 tok/s. FORGE jest na 14,7 ms (56% sufitu), llama.cpp na
12,3 ms (67%). Sam FFN ekspertów zajmuje 5,99 ms wobec 4,96 ms wynikających z
bajtów, czyli 83% pasma — tam nie ma już zapasu. Reszta różnicy siedzi w
projekcjach uwagi i głowie, rozdrobniona.

### MoE w prefillu hybrydy — rozwiązane

Blokadą nie była poprawność, tylko dobór kernela, i rozeszła się na cztery
rzeczy. Kolejność ma znaczenie, bo trzy pierwsze bez czwartej dawały regres.

1. **Ekspert współdzielony wsadowo.** Gęste SwiGLU po wszystkich `t` wierszach,
   dokładane po scaleniu — dzięki czemu pożycza jego scratch. Scalenie po
   identyczności z `top_k = 1` JEST skalowanym dodawaniem per wiersz, więc nie
   trzeba było ani nowego kernela, ani uruchomienia na token.
2. **Bramka per token dla tego eksperta.** Prefill wpisywał skalę 1,0, decode
   liczył `sigmoid(ffn_gate_inp_shexp · x)`. Dopóki ścieżka wsadowa była
   wyłączona, stała była przypadkiem poprawna dla każdej architektury, która tam
   docierała.
3. **Zgrupowany GEMM dla MXFP4.** Bez niego prefill wsadowy schodził na pętlę
   pięciu uruchomień na eksperta na token i był **dziesięciokrotnie wolniejszy**
   niż pętla „token po tokenie", która wcale nie jest wolna — idzie przez
   `moe_decode_ffn`, czyli rozsyłkę adresowaną na urządzeniu.
4. **Jednostka czterobitowa.** llama.cpp jest tu zbudowana na `arch 121`, więc
   jej MXFP4 przechodzi przez `mma.sync.aligned.kind::mxf4.block_scale` — e2m1
   po OBU stronach z ue8m0. Odpowiadanie na to aktywacją f16 nie jest tą samą
   operacją i nigdy jej nie dogoni: 834,9 wobec 2351,8 tok/s.

Do tego dwie rzeczy spoza mieszanki: aktywacja zgrupowana przygotowywana raz na
AKTYWACJĘ, nie raz na wagę (gate i up czytają te same wiersze), oraz uwaga
dobierana liczbą zapytań, a nie producentem karty — kafel zrównoleglony po
tokenach był osiągalny wyłącznie poza NVIDIĄ.

RTX pominięty; wszystko zmierzone na GB10, `pp512`, wobec llama.cpp `6db1304`
na tej samej maszynie: 67,9 → 2524,0 tok/s przy ich 2491,6.

### Bramka, bez której nic z tego nie jest wiarygodne

`tests/prefill_parity_gpu.rs` liczy ten sam prompt wsadowo i krokiem po kroku,
po czym porównuje logity, argmax i szesnaście chciwych kroków. Trzy rzeczy
musiały być w niej naprawione, zanim zaczęła cokolwiek znaczyć, i każda z nich
najpierw dała fałszywy wynik:

- **Porównywanie SHA wygenerowanych tokenów** nie umie rozstrzygnąć, która
  wersja jest poprawna. Logity pod teacher forcingiem umieją, bo obie ścieżki
  liczą wtedy tę samą funkcję.
- **Dwie sekwencje hybrydy żywe naraz** mierzą politykę puli stanu DeltaNet, nie
  prefill. Jedna naraz.
- **Prompt z losowych identyfikatorów** wpędza model w pętlę powtórzeń, gdzie
  argmax stoi na remisach i dowolne zaokrąglenie przestawia jej fazę. Realny
  tekst.

Pula wag testu liczy się z rozmiaru checkpointu, nie z całej wolnej pamięci: na
GB10 pamięć GPU JEST pamięcią systemu i ta druga arytmetyka zawiesiła hosta.
