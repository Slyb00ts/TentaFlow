# Przegląd układu projektu — co da się używać ponownie, a co jest pisane od nowa

Stan na dziś, zmierzony, nie oszacowany. Odpowiada na dwa pytania: **dlaczego
tyle rzeczy jest pisanych osobno dla każdej platformy** i **co znaczy „granica
HAL"**.

## 1. Ile czego jest

| crate | linie | rola |
|---|---:|---|
| forge-engine | 42 285 | silnik CUDA: model, harmonogram, spekulacja, ładowanie |
| forge-kernels | 25 950 | kernele i ich wybór |
| forge-formats | 13 739 | GGUF / safetensors / MLX / NVFP4 + rejestr architektur |
| forge-server | 5 120 | HTTP / OpenAI |
| forge-hal | 4 324 | jedyny styk ze sterownikiem |
| forge-cli | 3 532 | run / bench / inspect |
| forge-grammar | 2 281 | gramatyki wymuszane przy dekodowaniu |
| forge-whisper | 2 113 | STT |
| forge-onnx | 1 962 | ONNX |
| forge-model | 1 705 | model gęsty na Metalu |
| forge-tokenize | 1 669 | tokenizacja |
| forge-types | 355 | typy bazowe |
| forge-rdma | 270 | transport RoCE |

**forge-engine to 40% projektu.** Plan naprawy nie przewiduje takiego crate'u
w ogóle — jego zawartość ma się rozejść na `forge-graph`, `forge-model`,
`forge-sched`, `forge-spec` i `forge-state`. Dopóki tego nie ma, „silnik" znaczy
„CUDA", a każda inna platforma musi sobie dopisać własny.

## 2. Co to jest granica HAL

`forge-hal` to jedyne miejsce, które rozmawia ze sterownikiem: przydziela
pamięć, uruchamia kernel, czeka na zakończenie. Typy `DevBuffer`, `Stream`,
`Device` pochodzą stamtąd.

Plan (§5.1, reguła 2) mówi, że widzą je **wyłącznie** `forge-kernels`,
`forge-state` i `forge-cli`. Wszystko powyżej — opis modelu, harmonogram,
spekulacja — ma nie wiedzieć, czy pod spodem jest CUDA, Metal czy CPU.

**Dlaczego to nie jest czystość dla czystości.** Model, który trzyma `DevBuffer`,
jest modelem DLA TEGO urządzenia. Nie da się go uruchomić na drugim, więc drugie
urządzenie dostaje własny model. Tak powstaje „coś dedykowanego wszędzie" — nie
z lenistwa, tylko z jednego importu.

Konkretnie, dziś:

```rust
// crates/forge-model/src/mlx_dense.rs
use forge_hal::{DevBuffer, Device, Event, KernelHandle, LaunchArgs, LaunchConfig, Pool, Stream};
```

Ten jeden wiersz sprawia, że `mlx_dense` opisuje architekturę gęstą **i**
prowadzi bufory Metala. Architektura jest wspólna, prowadzenie buforów nie —
więc całość staje się metalowa. To moja zmiana i to jest jej koszt.

Docelowo model mówi „pomnóż A przez B, wynik do C", gdzie A, B i C są
nieprzezroczystymi uchwytami, a `forge-kernels` decyduje, czym to policzyć.
Wtedy jeden opis architektury działa na trzech backendach.

## 3. Gdzie ponowne użycie jest tracone — zmierzone

### 3.1. Ta sama architektura opisana dwa razy

| plik | linie | co opisuje |
|---|---:|---|
| `forge-engine/src/model/arch/dense.rs` | 2 822 | model gęsty, CUDA |
| `forge-model/src/mlx_dense.rs` | 1 096 | model gęsty, Metal |

**3 918 linii na jedną architekturę.** Kolejność warstw, RoPE, uwaga, KV,
residual — identyczne. Różni się wyłącznie to, czym się mnoży.

Koszt rośnie liniowo z architekturami: hybryda i MoE istnieją dziś tylko po
stronie CUDA. Doniesienie ich na Metal przy obecnym układzie znaczy napisać je
drugi raz, a nie podłączyć.

### 3.2. Dwa niezależne światy kerneli

| | CUDA | Metal |
|---|---|---|
| źródło | Mojo → PTX, budowane wcześniej | napisy MSL, kompilowane przy starcie |
| wybór | `registry.rs` (3 112 linii) + typowane launchery | `msl.rs` (1 454 linie), 12 kerneli |
| katalog | 417 artefaktów | brak |

To NIE jest ten sam mechanizm z dwiema implementacjami. To dwa mechanizmy.
`variant.rs`, który miał być wspólnym rejestrem wariantów, ma dziś **cztery
bramki `cfg`** rozdzielające formy metalowe od CUDA — czyli formy leżą obok
siebie, ale nie w jednym rejestrze.

### 3.3. Co JUŻ jest wspólne i pokazuje docelowy kształt

Formaty i kwantyzacje są dziś najdalej posuniętym przykładem tego, o co w tym
całym układzie chodzi, więc warto zapisać, jak wygląda skończona wersja:

| warstwa | gdzie | co wie |
|---|---|---|
| skąd bajty | `TensorSource` | GGUF / safetensors / MLX / NVFP4 |
| co znaczą bajty | `forge-formats::dequantize_to_f32` | 22 kwantyzacje, wzorzec CPU |
| czym je mnożyć | `block_formats!` w wykonawcy | 22 wiersze, kernel per format |
| czy to prawda | `tests/format_table.rs` | te same bajty obiema drogami |

Model nie występuje w tej tabeli ani razu — i to jest cała teza. Dodanie
kwantyzacji dotyka dekodera CPU (wzorzec) i jednego wiersza tabeli wykonawcy;
nie dotyka ani opisu architektury, ani loadera, ani drugiego backendu. Wykonawca
na inną kartę wnosi WŁASNĄ tabelę wobec tego samego wzorca, więc „ten sam format
na każdej architekturze" jest sprawdzalne, a nie deklarowane.

Warstwa formatów, po zmianach z tej sesji, jest przykładem, jak to ma wyglądać:

- `TensorSource` z implementacjami dla GGUF, safetensors, MLX, NVFP4
- `Checkpoint` — jedno wejście, ukrywa „katalog czy plik"
- `affine.rs` — Q4_1, Q4_K i Q6_K sprowadzone do jednej formy, bo liczą tę samą
  formułę
- `ModelDescriptor` — pytasz o ROLĘ, dostajesz nazwę właściwą dla źródła

Model nie wie, z jakiego formatu został wczytany, i nie ma tam ani jednej
gałęzi „jeśli GGUF". Tak samo ma wyglądać reszta.

Ten sam kształt złapała warstwa architektur po kroku 4 (`ZADANIE_MOE_HYBRID.md`),
i tu też model nie występuje w tabeli:

| pytanie | gdzie | co wie |
|---|---|---|
| co niesie warstwa | `ModelDescriptor` | role, nie nazwy tensorów |
| czym miesza tokeny | `Mixer` per warstwa | uwaga albo rekurencja |
| gdzie leży jej stan | `forge-state` | strony KV, okna splotu, macierze |
| jak to policzyć | `Op` + trzej wykonawcy | jedna operacja na rodzinę |

Stos Qwen3.6 przeplata dwa miksery trzy do jednego, więc „który mikser" jest
własnością WARSTWY, a nie modelu — i dlatego jest enumem w tym samym miejscu, co
kształt bloku FFN. Trzy rodziny (gęsta, mieszanka, hybryda) liczą się dziś tym
samym opisem; dodanie czwartej dotyka słownictwa tylko wtedy, gdy wnosi
operację, której żaden wykonawca nie umie wykonać.

## 4. Trzy dźwignie, w kolejności opłacalności

### Dźwignia 1: opis modelu bez urządzenia (największa)

Model przestaje trzymać `DevBuffer` i zaczyna opisywać operacje na uchwytach.
`forge-kernels` dostaje fasadę wykonawczą: „to jest mnożenie o takim kształcie,
policz je". Wtedy:

- jeden `dense` zamiast dwóch — **minus ~1 100 linii od razu**,
- hybryda i MoE działają na Metalu bez przepisywania,
- nowy backend to nowy zestaw kerneli, a nie nowy model.

To jest `forge-graph` z planu. Bez niego każda kolejna architektura kosztuje
podwójnie.

### Dźwignia 2: jeden rejestr wariantów zamiast dwóch światów

`variant.rs` już ma właściwy kształt (problem → uporządkowana lista form,
każda z pomiarem). Brakuje mu tego, żeby backendy REJESTROWAŁY w nim swoje
formy, zamiast być rozdzielone `cfg`. Wtedy „która forma obsłuży ten kształt"
jest jednym pytaniem, a nie dwoma równoległymi.

### Dźwignia 3: rozbicie forge-engine

40% projektu w jednym crate'cie oznacza, że wszystko, co go dotyka, dotyka
CUDA. Plan ma gotowy podział; drugi agent już zaczął (`model.rs` 21 430 →
`model/`). Dopóki nie zejdzie poniżej, dźwignie 1 i 2 nie mają gdzie zamieszkać.

## 5. Co pilnuje, żeby nie było gorzej

Zapadka lintu ma od dziś regułę `hal_boundary`. Stan zastany:

| crate | wystąpień |
|---|---:|
| forge-engine | 107 |
| forge-server | 24 |
| forge-whisper | 9 |
| forge-onnx | 5 |
| forge-model | 3 |

`forge-engine` to monolit, który plan i tak rozbiera. Cztery pozostałe to crate'y
leżące NAD granicą, więc ich importy są długiem do spłaty. Zapadka trzyma
dzisiejszą liczbę — nowe naruszenie nie przejdzie.

## 6. Kolejność, którą proponuję

1. **Dokończyć GGUF na Metalu** przez istniejący układ (kernele sześciobitowe +
   mieszana kwantyzacja w rejestrze). To domyka bieżący wątek i przy okazji
   sprawdza, czy rejestr wariantów udźwignie drugą oś.
2. **Dźwignia 1 na jednej architekturze** — wyprowadzić `dense` do opisu bez
   urządzenia i uruchomić TEN SAM opis na Metalu i CUDA. Dowód działa albo nie
   na jednym pliku, zanim ruszy się resztę.
3. **Dźwignia 2**, bo bez niej krok 2 skończy się rejestrem per backend.
4. **Dźwignia 3** — dalsze rozbijanie `forge-engine`, już z gotowym miejscem
   docelowym.

Odwrotna kolejność (najpierw rozbijać silnik) daje mniejsze pliki, ale nadal dwa
modele i dwa światy kerneli.
