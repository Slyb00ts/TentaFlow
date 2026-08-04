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
| formaty wag | **24** | 2 |
| rodziny architektur | dense, MoE, hybrid, DeepSeek | dense |
| radix, continuous batching, spekulacja, TP | **jest** | nie ma |
| Apple / Metal | **zero wystąpień** | jedyna, która tam liczy |

Kierunek zejścia nie jest wyborem. Silnik nie przeniesie się na Metal ani na
żaden następny backend, bo jego model JEST kodem wołającym kernele jednej
rodziny kart — „przeniesienie" znaczyłoby napisanie go drugi raz, czyli
dokładnie ten błąd, który ten dokument opisuje w §2. Nowa ścieżka wyraża to
samo danymi i ma trzech wykonawców. Więc: nowa ścieżka rośnie o to, co silnik
ma, silnik chudnie, a produkcja przechodzi DOPIERO po zrównaniu w pomiarze.

Kolejność, w której każdy krok odblokowuje następny:

1. **Wspólna warstwa stanu.** ZACZĘTE: `forge-state` istnieje i trzyma
   stronicowane KV oraz drzewo radix, oba wyjęte z `forge-engine` bez zmiany
   ani jednej linii logiki. Zostaje druga połowa — żeby `CudaExec` używał tego
   zamiast własnego stronicowania, które musiało powstać, dopóki tamto należało
   do jednej ścieżki. Admission i continuous batching (`server.rs`) idą tą samą
   drogą: też nie zależą od tego, jak model liczy warstwę, tylko od stron i
   tokenów. Na Apple wspólne stronicowanie wymaga stronicowanych wariantów
   dwóch kerneli MSL, bo `MetalExec` trzyma dziś cache jako jedną ciągłą połać
   na warstwę.
2. **Fuzja jako pass** (§7.3 `ZADANIE_CUDA_EXECUTOR.md`). Nowa ścieżka wykonuje
   szesnaście uruchomień na warstwę tam, gdzie scalony łańcuch silnika ma trzy.
   Dopóki tak jest, porównanie obu ścieżek mierzy narzut uruchomień, a nie
   architekturę — i nie wolno na jego podstawie niczego przełączać.
3. **Formaty wag.** 24 wobec 2, ale to nie jest dwunastokrotność pracy:
   `put_quant` bierze bloki źródła, launchery istnieją dla wszystkich, a
   `forge-formats::dequant` jest wzorcem CPU każdego z nich. To tablica
   dyspozycji w wykonawcy plus bramka na wzorcu per format.
4. **MoE i hybrid w słownictwie.** Jedyna pozycja, która jest prawdziwą pracą
   projektową: `Op` musi wyrazić routing ekspertów i stan rekurencyjny
   DeltaNet, a każda platforma potrzebuje tych kerneli — inaczej „wszystko na
   każdym systemie" jest nieprawdą, a nie planem.
5. **Spekulacja jako pass plus kontrakt proposera.** `forge-engine::speculation`
   ma już typowany `Proposer` i statystyki akceptacji, więc to przeniesienie,
   nie wymyślanie.
6. **Serwer przechodzi, silnik chudnie.** Dopiero tutaj kasujemy cokolwiek.

Reguła na cały ten czas: żaden krok nie kończy się deklaracją, tylko pomiarem
wobec wzorca hostowego (poprawność) i wobec silnika (wydajność), na tym samym
checkpoincie.

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
