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
   sprawdzalne zapadką, dziś liczy 3.
4. **CUDA implementuje ten sam `Executor`.** Dopiero to kasuje 2822 linie
   `dense.rs`, i dopiero wtedy hybryda oraz MoE działają na Metalu bez
   przepisywania.
5. **`forge-quant` wydzielony z `forge-formats`**, z wzorcem CPU jako wyrocznią.
6. **Passy nad `Vec<Op>`** — fuzja i autotuning, bez dotykania modeli.

Kroki 1–3 są mechaniczne i sprawdzalne istniejącymi testami. Krok 4 jest tym, po
którym widać zysk. Kroki 5–6 są ulepszeniami, nie warunkami.
