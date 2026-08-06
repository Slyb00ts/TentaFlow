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
