# Czas budowania i miejsce na artefakty

Główny `Cargo.toml` definiuje jeden workspace, profile oraz wersje zależności
wszystkich własnych pakietów. Dzieci dziedziczą zależności przez
`workspace = true`; jeden root `Cargo.lock` ustala rozwiązane wersje.
Domyślnym pakietem jest `tentaflow`, więc `cargo build` działa również
w korzeniu repo. Konfiguracja `.cargo/config.toml` wybiera wspólny katalog
`target_shared/`. Ustawienie `CARGO_TARGET_DIR` albo `--target-dir` wybiera
osobny katalog i osobny zestaw artefaktów.

## Profile i codzienna praca

Profile mają jedno źródło w głównym `Cargo.toml`:

| Profil | Ustawienia | Zastosowanie |
| --- | --- | --- |
| `dev` | `opt-level=0`, `debug=1`, incremental, 256 jednostek generowania kodu | Zwykłe `build` i `check`; wybrane ciężkie zależności mają osobne `opt-level=3` |
| `test` | Dziedziczy `dev`, `debug=1`; zachowuje optymalizacje wybranych zależności | Testy i iteracje nad kodem testowanym |
| `release` | `opt-level=3`, ThinLTO, 4 jednostki generowania kodu, bez incremental, `strip=true` | Wydanie produkcyjne |
| `release-fast` | `opt-level=3`, LTO wyłączone, 4 jednostki generowania kodu, incremental, `strip=false` | Codzienna praca z optymalizowanym kodem |
| `release-wasm` | `opt-level="s"`, ThinLTO, 1 jednostka generowania kodu | Rozmiar wynikowego WASM; używany przez zagnieżdżone buildy |
| `bench` | Dziedziczy `release`, `debug=1`, `strip=false` | Benchmarki i przypisywanie próbek profilowania do kodu |

`debug=1` oznacza ograniczone informacje debugowania: pomaga odtworzyć stos
i położenie kodu, lecz daje mniej informacji o zmiennych lokalnych i typach
niż pełne `debug=2`. Samo `strip=false` w `release-fast` nie włącza pełnego
debugowania. Zmiany poziomu debugowania należy definiować w profilu korzenia.

Przykłady z zestawem cech użytym w pomiarach; przy własnej konfiguracji zachowaj
swoje `--features` i target. Linux/macOS:

```bash
./scripts/build.sh build --profile release-fast --locked --no-default-features --features gpu-vulkan
./scripts/build.sh build --release --locked --no-default-features --features gpu-vulkan
./scripts/build.sh test -p tentaflow-core --lib --locked --no-default-features --features gpu-vulkan
```

PowerShell:

```powershell
.\scripts\build.ps1 --profile release-fast --locked --no-default-features --features gpu-vulkan
.\scripts\build.ps1 --release --locked --no-default-features --features gpu-vulkan
.\scripts\build.ps1 -Cmd test --package tentaflow-core --lib --locked --no-default-features --features gpu-vulkan
```

Wybór `--profile release-fast` nie zmienia ustawień produkcyjnego `release`.
Profile mają osobne artefakty w `target_shared/release-fast/` i
`target_shared/release/`; pierwsze użycie kolejnego profilu może wymagać
zbudowania jego zależności. Wielkość przyspieszenia codziennej iteracji wymaga
pomiaru po konkretnej zmianie źródła.

## Co zajmuje miejsce

| Katalog | Zawartość | Zasada przechowywania |
| --- | --- | --- |
| `target_shared/` | Biblioteki, binarki, wyniki `build.rs`, dane kompilacji przyrostowej | Artefakty zależą od profilu, targetu, cech, flag i wersji kompilatora |
| Pozostałe `target/` | Osobne drzewa kompilacji | Mogą zawierać kopie zależności używanych także w `target_shared/` |
| Cache `sccache` | Skompresowane wyniki wywołań kompilatora | Osobny limit pojemności i automatyczna ewikcja LRU |
| Cargo `registry/` i `git/` | Pobrane paczki oraz źródła zależności | Osobny cache pobierania; jego limit nie ogranicza `target_shared/` |

Cargo nie przechowuje katalogów odpowiadających pięciu ostatnim kompilacjom.
Kolejne kompilacje współdzielą artefakty, a zmiana profilu, cech lub flag może
utworzyć kolejną wersję tej samej biblioteki. Usuwanie plików wyłącznie według
daty modyfikacji może usunąć nadal używaną zależność, która nie wymagała
przebudowy od kilku tygodni.

## Automatyczna retencja artefaktów

Budowanie przez `scripts/build.sh` na Linux/macOS lub `scripts/build.ps1`
na Windows uruchamia wspólny wrapper `scripts/cargo-build.py`. Reguły znajdują
się w `scripts/config/build-cache.toml` i nie zawierają ścieżek użytkownika:

- Historia pięciu udanych kompilacji chroni użyte przez nie artefakty,
  również zależności oznaczone przez Cargo jako aktualne. Nie przechowuje
  pięciu kopii gotowej binarki.
- Limit katalogu target wynosi 250 GiB. Niechronione warianty starsze niż
  14 dni mogą być usuwane; presja limitu pozwala usuwać również młodsze.
- Kompilacja przyrostowa ma osobny limit 40 GiB i pięć wariantów na crate.
- Przed pierwszą zarejestrowaną kompilacją najnowszy rozpoznany wariant
  każdej roli pakietu jest chroniony.
- Osobne, stabilne katalogi kompilacji zagnieżdżonych mają limity 10 GiB
  dla `target-browser-wasm`, 10 GiB dla `target-addon-wasm` i 20 GiB dla
  `target-meeting-bot`. Wrapper porządkuje je po udanym buildzie.

Limity są **miękkie i dotyczą wybranego katalogu target**. Jeżeli chronione
artefakty lub pliki poza zakresem retencji przekraczają limit, wrapper
informuje o tym i pozostawia je. Blokady Cargo chronią trwające kompilacje,
a kontrole ścieżek wstrzymują usuwanie przy dowiązaniach. Cache sccache
i cache źródeł Cargo mają osobne zasady.

Katalog target i jego przodkowie muszą być fizycznymi ścieżkami, bez
dowiązań symbolicznych ani punktów ponownej analizy Windows, w tym junction.

```bash
# Zwykły build zakończony retencją.
./scripts/build.sh --profile release-fast

# Sam plan; niczego nie usuwa.
python3 scripts/cargo-build.py report --target-dir target_shared

# Jednorazowe uporządkowanie istniejących artefaktów według tego samego planu.
python3 scripts/cargo-build.py prune --target-dir target_shared --apply
```

Plan jest zapisywany jako `.build-cache-plan.json` w wybranym katalogu target.
Ręczne `cargo build` korzysta ze wspólnego workspace i profili, ale nie
uruchamia wrappera ani automatycznej retencji.

## Cache sccache

`sccache` pozostaje opcjonalną zależnością lokalną. Repo nie ustawia ścieżki
do konkretnej instalacji wrappera. Po jego zainstalowaniu można aktywować go
przez `RUSTC_WRAPPER=sccache` lub własną konfigurację Cargo.

Wspólna polityka repo ustawia `SCCACHE_CACHE_SIZE=20G` przez sekcję `[env]`
w `.cargo/config.toml`. `force = true` zapewnia tę samą wartość również wtedy,
gdy terminal odziedziczył większy limit. Jednostka `G` oznacza GiB.
Ustawienie obowiązuje na Linux, macOS i Windows; nie wymaga ścieżek konkretnego
użytkownika. Dotyczy lokalnego cache sccache, a nie katalogu `target_shared/`.

**Limit jest odczytywany przy starcie serwera sccache.** Działający wcześniej
serwer zachowuje swoją konfigurację. Polecenia uruchamiane bezpośrednio z powłoki,
np. `sccache --start-server` lub osobny build CMake, nie odczytują konfiguracji
Cargo. Serwer uruchomiony przez inne repo może więc mieć inną pojemność.

Po zakończeniu wszystkich kompilacji korzystających z danego serwera można
jednorazowo uruchomić go z właściwym limitem:

```bash
sccache --stop-server
SCCACHE_CACHE_SIZE=20G sccache --start-server
```

W PowerShell:

```powershell
sccache --stop-server
$env:SCCACHE_CACHE_SIZE = '20G'
sccache --start-server
```

W fish:

```fish
sccache --stop-server
env SCCACHE_CACHE_SIZE=20G sccache --start-server
```

Następne wywołanie kompilatora korzystające z cache inicjalizuje go z nowym
limitem i usuwa nadmiarowe wpisy LRU. Sam start serwera lub `--show-stats`
może jeszcze nie zainicjalizować dyskowego cache. Nie trzeba wykonywać
`cargo clean` ani kasować katalogu sccache. Stan sprawdza się poleceniem
`sccache --show-stats`, które pokazuje osobno bieżącą i maksymalną pojemność.

Nie należy automatycznie restartować współdzielonego serwera przy każdym
buildzie: może obsługiwać równoległą kompilację innego projektu.

Zachowanie sprawdzono na sccache 0.16.0 z izolowanym serwerem i 30 rzeczywistymi
kompilacjami małych plików C. Zmiana zmiennej środowiskowej przy działającym
serwerze nie zmieniła limitu. Po restarcie i kolejnym odczycie cache jego
zawartość zmniejszyła się z 15 969 do 4 799 bajtów przy nowym limicie 5 120
bajtów; pozostało 9 z 30 wpisów.

Na rzeczywistym cache tej maszyny ten sam mechanizm zmniejszył zawartość
z 171 793 363 138 do 21 473 101 600 bajtów przy limicie 20 GiB. Logiczny rozmiar
zawartości zmalał o 150 320 261 538 bajtów, czyli około 140 GiB, przez selektywną
ewikcję LRU. Nie jest to pomiar fizycznie zwolnionych bloków Btrfs.
Serwer uruchomiono ponownie po zakończeniu kompilacji; pierwsze rzeczywiste
wywołanie kompilatora uruchomiło redukcję zawartości.

Źródła: [konfiguracja sccache](https://github.com/mozilla/sccache/blob/v0.16.0/docs/Configuration.md),
[cykl życia serwera](https://github.com/mozilla/sccache/blob/v0.16.0/README.md),
[implementacja cache LRU](https://github.com/mozilla/sccache/blob/v0.16.0/src/lru_disk_cache/mod.rs).

## Aktualność bibliotek Androida

`tentaflow-mobile/android/scripts/build-rust.sh` sprawdza obecność wymaganych
bibliotek oraz ostatnie wpisy `llama-cpp-multi` i `zvec` w
`native-libs/<platform>/manifest.toml`. Commity muszą odpowiadać `LLAMA_CPP_REF`
i `ZVEC_REF` z `scripts/native-libs/common.sh`, używanym także przez producentów
bibliotek. Jawne wartości tych zmiennych z otoczenia obowiązują w obu miejscach.
Brak manifestu lub inny commit wymusza przebudowę bibliotek,
aby stary cache nagłówków nie zatrzymywał kompilacji aktualnego wrappera C++.
Wywołanie `cargo ndk` otrzymuje jawny `ANDROID_API_LEVEL` (domyślnie 26,
zgodnie z `minSdk`) oraz `--locked`, aby korzystać z wersji zapisanych w root locku.
Gradle pomija hashowane `libiroh-*.so` i `libiroh_relay-*.so`: Rust używa tych
bibliotek statycznie, a ich niezależne cdyliby kopiowane przez cargo-ndk nie są
bibliotekami JNI aplikacji. Dzięki temu stare kopie w katalogu wyjściowym nie
powiększają APK.
Filtr ABI wybiera spośród wspieranych architektur tylko te, dla których istnieje
`libtentaflow_mobile.so`. Brak JNI zatrzymuje scalanie bibliotek natywnych, a nie
samą kompilację Kotlin. CI sprawdza gotowy APK skryptem
`scripts/ci-local/check-android-apk.py`: każde pakowane ABI musi zawierać Core
oraz wszystkie jego niesystemowe zależności ELF. Zapobiega to deklarowaniu
architektur dodanych wyłącznie przez zależności AAR, bez biblioteki aplikacji.
Test decyzji o ponownym użyciu cache: `python3 scripts/ci-local/test-android-native-cache.py`.

## Wyniki pomiarów z 7 września 2026

Pomiary wykonano na Ryzen 9 7950X (16 rdzeni, 32 wątki), z 61,9 GiB RAM
widocznymi przez system i dyskiem NVMe. Repo leży na Btrfs z `noatime`
i kompresją `zstd:3`; ścieżka `/mnt/d` na tej maszynie nie oznacza WSL.
System używa swapu zram, więc odczyt `VmSwap` nie oznacza wymiany na dysk.
Kompilator to Rust 1.97.1. Sekcja `.comment` istniejącej binarki potwierdziła
**LLD 22.1.6 już przed zmianami** — przyspieszenie nie wynika z wymiany linkera.
Czas zbierano zegarem monotonicznym, a pamięć przez `resource.getrusage`
i próbki procesów `/proc`; nie sumowano RSS wszystkich procesów jako jednego szczytu.

Końcowa jednostka Cargo obejmuje również optymalizację LLVM i generowanie kodu.
Próbki `perf` z długiej końcówki pokazały pracę `rustc` w LLVM, m.in.
`GlobalOptPass`, usuwanie nieosiągalnych bloków i inlining. Fat LTO oraz jedna
jednostka generowania kodu ograniczały równoległość. Sam napis o końcowym
budowaniu binarki nie jest dowodem, że cały ten czas zajmuje linker.

Próbkowanie procesów co sekundę zobaczyło właściwy `rust-lld`, uruchamiany
przez `cc -fuse-ld=lld`, dopiero około 397,22–398,22 s z 399,22 s próby ThinLTO.
W pierwszym pełnym buildzie workspace wystąpił w próbce 783,57 s, tuż przed
końcem Cargo po 784,19 s. To nie jest dokładny stoper linkera, ale potwierdza,
że wielominutowa końcówka dotyczyła przede wszystkim pracy LLVM.
[Zapis procesów linkowania](/mnt/d/tentaflow-build-measurements/20260907/observed-linker-processes.json)
nie zawiera pomiaru mold; nie wyznaczono jego przewagi nad już używanym LLD.

### Porównanie samej końcowej kompilacji

ThinLTO uruchomiono ponownie na zapisanym poleceniu końcowego `rustc`, ze
sprawdzonymi niezmienionymi źródłami i zależnościami oraz wyłączną blokadą Cargo.
Obie próby używały `codegen-units=1`; zmieniono sposób LTO.

| Miara końcowej jednostki `rustc` | Fat LTO | ThinLTO |
| --- | ---: | ---: |
| Czas | 698,40 s | 399,22 s |
| Maksymalny RSS | 14,60 GiB | 14,49 GiB |
| Maksymalna zaobserwowana liczba wątków procesu | 4 | 38 |

Czas zmalał o **42,84%**, czyli około 1,75 raza. Pomiar nie wykazuje istotnego
spadku zużycia RAM. Liczba wątków procesu nie oznacza tylu stale zajętych rdzeni.
ThinLTO odtworzono bez jobservera Cargo, z równoległością dostępną na maszynie.
Część próby Fat nakładała się z innym `cargo test`; zarejestrowano również
wymianę z zram. Są to ograniczenia precyzji porównania, mimo zachowania wejść.

### Pierwszy pełny build po połączonych zmianach

Pełna próba używała `--release --no-default-features --features gpu-vulkan`.
Po zmianach profil release miał ThinLTO i cztery jednostki generowania kodu.
Jednocześnie zmieniono workspace i zależności, usunięto regenerowalne artefakty
z bundla oraz zastąpiono duże osadzane wartości `const` przez `static`.

| Miara | Przed zmianami | Po połączonych zmianach |
| --- | ---: | ---: |
| Cały Cargo | 2154,93 s (35 min 54,93 s) | 784,19 s (13 min 4,19 s) |
| Jednostka `tentaflow-core` | 904,93 s | 313,59 s |
| Końcowa jednostka `tentaflow` | 698,40 s | 159,48 s |
| Binarka release | 912,77 MiB | 339,36 MiB |
| Osadzony `container_bundle.tar.gz` | 594,90 MiB | 7,96 MiB |
| Metadane `tentaflow-core` w `lib.rmeta` | około 2,5 GiB | około 250 MiB |

To **nie jest czyste A/B zimnego buildu**: poprzedzająca udaną próbę kompilacja
zakończyła się błędem parsera po 124,04 s i rozgrzała część zależności. Zmian
było kilka, więc nie można przypisać całej różnicy ThinLTO ani jednemu
ustawieniu. Pierwotny build pomijał też cztery addony Pro z błędami wywołań
SDK; po naprawie ich wywołań wszystkie cztery zostały zbudowane. Te wyniki
nie mierzą wydajności uruchomionej aplikacji.

Pierwsza automatyczna retencja i obsługa wrappera dodały około **216,20 s**
po zakończeniu Cargo. Całe polecenie zakończyło się po 1000,39 s, czyli około
16 min 40 s. Tego kosztu nie należy zaliczać do czasu kompilacji ani zakładać,
że każde kolejne sprzątanie będzie kosztować tyle samo.

W tej retencji `target_shared` zmniejszył się z **898,68 do 247,68 GiB**.
Są to rozmiary logiczne plików, z każdym inode liczonym raz mimo hardlinków.
Osobno sccache zmniejszył się z około **160 do 20 GiB**. Tych różnic nie
sumujemy jako fizycznego odzysku miejsca: Btrfs stosuje kompresję, a pliki
mogą współdzielić bloki. Nie kasowano całego targetu przez `cargo clean`.

Lokalne archiwum dowodów znajduje się w
`/mnt/d/tentaflow-build-measurements/20260907/`. Zawiera
[porównanie pomiarów](/mnt/d/tentaflow-build-measurements/20260907/comparison.json),
[podsumowanie pełnego workspace](/mnt/d/tentaflow-build-measurements/20260907/workspace/summary.json),
[log Cargo i pierwszej retencji](/mnt/d/tentaflow-build-measurements/20260907/workspace/build.log),
[próbki LLVM z Fat LTO](/mnt/d/tentaflow-build-measurements/20260907/fat/final-llvm-perf-report.txt)
i [weryfikację wejść ThinLTO](/mnt/d/tentaflow-build-measurements/20260907/thin-replay/inputs-verified.json).
Nazwy katalogów identyfikują konkretne próby; dokumentacja nie wymaga tych
lokalnych ścieżek do normalnego budowania repo.

### Kolejne kompilacje i codzienna iteracja

Kolejne próby również używały wariantu Vulkan
(`--no-default-features --features gpu-vulkan`), a lokalne polecenia miały
`CARGO_BUILD_JOBS=4`. Nie jest to globalne ograniczenie zapisane w repo.
Czas wrappera poniżej zawiera Cargo, zapis historii i selektywną retencję.

| Próba | Cargo [s] | Całe polecenie [s] | `tentaflow-core` [s] | Końcowy `tentaflow` [s] |
| --- | ---: | ---: | ---: | ---: |
| Kolejny `release` po migracji | 669,175 | 677,24 | 312,59 | 170,14 |
| Pierwszy `release-fast` | 881,84 | 896,77 | 433,89 | 23,10 |
| `release-fast` po zmianie literału w kodzie Rust | 158,282 | 170,36 | 134,29 | 2,84 |
| Przywrócenie literału oraz poprawki generatorów | 161,92 | 171,71 | 134,77 | 3,29 |
| Końcowy build bez zmian | 0,689 | 8,054 | Fresh | Fresh |
| Build po zmianie ignorowanego artefaktu | 1,515 | 10,063 | Fresh | Fresh |

Pierwszy build nowego profilu budował jego zależności i dane incremental,
więc jego 14 min 42 s nie jest miarą kosztu zwykłej iteracji. W ciepłej próbie
zmieniono rzeczywisty literał w `tentaflow-core/src/crypto/mod.rs`, a następnie
przywrócono dokładną pierwotną treść. Podczas samej ciepłej próby nie wykryto
innych edycji źródeł. Obejmowała jednak jeszcze 19,51 s generowania zasobów
przed usunięciem pętli przebudów opisanych poniżej. Nie jest więc idealnym
izolowanym A/B samej zmiany literału. Przywrócenie połączono z poprawkami
skryptów budowania; nie służy do wyliczania oddzielnego przyspieszenia.

W kolejnej próbie `release` i podczas pierwszego `release-fast` trwały także
edycje użytkownika. Wyników tych nie należy przedstawiać jako porównania
identycznych, zamrożonych źródeł. Próby nie obejmują konfiguracji CUDA,
pozostałych zestawów cech ani natywnego budowania na Windows/macOS.

Binarka `release-fast` miała około 406 MiB. Maksymalny RSS końcowego procesu
wyniósł 2,16 GiB przy pierwszym buildzie oraz 1,33 GiB przy ciepłej iteracji;
czas i pamięć kompilacji całego core pozostają osobnym kosztem. Automatyczna
retencja po pierwszym buildzie profilu zmniejszyła target z 258,48 do
249,47 GiB rozmiaru logicznego, zachowując używany cache incremental.

Dwa końcowe wywołania pokazały `Fresh` zarówno dla aplikacji, core, jak i ich
skryptów budowania. Zmiana ignorowanego artefaktu nie zmieniła SHA binarki
ani wygenerowanego `services-manifest.js`; znacznik testowy został usunięty.
Koszt pełnego wrappera pozostał wyższy od samego Cargo, ponieważ obejmuje
kontrolę i porządkowanie cache.

Dowody: [ciepła iteracja](/mnt/d/tentaflow-build-measurements/20260907/fast-warm/summary.json),
[przywrócenie](/mnt/d/tentaflow-build-measurements/20260907/fast-restore/summary.json),
[końcowy Fresh](/mnt/d/tentaflow-build-measurements/20260907/fast-noop-verified/summary.json),
[ignorowany artefakt](/mnt/d/tentaflow-build-measurements/20260907/fast-ignored-noop/summary.json)
i [porównanie SHA](/mnt/d/tentaflow-build-measurements/20260907/ignored-artifact-result.json).
[Pełny raport eksperymentu](/mnt/d/tentaflow-build-measurements/20260907/RAPORT.md)
zawiera także komendy i ograniczenia pomiarów. Końcowe `--version` zakończyło
się kodem 0 i wypisało `tentaflow 0.1.0-beta`. Ostatnie poprawki pętli skryptów
zweryfikowano na `release-fast`; nie wykonywano dla nich kolejnego pełnego release.

### Przyczyny niepotrzebnych przebudów

Usunięto trzy potwierdzone pętle niezależne od LTO:

- Skrypt aplikacji obserwował całe `target/<profile>/build` i skanował dawne
  biblioteki llama w `out/lib`. Usunięto nieużywany skan i jego pomocnicze
  funkcje; bieżący build używa bibliotek przygotowanych w `native-libs`.
- Generator manifestu serwisów serializował trzy `HashMap` w losowej kolejności.
  Równe semantycznie dane dawały różne bajty JS, co zmieniało manifest zasobów
  i uruchamiało kolejną kompilację. Generowane typy używają teraz `BTreeMap`.
  Rzeczywista serializacja manifestu llama.cpp w ośmiu osobnych procesach dała
  wcześniej osiem różnych SHA, a po poprawce jeden identyczny SHA.
- Skrypt zawsze kopiował `tentaflow-meeting`, jednocześnie obserwując plik
  docelowy. Teraz kopiuje go tylko przy zmianie bajtów albo braku pliku.
  Test sprawdził zachowanie mtime dla identycznej treści, odtworzenie usuniętej
  kopii, aktualizację zmienionej treści i zachowanie praw wykonywania `0755`.

Wcześniej ograniczono również obserwację źródeł natywnych, aby `.build-*`
i targety nie wyzwalały generowania zasobów. Generator wasm-bindgen oraz JS
zapisują publikowane pliki tylko przy zmianie zawartości. Obserwacja WWW
pozostaje aktywna, aby wykryć również brak wygenerowanego pliku; po rzeczywistej
zmianie generatora może być potrzebna jednorazowa stabilizacja. Końcowe próby
Fresh potwierdziły ustanie powtarzających się przebudów.

[Raport deterministyczności](/mnt/d/tentaflow-build-measurements/20260907/manifest-determinism.json)
i [wykonany test kopiowania](/mnt/d/tentaflow-build-measurements/20260907/meeting-copy-probe.rs)
uzupełniają pomiary całej aplikacji.

## Końcowa walidacja i jej ograniczenia

Po ostatnich poprawkach zgodności API zwykły build głównej aplikacji
`release-fast` z wariantem Vulkan przeszedł w **157,772 s wraz z retencją**.
Końcowe `--version` również przeszło: kod 0, `tentaflow 0.1.0-beta`, 0,036 s.
To sprawdzenie aktualnego stanu źródeł, nie dodatkowy benchmark porównawczy.
Ostatnia retencja zmniejszyła logiczny rozmiar głównego targetu z 259,71 do
242,87 GiB. [Zbiorczy raport walidacji z komendami i logami](/mnt/d/tentaflow-build-measurements/20260907/targeted-tests/summary.md)
zawiera także wcześniejsze próby zakończone błędami migracji.

Wybrane testy Cargo dały **974 zaliczone i 2 niezaliczone testy**. Liczby
obejmują unikalne testy, bez dodawania powtórnych uruchomień do sumy:

| Zakres | Wynik |
| --- | --- |
| SDK specyfikacji | 709 zaliczonych |
| Addon Notes | 105 zaliczonych |
| Addon Go2 | 34 zaliczone |
| `forge-formats` | 122 zaliczone, 1 niezaliczony |
| Klient natywny FFI | 3 zaliczone, 1 niezaliczony |
| Strumieniowe SHA-256 pliku w Forge CLI | 1 zaliczony; wiele buforów i końcowy niepełny fragment, 131 195 bajtów |

Ponadto przeszły `cargo check` dwóch modułów browser WASM dla
`wasm32-unknown-unknown`, sprawdzenie Forge CLI/server/bridge z `--tests`
oraz osobne sprawdzenie `tentaflow-ui` z cechami `eframe/x11,eframe/wayland`.
Ostatnie potwierdza migrację API egui/eframe 0.34, bez deklarowania poprawności
całej aplikacji desktopowej.

Dwa pozostające niepowodzenia mają następujący zakres:

- `chat_template::tests::test_chatml_format` oczekuje znacznika ChatML użytkownika,
  którego nie ma w wyniku. Niepowodzenie odtworzono również na kodzie z HEAD
  w oddzielnym uruchomieniu przez `rustc`.
- `arch::tests::detect_qwen35moe_hybrid_metadata` dla lokalnego
  `qwen36-moe.gguf` oczekuje odrzucenia MTP MoE, a detektor akceptuje model.
  Pliki `arch.rs`, `gguf.rs` i opis RON są identyczne z HEAD. Nie wykonano
  jednak tego testu na pierwotnym grafie zależności, więc sama zgodność źródeł
  nie dowodzi, że aktualizacja zależności nie wpłynęła na wynik.

Pełny `tentaflow-desktop-linux` nadal zgłasza **14 błędów niezgodności API**
w integracji rdzenia: bazie danych, identyfikatorach flow/aktora,
uwierzytelnianiu i konfiguracji. Sześć odpowiednich plików źródłowych jest
bajtowo zgodnych z HEAD; zmiana warstwy egui nie naprawia tych zastanych
rozbieżności. Nie zmieniano domyślnych zasad aktorów ani uwierzytelniania,
aby dopasować je do starego wywołania. Cały desktop nie ma statusu PASS.

[Wyniki testów Forge](/mnt/d/tentaflow-build-measurements/20260907/targeted-tests/forge-formats-verified.json),
[FFI](/mnt/d/tentaflow-build-measurements/20260907/targeted-tests/native-ffi.json),
[dowody dla lokalnego modelu](/mnt/d/tentaflow-build-measurements/20260907/targeted-tests/forge-model-evidence.json)
i [dowody zgodności źródeł desktopu z HEAD](/mnt/d/tentaflow-build-measurements/20260907/targeted-tests/desktop-preexisting-source-proof.json)
pozwalają odróżnić wykonane sprawdzenia od ich ograniczeń.

Testy narzędzi Python: **35 zaliczonych, 2 pominięte ze względu na platformę**.
Obejmują walidator workspace, retencję i rzeczywiste blokady Cargo oraz eksport
samodzielnego workspace. Pominięte na Linux testy junction są przeznaczone dla
Windows. Walidator potwierdził 70 własnych pakietów w workspace; kontrola
`git diff --check` oraz pełne `cargo metadata --locked --offline` przeszły.
CI ma macierz Linux/macOS/Windows, ale podczas tej
sesji nie wykonano natywnych testów Windows ani macOS. Wyniki opisane powyżej
nie oznaczają zaliczenia całego workspace ani wszystkich platform.

Powyższe pomiary i walidacja opisują stan sprzed uporządkowania addonów:
wówczas obejmowały cztery lokalne dodatki Pro ignorowane przez Git. Następnie
Outlook, SharePoint RAG i Teams przeniesiono do wspólnego `tentaflow-core/addons/`,
a addon WASM `teams-bot` usunięto. Historyczna liczba 70 pakietów i wyniki
kompilacji odnoszą się do wcześniejszego układu. Natywny Meeting Bot pozostał
w `tentaflow-containers/agents/native/teams-bot/`.

## Jak porównywać czas kompilacji

Uruchamiaj pomiary z tym samym profilem, cechami i targetem. Pierwsza kompilacja
po zmianie ustawień optymalizacji może przebudować zależności; nie reprezentuje
zwykłej iteracji po małej zmianie w aplikacji. Nie wykonuj `cargo clean` pomiędzy
pomiarami przebudowy przyrostowej.

```bash
cd tentaflow
/usr/bin/time -v cargo build --release --timings
```

`/usr/bin/time -v` jest przykładem dla Linux. Samo `cargo build --timings`
działa także na macOS i Windows. Zachowaj flagi `--features` swojej normalnej
komendy. Raport Cargo rozdziela jednostki kompilacji, ale nie pokazuje osobno
czasu LLVM LTO i końcowego linkera. Do oceny końcówki potrzeba również pomiaru
procesów oraz zużycia RAM i swapu.

Fat LTO scala reprezentację kodu Rust na potrzeby optymalizacji między
crate'ami. ThinLTO używa podsumowań modułów i pozwala przetwarzać je osobno.
Wcześniej skompilowane statyczne biblioteki C/C++, takie jak lokalna llama.cpp,
nie podlegają automatycznie rustowemu LTO; wspólna optymalizacja Rust i C++
wymaga osobnej konfiguracji bitcode oraz linker-plugin LTO. W dołączonej
llama.cpp opcja `GGML_LTO` ma domyślnie wartość `OFF`
([CMakeLists.txt](../vendor/crates/llama-cpp-sys-2/llama.cpp/ggml/CMakeLists.txt)),
a `build.rs` tej biblioteki jej nie włącza.

Repo nie wymusza `target-cpu=native`. Wcześniejsza lokalna konfiguracja
`tentaflow-voice/.cargo/config.toml` została usunięta, więc wejście do tego
katalogu nie zmienia już flag kompilatora i zestawu cache. Eksperyment
profilowania konkretnego CPU można wykonać jawnie przez `RUSTFLAGS`, pamiętając,
że wynik może wymagać instrukcji niedostępnych na innych maszynach.

## Utrzymanie workspace

`python3 scripts/check-cargo-workspace.py` sprawdza, czy wszystkie własne
pakiety należą do workspace, a profile, źródła zależności i ich wersje są
zdefiniowane tylko w korzeniu. Wszystkie addony pochodzą ze wspólnego
`tentaflow-core/addons/`. Testy walidatora uruchamia się poleceniem
`python3 scripts/test-cargo-workspace.py`. Wymagane są Python 3.11+, Git i Cargo.
Workflow `cargo-workspace.yml` uruchamia walidator, jego testy oraz testy
retencji i blokad cache na Linux, macOS i Windows po zmianach manifestów,
konfiguracji i związanych z nimi skryptów.

Addony Rust są jawnymi członkami workspace, w tym `outlook`, `sharepoint-rag`
i `teams`. Dodając nowy addon Rust, dopisz jego katalog do `members` w głównym
`Cargo.toml`; walidator zgłosi brak przed buildem. Nie ma osobnego katalogu
ani wyjątków workspace dla dodatków Pro.

Nie należy wpisywać globu kończącego się `/Cargo.toml` do `members`: Cargo
może zaakceptować manifest korzenia, pomijając takie dopasowania w rzeczywistej
liście pakietów. Walidator porównuje członków z wynikiem `cargo metadata`.

[Pełny audyt zależności](cargo-dependencies-audit.csv) obejmuje 197 zewnętrznych
pakietów: wersję w korzeniu, rolę, deklarujących odbiorców i decyzję dotyczącą
podobnych bibliotek. Nie wszystkie podobne nazwy oznaczają zbędną duplikację: 
np. kod CBOR serde i kodek jawnie indeksowanego SDK mają różne kontrakty wire,
a Wasmtime i wasmi obsługują różne klasy platform.

## Przenośne fragmenty workspace

`scripts/export-workspace.py` tworzy samodzielny podzbiór workspace z
wybranych pakietów i ich zależności ścieżkowych. Manifest, profile i źródła
zależności pochodzą z głównego `Cargo.toml`; resolver otrzymuje root lockfile,
a eksport sprawdza, czy rozwiązane wersje i sumy kontrolne należą do tego
samego katalogu wersji. Nie ma drugiego ręcznie utrzymywanego zestawu wersji
dla Dockera ani SDK.

Eksport domyślnie działa offline. `cargo update --workspace --offline` przycina
kopię root lockfile do eksportowanych pakietów bez pobierania ich źródeł;
kontrola wersji, źródeł i sum kontrolnych pozostaje obowiązkowa. Sam eksport
potrzebuje metadanych indeksu Cargo, ale nie pełnego cache paczek dla innych
platform. Pełne `cargo metadata --offline` wymagałoby także takich źródeł,
których wcześniejsza kompilacja pojedynczego targetu nie pobrała.

```bash
# Samodzielny kontekst sidecara poza repo.
python3 scripts/export-workspace.py --output /tmp/tentaflow-sidecar-context \
  --archive /tmp/tentaflow-sidecar-context.tar.gz --member tentaflow-containers/sidecar

# Szablon addonu wraz z bibliotekami wymaganymi do budowania poza repo.
python3 scripts/export-workspace.py --output /tmp/tentaflow-addon-context \
  --archive /tmp/tentaflow-addon-context.tar.gz --member tentaflow-core/addon-sdk/template
```

Na macOS w tych przykładach użyj `/private/tmp` zamiast `/tmp`, ponieważ
systemowe `/tmp` jest dowiązaniem. Eksporter wymaga fizycznych ścieżek
źródeł, katalogu roboczego i archiwum; odrzuca także dowiązania w ich
przodkach i eksportowanych źródłach.

Build aplikacji używa tego samego eksportera do archiwum kontenerów. Filtr
odrzuca przed kopiowaniem m.in. katalogi `target`, `.build-*`, środowiska
Pythona, lokalne wyniki kontenerów i pliki `.env`. Archiwum kontenerów oraz
lista wbudowanych addonów są generowane jako `static`, aby ograniczyć
powielanie dużych zasobów w metadanych biblioteki jako wartości stałych.
Przenośnym wynikiem jest archiwum wskazane przez `--archive`; `--output`
określa katalog roboczych metadanych eksportu. `--in-place` służy do zawężania
już skopiowanego kontekstu Dockera.

Zmiana istniejących plików zasobów i root manifestu jest śledzona przez build.
Po dodaniu samodzielnego nowego zasobu, który nie zmienia żadnego śledzonego
pliku, można wymusić regenerację przez `touch tentaflow-core/build.rs`.
W PowerShell odpowiednikiem jest
`(Get-Item tentaflow-core/build.rs).LastWriteTime = Get-Date`.
