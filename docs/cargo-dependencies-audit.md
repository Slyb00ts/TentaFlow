# Audyt zależności Cargo

[Katalog 197 bezpośrednich zależności zewnętrznych](cargo-dependencies-audit.csv)
podaje rolę każdej biblioteki, wersję lub rewizję w głównym manifeście,
pakiety deklarujące zależność oraz decyzję dotyczącą jej pozostawienia.
To audyt deklaracji i architektury. Lista odbiorców pochodzi z Cargo metadata;
nie stanowi pomiaru tego, ile kodu danej biblioteki trafia do konkretnej binarki.

## Usunięte i ujednolicone zależności

- Własne pakiety dziedziczą wersje i źródła z jednego katalogu w korzeniu.
- Usunięto bezpośrednie `rand_core` i alias `rand_core_06`; generowanie kluczy
  korzysta z losowych bajtów systemowych i istniejących konstruktorów kluczy.
- Klient natywny używa `std::sync::OnceLock`, więc usunięto jego `once_cell`.
- Addon RAG nie deklaruje już nieużywanych parserów `calamine`, `zip`,
  `flate2` i `quick-xml`; przetwarzanie tych dokumentów odbywa się w rdzeniu.
- `burn-spike` aktywuje potrzebne cechy `std`, `wgpu` i `ndarray` zamiast
  całego zestawu domyślnego. Usuwa to konflikt SQLite pochodzący z nieużywanego
  `burn-dataset`, bez zmiany kodu upstream.
- W grafie jest jedna wersja `rusqlite` 0.39.0, `egui` i `eframe` 0.34.3
  oraz `wgpu` 29.0.3. Zakresy wersji w manifeście mogą wskazywać niższą
  zgodną wersję poprawkową; konkretne rozwiązanie zapisuje root `Cargo.lock`.
- Integracja eframe używa aktualnych metod `App::logic` i `App::ui`;
  rysowanie paneli otrzymuje główne `egui::Ui` i korzysta z `show_inside`.
  Osobny check UI na Linux przeszedł. Nie jest to potwierdzenie całego
  desktopu, którego zastane rozbieżności API opisuje
  [raport walidacji](build-performance.md#końcowa-walidacja-i-jej-ograniczenia).

## Wersje pośrednie

Pełny graf całego workspace, obejmujący zależności różnych platform,
zawiera 179 nazw pakietów z więcej niż jedną wersją. Nie wszystkie występują
w pojedynczym buildzie. [Pełna lista](cargo-transitive-versions.csv) podaje
wersje i bezpośrednich odbiorców w rozwiązanym grafie.

| Biblioteka | Pozostałe wersje | Powód |
| --- | --- | --- |
| `reqwest` | 0.12.28 i 0.13.3 | Własny kod używa 0.13.3; `hf-hub`, `tokio-rustls-acme` i narzędzia LLVM wymagają 0.12 |
| `sha2` | 0.10.9 i 0.11.0 | Własny kod używa 0.11; m.in. Ed25519 2.x, Cozo i WebRTC nadal wymagają wcześniejszego API kryptograficznego |
| `rand` | 0.8.5, 0.9.2, 0.10.1 | Zależności kryptograficzne, WebRTC/Candle oraz iroh/Burn wymagają różnych generacji API |
| `rand_core` | 0.6.4, 0.9.5, 0.10.1 | Pozostają wyłącznie pośrednio jako wymagane interfejsy generatorów losowych |
| `getrandom` | 0.2.17, 0.3.4, 0.4.3 | Starsze interfejsy są wymagane m.in. przez `ring`, `rand_core`, `ahash`, `jobserver` i tokenizery; własny kod używa 0.4.3 |
| `zip` | 0.6.6, 7.2.0, 8.6.0 | `tch` wymaga 0.6, Calamine i Candle wymagają 7, własny kod i Burn Store używają 8 |
| `once_cell` | 1.21.4 | Pozostała pojedyncza wersja pośrednia, wymagana przez biblioteki zewnętrzne; brak własnej bezpośredniej deklaracji |

Wymuszenie jednej niezgodnej wersji przez globalny patch nie jest poprawnym
sposobem usunięcia takich zależności. Może zmienić publiczne typy, wymagane
cechy lub ABI. Dalsza redukcja wymaga migracji konkretnych odbiorców upstream,
a następnie testów funkcjonalnych i pomiaru czasu oraz rozmiaru builda.

## Podobne biblioteki o różnych zadaniach

- `ciborium` obsługuje istniejącą serializację serde, a `minicbor` jawnie
  indeksowane struktury SDK. Zmiana wymaga migracji kontraktu wire.
- `wasmtime` zapewnia JIT na desktopie, `wasmi` interpreter na urządzeniach
  mobilnych; `wasmtime-wasi` dostarcza interfejsy systemowe.
- `handlebars` generuje dokumenty prawne, a `minijinja` odtwarza semantykę
  szablonów modeli. Zamiana silnika wymaga migracji i porównania szablonów.
- `quick-xml` czyta i zapisuje strumieniowo, `roxmltree` udostępnia drzewo URDF.
- ONNX Runtime, Tract, Burn oraz natywne silniki inferencji różnią się
  obsługiwanymi modelami, sprzętem, zasobami i sposobem wdrożenia.
- `ureq` pozostaje w synchronicznych ścieżkach, które wcześniej miały problem
  z zagnieżdżonym runtime `reqwest::blocking`. Ujednolicenie wymaga osobnej
  analizy uruchamiania zadań i testów, a nie samej zamiany nazw metod.
- `num_cpus` pozostaje do osobnego pomiaru: `available_parallelism` może inaczej
  uwzględniać affinity oraz limity procesora, wpływając na rozmiar pul.

Pozostałe decyzje są opisane przy wszystkich pozycjach katalogu CSV.
