# Changelog

Najważniejsze zmiany w TentaFlow.
Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) /
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### TentaNas

- Lista dozwolonych targetu iSCSI/NVMe-oF jest zapamiętywana w bazie
  (migracja 30: `nas_targets.allowlist_mode` i `nas_targets.open_confirmed`).
  Target z listą dozwolonych, której ostatni wpis usunięto, jest **zamknięty
  dla wszystkich**: nikt nowy się nie zaloguje (także po restarcie węzła czy
  przebudowie grupy portali). Na iSCSI sesje usuniętych klientów są zrywane;
  na NVMe-oF hosty połączone w tej chwili zachowują dostęp, dopóki nie połączą
  się ponownie (aby odciąć je od razu, zatrzymaj target).
  Usunięcie ostatniego wpisu (w edytorze listy albo „Rozłącz … i usuń z listy”)
  nie jest już odrzucane (`refusal:target_last_initiator` wysyłają tylko
  starsze węzły) i wymaga potwierdzenia na ekranie.
- „Otwórz dla wszystkich”: jedyna świadoma droga od listy dozwolonych do
  targetu otwartego, z ostrzeżeniem i przepisaniem nazwy targetu. Usuwa listę
  i nieużywane wpisy w jądrze; odblokowuje też target, który jądro trzymało
  zamknięty wbrew zapisowi. Niedostępne dla NVMe-oF z DH-HMAC-CHAP.
  Protokół: `TargetOpenRequest` (`target_id`, `confirm_name`,
  `expected_updated_at` — okno, które widziało starszą wersję targetu, dostaje
  `refusal:target_open_changed`), `TargetGetResponse.open_blocked`,
  `NasTarget.allowlist_mode`.
- Nowe alerty: `target_left_disabled` (krytyczny: target po zerwaniu sesji
  pozostał wyłączony), `target_open_refused` (jądro trzyma listę dozwolonych,
  której nie ma w zapisie), `target_sessions_reset` (ostrzeżenie: wszystkie
  sesje targetu zresetowano raz, bo klient spoza listy był zalogowany).
- Eksport konfiguracji z zamkniętym targetem o pustej liście ma schemat 2 —
  węzeł starszej wersji odrzuca taki plik w całości, zamiast otworzyć target.
  Pozostałe eksporty mają nadal schemat 1.
- `tentanas-helper` ma wersję `0.17.3`; operacje na targetach wymagają tej
  wersji (bramka wersji odrzuca inną) — po aktualizacji ponów nadanie uprawnień
  systemowych.
- Aktualizacja: kolumna „Czas trwania” sesji targetów zaczyna się jednorazowo
  od nowa dla sesji aktywnych w chwili aktualizacji (klucz sesji zawiera teraz
  wcielenie grupy portali, aby zatrzymanie i wznowienie targetu nie przedłużało
  starej sesji).

## [0.4.0-beta.1] — 2026-09-29

Zmiany od tagu `v0.4.0-beta`. Pełny opis zmian od `v0.3.0-beta` znajduje się
w [sekcji 0.4.0-beta](#040-beta--2026-09-27). Poprzednia próba wydania
nie zakończyła się publikacją artefaktów; ten tag uruchamia ponowną kompilację
wszystkich platform. Wynik kompilacji należy sprawdzić w GitHub Actions.

### TentaNas

- Dodano alerty utraty nasłuchu RDMA i rozpoznawanie połączeń NVMe-oF RDMA.
  Alerty zachowują stan między odczytami, a spóźnione wyniki diagnostyki
  nie nadpisują aktualnego stanu targetu.
- Kreator targetów ostrzega o niedostępnym iSER i blokuje niedostępne opcje.
  Wykrywanie urządzeń RDMA uwzględnia interfejsy VLAN i bond; odczyt GID
  wykonuje się poza asynchronicznym wątkiem wykonawczym.
- Ograniczono ujawnianie informacji o peerach należących do innych organizacji.

### TentaBus

- Topik ma opis i autora. Opis (do 500 znaków) podaje się w pierwszym kroku
  kreatora „Nowy topik” albo w oknie „Zmień” karty „Opis” w Ustawieniach
  topiku; strona topiku pokazuje go pod nazwą, a bez opisu — jak dotąd rodzaj
  treści i wzór. Karta „Opis” pokazuje też, kto utworzył topik (nazwę osoby
  albo klucza API). Autor jest zapisywany z sesji przy tworzeniu i nie zmienia
  go żadna aktualizacja; topiki utworzone przed tą wersją nie mają autora.
  Protokół: `BusTopicOptionsWire.description`,
  `BusTopicConfigWire.{description, created_by, created_by_label}`.
- Wycofanie pojedynczej wersji wzoru wiadomości: `SchemaDeleteRequest`
  z `version` i `deprecate_only` (oraz REST `DELETE …?version=N&deprecate_only=true`)
  oznacza tę wersję jako wycofaną zamiast odrzucać żądanie. Topik sprawdza
  wiadomości najwyższą niewycofaną wersją; gdy wycofany jest cały wzór albo
  wszystkie jego wersje, sprawdza dalej ostatnią wersją. Wycofany wzór nadal
  nie przyjmuje nowych wersji ani nowych powiązań z topikami. Lista wersji
  podaje `deprecated_at_ms` (`BusSchemaVersionWire`, REST), a wpis audytu
  `bus.schema.deprecate` podaje numer wersji.
- Aktualizacja: migracja bazy 176 dodaje `bus_topics.description`,
  `bus_topics.created_by`, `bus_schema_subjects.deprecated_versions_json`
  i `bus_schema_subjects.generation` (wcielenie wzoru: wzór usunięty
  i zarejestrowany ponownie jest nowym wzorem, do którego nie trafia nic,
  co dotyczyło poprzedniego, łącznie z jego wersjami),
  `bus_schema_versions.subject_generation` (wcielenie, pod którym
  zarejestrowano wersję; wersja nowego wcielenia, która dotrze przed nim,
  czeka na nie, a wersja usuniętego jest pomijana) oraz tabelę
  `bus_schema_subject_tombstones` (usunięty wzór nie wraca po spóźnionej
  zmianie z innego noda). Czasy wycofania i usunięcia wersji są znaczone
  zegarem synchronizacji (HLC), nie zegarem ściennym noda.
  Opis i autor synchronizują się w wierszu topiku (ostatni zapis wygrywa,
  autor zostaje ustalony przy tworzeniu). Wycofania wersji i wycofanie całego
  wzoru synchronizują się jako suma: dwa jednoczesne wycofania na różnych
  nodach przetrwają oba, a zapis z noda, który jeszcze nie widział wycofania,
  go nie cofa; wersja usunięta na stałe nie wraca jako wycofana. W klastrze
  z nodami w starszej wersji ich zmiany topiku i wzoru nie czyszczą opisu ani
  wycofań zapisanych na nowszych nodach, a wycofania wysłane przez starszy
  node nie trafiają do wzoru znanego nowszym nodom jako konkretne wcielenie; starsze nody same nie znają opisów
  ani wycofanych wersji (dla nich wzór sprawdza zawsze najnowszą wersję),
  więc do czasu aktualizacji wszystkich nodów instancji walidacja może się
  między nimi różnić. Opis spoza limitów tej wersji (np. dłuższy niż
  500 znaków) jest przy synchronizacji skracany.
- Wzory wiadomości w panelu: wiersz listy otwiera stronę wzoru, a
  administrator instancji dodaje wzór („Dodaj wzór”: nazwa, zgodność, format
  z tych, które serwer sprawdza, tekst wpisany albo wczytany z pliku) i usuwa
  wzór, którego nie używa żaden topik — przy używanym zamiast kosza jest
  kłódka, która po kliknięciu podaje topiki. Strona wzoru pokazuje format, wersję, stan, kto i kiedy
  go dodał, topiki i zgodność, tekst wybranej wersji tylko do odczytu
  z „Kopiuj” i „Pobierz” oraz opis zapisany w samym wzorze (`description`
  JSON Schema, `doc` Avro), wersje z „Wycofaj” przy każdej aktywnej i kartę
  zgodności z „Zmień”. Okna „Nowa wersja”, „Zmień zgodność”, „Wycofaj wzór”,
  „Wycofaj wersję” i „Usuń…” mówią przed potwierdzeniem, według której
  wersji topiki będą sprawdzać wiadomości. Odmowę nowej wersji przez
  zgodność panel opisuje zwykłymi słowami (np. „nowa wersja wymaga pola
  „pilne”, którego stare wiadomości mogą nie mieć”), a nieznany powód
  pokazuje z tekstem serwera w zwijanym bloku; tekst odrzuconej wersji
  wraca przy następnej próbie. Wycofany wzór ma ostrzeżenie „Wzór wycofany”
  i nie przyjmuje nowych wersji. Gdy wzór o tej samej nazwie powstał w
  międzyczasie, „Dodaj wzór” mówi, że dodano do niego wersję (albo że nic
  się nie zmieniło), zamiast ogłaszać nowy wzór. Adres strony wzoru:
  `#/tentabus?instance=…&tab=schemas&subject=…`.
- Edytor kodu w panelu (`tf-code-editor`) po podmianie całego tekstu
  pokazywał wiersze poprzedniego dokumentu, gdy miał on tyle samo linii;
  teraz rysuje nowy tekst.

- Nieprzetworzone wiadomości: usunięto ograniczenia opisane w wydaniu 0.4.0-beta.
  „Wczytaj więcej” nie powtarza wierszy przy wielu partycjach. Ponowienie
  i odrzucenie wykonuje tylko node prowadzący partycję nieprzetworzonych
  wiadomości, a oznaczenie obsługi trafia do każdej jej kopii, także tej, która
  była niedostępna. Ponowienie przerwane po zapisaniu wiadomości w topiku
  nie zapisze jej drugi raz. „Ponów wszystkie” zaczyna od najstarszych
  wiadomości ze wszystkich partycji, a wiadomość znów odrzuconą przez wzór
  liczy jako nieponowioną. Tekst błędu odbiorcy jest ukryty przed osobami,
  które obejmują zasady ukrywania danych topiku. Lista czyta tylko wiadomości,
  które pokazuje, i trzyma z nich jedynie podgląd treści, a „Ponów wszystkie”
  czyta ograniczoną liczbę wiadomości na raz i podaje, ile wiadomości
  odrzuconych przy zapisie zostawiło na liście, by można je było odrzucić.
- Ponowiona wiadomość, którą wzór topiku znów odrzuca, trafia z powrotem do
  tej samej partycji nieprzetworzonych wiadomości. Jeśli jej nowej kopii nie
  uda się zapisać, ponowienie jest odmawiane, a wiadomość zostaje na liście —
  wcześniej znikała. Liczba wiadomości skierowanych do nieprzetworzonych przy
  zapisie (`schema_rejected`) obejmuje teraz tylko te, których kopię zapisano.
  Wiadomości, których kopii nie udało się zapisać — utracone, ani
  opublikowane, ani wśród nieprzetworzonych — podaje nowe pole
  `schema_dropped` w odpowiedzi REST publikacji, w wyniku funkcji hosta
  `bus_publish_v1` (SDK Rust i .NET) oraz w metadanych bloku przepływu
  (`bus_publish_schema_dropped`).
- Ponowienie, które reguła jednorazowego dostarczenia topiku (klucz
  idempotencji) odrzuciłaby jako powtórzenie oryginału, jest odmawiane,
  a wiadomość zostaje na liście — wcześniej znikała z listy, nie wracając do
  topiku. Można ją ponowić po upływie okna tej reguły.
- Nagłówki `dlq.*` dodane przez producenta nie przechodzą do kopii
  nieprzetworzonej wiadomości, więc nie podszyją się pod dane szyny.
- Oznaczenie obsługi przechodzi teraz strumieniem replikacji partycji
  nieprzetworzonych wiadomości: w klastrze, w którym część nodów ma jeszcze
  wersję 0.4.0-beta, oznaczenia nie docierają do kopii do czasu aktualizacji
  wszystkich nodów.
- Znane ograniczenia:
  - Ponowienie wymaga, by ten sam node prowadził partycję nieprzetworzonych
    wiadomości i partycję topiku, do której wiadomość wraca; zapis nie jest
    przekazywany między nodami. W przeciwnym razie ponowienie jest odmawiane
    z nazwą noda prowadzącego partycję topiku.
  - Ochrona przed powtórnym zapisem przerwanego ponowienia działa na nodzie
    prowadzącym partycję topiku; jeśli między przerwaniem a kolejnym
    ponowieniem prowadzenie tej partycji przejmie inny node, wiadomość może
    trafić do topiku drugi raz.
  - Oznaczenie obsługi jest wysyłane do kopii co 0,5 s i nie czeka na ich
    potwierdzenie. Jeśli prowadzenie partycji nieprzetworzonych wiadomości
    przejmie inny node, zanim oznaczenie do niego dotarło (utrata noda
    w tym oknie albo kopia niepołączona od chwili oznaczenia do zmiany
    prowadzenia), ponowiona lub odrzucona wiadomość wraca na listę na nowym
    nodzie i może zostać ponowiona jeszcze raz, także przez „Ponów wszystkie”.
    Nowy prowadzący nie pobiera oznaczeń od pozostałych kopii.

### Poprawki

- Kompilacja zvec dla iOS wybiera SDK hosta macOS dla narzędzi uruchamianych
  podczas budowania. iOS nie należy do macierzy artefaktów tego wydania.
- Numer wersji aplikacji i pakietów dziedziczących wersję workspace ustawiono
  na `0.4.0-beta.1`; Android używa `versionCode = 5`.
- Usunięto błąd kompilacji parsera diagnostyki RDMA na Windows. Rodziny adresów
  IPv4 i IPv6 są rozpoznawane według linuksowego ABI netlink, niezależnie od
  systemu, na którym działa parser.
- Dodano artefakt diagnostyczny kompilacji Androida z logiem kompilacji oraz
  pomiarami pamięci RAM, swapu i miejsca na dysku, ułatwiający analizę przerwań.

## [0.4.0-beta] — 2026-09-27

Zmiany od tagu `v0.3.0-beta` do `v0.4.0-beta`. Wydanie testowe.
Tag `v0.3.0-beta` nie zakończył się publikacją artefaktów wydania na GitHubie.
Dla instalacji z ostatniego opublikowanego wydania `v0.1.0-beta` obowiązują
również opisane poniżej zmiany i uwagi aktualizacyjne wersji 0.2.0 oraz 0.3.0.

### Aktualizacja i zgodność

- **Zaktualizuj wszystkie węzły TentaBus, zaczynając od replik podrzędnych,
  a kończąc na liderach.** Pełna ochrona zatwierdzonych wiadomości oraz zasada
  jednego lidera na epokę wymagają nowej wersji na każdym węźle. Starsze węzły
  nie zapisują epok ani zatwierdzonych offsetów i nie składają trwałych obietnic
  wyborczych; podczas aktualizacji mieszany klaster nie ma pełnych gwarancji.
  Nowy lider może wstrzymać widoczność odtworzonych wiadomości do potwierdzenia
  jego kadencji przez większość nowych replik lub zapisu nowej wiadomości przez
  większość. Stare partycje zaczynają z zatwierdzonym offsetem 0, co zwiększa
  zakres ponownej synchronizacji.
- Migracje bazy obejmują zwalnianie nazw usuniętych workspace'ów (171), generacje
  tematów i porządkowanie pozostałości TentaBus (172–174) oraz trwałe przydziały
  epok replik (175). Partycje zapisują epoki w `partition.epochs` i zatwierdzony
  offset w rozszerzonym `partition.meta`.
- Narzędzia plikowe agentów zwracają teraz `blob_id` i przyjmują
  `expected_blob_id`, zamiast `sha256` i `expected_sha256`. To identyfikator
  obiektu Git kopiowany z wyniku odczytu. Własne prompty i wywołania narzędzi
  wymagają dostosowania; niezmodyfikowany prompt dostarczany z aplikacją jest
  aktualizowany automatycznie.
- `tentanas-helper` ma wersję `0.17.2`; nowe operacje i diagnostyka wymagają
  aktualnego helpera na zarządzanym węźle.

### TentaBus

- Nowe ekrany tematów, odbiorców oraz nieprzetworzonych wiadomości: filtrowanie,
  kreator tematu, podgląd wiadomości, usuwanie z potwierdzeniem nazwy,
  szczegóły ustawień i partycji, opóźnienia odbiorców, wstrzymywanie odczytu
  oraz ponawianie i odrzucanie nieprzetworzonych wiadomości.
- Przesuwanie miejsca czytania według numeru, początku, końca lub czasu pokazuje
  liczbę wiadomości do powtórzenia albo pominięcia. Działa także dla aktywnego
  odbiorcy: stare potwierdzenie nie cofa zmiany, a nowe pobranie używa nowej pozycji.
- Widok „Kopie i nody” pokazuje liderów, repliki, problemy i historię zmian.
  Nowe partycje otrzymują liderów; zmiany ustawień trafiają od razu do innych
  węzłów. Usunięto ręczne przestawianie zestawu replik w interfejsie.
- Uprawnienia tematów obejmują widoczność odbiorców, statystyki, informacje
  o replikach i przenoszenie lidera. Zmiany uprawnień aplikacji działają od razu,
  także po synchronizacji z innego węzła. Usunięcie tematu usuwa jego reguły,
  zasady ukrywania danych i kolejkę nieprzetworzonych wiadomości.
- Ponowne utworzenie tematu o tej samej nazwie tworzy nową generację:
  stare dane i liderzy nie przechodzą do nowego tematu.
- Poprawiono wybory lidera, rozstrzyganie rozbieżnych logów, blokowanie zapisów
  przez odsuniętego lidera oraz natychmiastowe przekazywanie zatwierdzonego
  offsetu replikom. Dla co najmniej trzech replik wybory wymagają większości;
  odtworzone wiadomości wcześniejszej kadencji pozostają niewidoczne, dopóki
  większość nie potwierdzi nowej kadencji.
- Epoka identyfikuje jednego lidera dzięki stałym slotom i trwałym obietnicom
  wyborczym. Dodanie repliki wymaga lidera partycji; limit wynosi 64 przydzielone
  sloty na generację tematu, bez ponownego używania slotów usuniętych węzłów.
  Po wyczerpaniu limitu trzeba odtworzyć temat. Wybory mogą potrwać dłużej
  o losowe opóźnienie do jednej czwartej dzierżawy lidera i dodatkowe oczekiwanie
  na odpowiedź wyborczą (300 ms).
- `acks=all` czeka na żywe, zsynchronizowane repliki, przy co najmniej trzech
  kopiach zachowując wymóg większości. Dla dwóch kopii ocalała replika przyjmuje
  `acks=leader` i `acks=all`; `acks=quorum` nadal wymaga obu.

### TentaNAS

- Rozbudowano diagnostykę RDMA/iSER: wykrywanie urządzeń, rzeczywistego nasłuchu
  oraz transportu aktywnych sesji. Kreator targetów weryfikuje interfejs sieciowy;
  ekran sesji rozróżnia połączenia iSCSI i iSER oraz pozwala rozłączać iSCSI.
- Zaostrzono listy dozwolonych inicjatorów i zamykanie dynamicznych uprawnień
  po przejściu z otwartego dostępu na listę dozwolonych. Dodano wykrywanie
  zdalnych LUN-ów i ochronę operacji niszczenia przed przekroczeniem organizacji.
- Wyłączanie i odinstalowywanie dodatku pokazuje konsekwencje na poszczególnych
  węzłach, status dostępności i potwierdzenia. Zatrzymanie udostępniania oraz
  wyłączenie mogą wymagać zatwierdzenia przez drugą osobę; ponowne włączenie
  przywraca udostępnianie. Odliczanie potwierdzenia pochodzi z węzła.
- Poprawiono raportowanie użytecznej pojemności, liczby targetów, zajętości
  folderów, znanych pul i niedostępnych węzłów. Dodano rozpoznawanie AnyRAID
  oraz odłączanie dysku z puli ZFS.
- Poprawiono zadania rozruchowe, scrub i SMART; dodano zbiorcze zadanie SMART
  oraz przekazywanie alertów organizacji z kontrolą adresów chroniącą przed SSRF.
- Komunikaty odmowy, stanów i harmonogramów mają kody oraz parametry tłumaczeń.
  Dialogi i logi używają czytelnych nazw, a diagnostyka RDMA otrzymała tłumaczenia.

### Code Studio i agenci

- Kolejne tury sesji zachowują rozmowę. Nowa sesja przypina bieżącą, zapisaną
  wersję procesu agentowego, a podagenci otrzymują pierwotne zadanie użytkownika.
- Następne rundy planowania, implementacji i recenzji otrzymują wcześniejsze
  uwagi krytyka oraz wyniki testera. Odmowa zawierająca frazę „BEZ UWAG” nie
  zostaje uznana za akceptację; awaria wszystkich oczekiwanych podagentów
  kończy proces z rzeczywistą przyczyną.
- Usunięto limit 50 uruchomień na sesję oraz domyślny godzinny termin zakończenia
  orkiestratora. Własne limity operatora są zachowane, a pętle recenzji korzystają
  ze swoich budżetów. Odpowiedź ucięta przez limit tokenów nie oznacza sukcesu.
- Strumień pokazuje odpowiedź agenta, trwające myślenie, czas i aktualne narzędzie.
  Akceptacja zmian kończy oczekującą recenzję; pytanie nie zasłania całej rozmowy.
  Usunięcie workspace'u zwalnia jego nazwę.
- Na Linuksie wykrywanie sandboxa sprawdza rzeczywiste uruchomienie `bwrap`.
  Administrator może naprawić profil AppArmor z dashboardu, również na zdalnym
  węźle, podając hasło sudo. Błędy logowania CLI pokazują końcówkę wyjścia
  procesu po usunięciu sekwencji terminala i zamaskowaniu sekretów.

### Modele, RAG i inferencja

- Natywne osadzanie modeli GGUF do embeddingów i rerankingu przez llama.cpp
  rzeczywiście ładuje model, obsługuje ranking par zapytanie–dokument i zwalnia
  model przy zatrzymaniu. Modele pomocnicze mogą działać obok załadowanego LLM.
- Wyszukiwanie wektorowe pomija nieistniejące pola projekcji bez usuwania filtrów;
  błędy wykonania flow zachowują pełny łańcuch przyczyn.
- Rozmowy przekazywane przez mesh zachowują wymianę narzędzi i raportują zużycie
  tokenów. Parser rozpoznaje DSML DeepSeek, także warianty mieszane z JSON
  i argumenty zagnieżdżone w pojedynczym polu `arguments`.
- W `tentaflow-infer` dodano integrację CoreML/Apple Neural Engine do obliczeń
  prefill, eksport odpowiednio przygotowanych modeli oraz pomiary na Apple
  Silicon. Ta ścieżka wymaga wyeksportowanych modeli CoreML; nie oznacza
  automatycznej obsługi dowolnego modelu przez ANE.
- Poprawiono tensor parallelism CUDA i kernele NVFP4 oraz odświeżono receptury
  konfiguracji vLLM.
- Dodano dane, benchmarki i skrypty przygotowania, ewaluacji oraz treningu QLoRA
  dla adaptacji modeli do języka polskiego. Są to narzędzia treningowe,
  a nie nowe wytrenowane wagi dostarczane z aplikacją.

### Platformy i zarządzanie węzłami

- W procesie wydania wydłużono przechowywanie artefaktów bibliotek natywnych
  z jednego do siedmiu dni, aby pozostawały dostępne dla późniejszych etapów
  kompilacji. Numer wersji aplikacji Android ustawiono na `0.4.0-beta`.
- Linux: sherpa-onnx i pozostałe moduły korzystają ze współdzielonego ONNX Runtime,
  co usuwa konflikt ABI powodujący awarię startu `free(): invalid pointer`.
  Pełne archiwa zawierają `libonnxruntime.so.1`; wariant GPU ARM64 korzysta
  z ONNX Runtime 1.30.0 dla CUDA 13.
- macOS: budowanie bibliotek natywnych wybiera SDK aktywnego Xcode.
- Zdalne zatrzymywanie wdrożenia klastrowego trafia do węzła z jego rekordem;
  wydłużono czas na zdalne operacje usług. Widok mesh zachowuje rodzaj węzła
  i oznaczenie operatora.
- Poprawiono tabele na telefonach, okna modalne i ukrywanie `tf-alert`.

### Znane ograniczenia

- Wydanie obejmuje Linux x86_64 i ARM64, macOS Apple Silicon, Windows x86_64
  oraz Android ARM64. Nie zawiera kompilacji dla iOS ani macOS Intel.
  Dostępne warianty akceleracji zależą od platformy i załączonych archiwów.
- APK Android jest podpisany kluczem debug i przeznaczony do instalacji ręcznej.
  Przyszła wersja z innym kluczem podpisu nie zaktualizuje go w miejscu.
- TentaBus z dwiema replikami preferuje dostępność: podział sieci może pozostawić
  dwóch liderów. Po połączeniu wygrywa nowsze przywództwo, a nieprzesłane zapisy
  przegranej strony przepadają. Utrata jedynej pozostałej repliki również może
  oznaczać utratę zatwierdzonych tylko przez nią wiadomości.
- Wstrzymanie odbiorcy i cofnięcie jego miejsca czytania nie są utrwalane na
  wszystkich replikach i nie przetrwają zmiany lidera.
- Lista nieprzetworzonych wiadomości może powtarzać wiersze przy stronicowaniu
  wielu partycji. Ponowienie/odrzucenie nie wymaga jeszcze lidera, a oznaczenie
  obsługi nie dociera do każdej kopii. Awaria między ponownym publikowaniem
  a zapisaniem oznaczenia może pozwolić na powtórne ponowienie wiadomości.
  Ponawianie zbiorcze nie gwarantuje kolejności od najstarszej wiadomości między
  partycjami; ponownie odrzucony zapis może zostać policzony jako ponowiony.
  Tekst błędu odbiorcy nie podlega regułom ukrywania pól. Odczyt listy przegląda
  także obsłużone rekordy w swoim oknie, co może zwiększać koszt odczytu.

[Pełne porównanie zmian 0.3.0 → 0.4.0](https://github.com/Slyb00ts/TentaFlow/compare/v0.3.0-beta...v0.4.0-beta).

## [0.3.0-beta] — 2026-09-24

Zmiany od tagu `0.2.0-beta`. Tagi `0.2.0-beta` i `0.3.0-beta` zostały
przygotowane, ale ich procesy wydania nie zakończyły się publikacją artefaktów.
Ostatnim opublikowanym wydaniem pozostało `0.1.0-beta`; przy aktualizacji
obowiązują również poniższe uwagi wersji `0.2.0-beta`.

### Upgrade notes

- **Update every mesh node together.** The binary protocol moved from schema 29
  to 32; an older and a newer node reject each other's handshake.
- **Sign in to agent CLI accounts again.** Agent CLI accounts now live in the
  provider-account registry. Every migrated account starts as "sign-in
  required": credentials stored in the old places (the Code Studio content
  database and the bridge's on-disk logins) are **not** carried over, because
  they cannot be decrypted with another node's key. At startup the node logs how
  many credential rows it removed, per node and engine and in total, so the
  loss of old entries never looks like silent data loss.
- **TentaBus partitions damaged by the old segment-roll bug no longer open.** A
  partition whose segment files were misnamed by that bug now fails with
  `SegmentOffsetMismatch` instead of being silently truncated; recover it by hand
  from a healthy replica.

### Windows

- Release archives for Windows x86_64 — `slim`, `full-vulkan` and `full-cuda13`
  — built in CI by the same `setup.ps1` → `build-all.ps1` → `build.ps1` scripts
  a developer runs. Each archive carries its Visual C++ runtime (and cuBLAS for
  `full-cuda13`); CI starts every archive from a bare `PATH` before publishing.
- `install.ps1` installs TentaFlow as the `TentaFlow` Windows service: explicit
  edition choice, verified download, the GStreamer runtime the `full` editions
  need (checksum-verified), a virtual service account, automatic start with
  restart on failure, firewall rules and a data directory only the service and
  administrators can read. `uninstall.ps1` removes it (`-Purge` also removes
  the data).
- `tentaflow start|stop|restart|status` drive the Windows service, and
  `tentaflow update` updates a Windows installation, including a newer
  GStreamer runtime when the release needs one.
- CI installs, upgrades, drives and uninstalls every Windows archive on a clean
  runner before a release is published.
- Host telemetry on Windows: GPU utilisation and VRAM (DXGI + PDH, independent
  of the UI language), disks, and network interface details now report real
  values instead of zeros.

### Android

- The Android debug APK (arm64-v8a) is attached to the release. It is
  debug-signed: it installs by sideloading, and a future properly signed build
  will not upgrade it in place.

### TentaNAS and storage

- Elastic Array: isolated mounts for new arrays, a durable service mode, a
  verified single-disk cache tier and a mover that relocates files from the
  cache to the data disks automatically, on a schedule or on demand — without
  freezing the share.
- Scheduled sync and scrub (every array is scrubbed monthly); a failed parity
  run is recorded as a cause instead of wedging the array, a Sync over a fault
  needs an explicit acknowledgement, and an unfinished disk add can be resumed
  or undone.
- Repair, grow and dissolve an array; share and adopt arrays between nodes;
  export an array over NFS; clear a disk only behind a namespace-proof guard
  and a retyped name.
- Disk health: correct SMART and NVMe self-test decoding, one self-test per
  disk at a time, failures that reach the health verdict, and a dashboard that
  tells a failing disk from a warning.
- Multi-tenancy: jobs, alerts, disks, approvals, share sources and targets are
  scoped to the asking organisation; internal configuration rows stay hidden
  from tenants.
- The UI names nodes, users and disks instead of showing identifiers, and
  patches the dashboard, pool and array views in place instead of rebuilding
  them.

### TentaBus

- Package 1: field policies reach the dead-letter queue, per-action ACLs,
  instance isolation, quota enforcement and validation.
- Schema-registry REST API with per-action API-key scopes; the compliance
  retention floor applies to bus topics.
- Replication: followers acknowledge on their own cadence and the leader waits
  without a thread per publish, raising quorum throughput from about 1k to about
  220k messages/s. Fixed a segment-roll bug that could make consumers skip
  records, and leader-term races (term rollback, duplicate leaders after
  failover, unauthenticated leader handshakes). The replication transport is
  hardened and the failover audit names the instance.
- Lag without side effects, truthful deprecation, lag history, and the first
  TentaBus screen (shell and overview).

### Agents, Code Studio and provider accounts

- Provider accounts: an on-demand runtime, CLI sign-in with a login GUI,
  sessions, shared accounts and credentials that sync between nodes.
- The account card no longer claims an account has no sessions when it only
  sees this node's. It tells apart an account homed on this node, one homed on
  another reachable node (named, with its own subset) and one homed on a node
  that does not answer ("no data" instead of an empty list).
- Coding agents run on Linux and Windows; only the Code Harness that Code
  Studio actually runs is kept; agents without a model get a chat model.

### Robotics (Go2)

- Cloud onboarding, the `data2=3` handshake with a per-device AES key and GPU
  camera anonymisation with tighter privacy regions.
- Shared map: a persistent occupancy model, frame carving, chunk storage, a
  control plane and relocalisation against the shared map by branch-and-bound
  scan matching.

### Inference, vision and voice

- ONNX Runtime never falls back to the CPU with device-bound inputs, and cuDNN
  is preloaded for the CUDA provider.
- Addon model requirements can be installed from the addon's settings.
- GPU detection runs in a child process, so a crashing driver cannot take the
  node down.
- A bundled Jarvis voice clone for Supertonic TTS.

### Mesh and sync

- Baseline adoption and node-log catch-up no longer block between nodes; a
  donor that refuses a fuller requester adopts from it instead; a replicated
  flow version whose number is already taken locally is kept.
- Security and mesh audit findings closed.

### Build

- The `slim` edition compiles again; Arch Linux setup and macOS build errors
  are fixed; generated browser assets are no longer tracked in Git.
- Native library and toolchain versions live in one file,
  `scripts/versions.env`, read by every build script on every platform.

## [0.2.0-beta] — 2026-09-08

The main changes since the last published release, `0.1.0-beta`.

### TentaNAS and storage

- A new application for managing disks, ZFS pools, datasets, snapshots, network
  shares and block storage across the fleet, with schedules, an access audit, a
  restricted privileged helper and second-administrator approval for selected
  operations.
- Elastic Array joins data and cache disks through mergerfs with periodic
  SnapRAID parity. Operations run with a durable intent record; data moves off
  the cache, sync and scrub run on demand, mounts can be restored, and data still
  waiting for parity protection is visible.

### Applications, TentaBus and agent accounts

- Multiple application instances with separate data, permissions and lifecycle.
  TentaBus gained a durable log, replication, a schema registry, field policies
  and integration with flows, REST and the SDK; instance isolation and recovery
  after a leader change were fixed.
- Code Studio manages agent CLI accounts, isolated working directories and moving
  accounts between nodes. TentaVM foundations: a host registry, capability
  probing, and granting and requesting access.

### TentaQuant

- A new quantum-circuit studio and notebook with an OpenQASM 3 subset parser,
  CPU/WGPU simulation, in-browser execution through WASM and Qiskit export. Jobs
  can be cancelled, stream their results, visualise the state, and compare and
  export results.

### Mesh, clusters and processing

- The Mesh list shows the local node, devices discovered over mDNS and trusted
  nodes, keeping transitive trust; the relay does not create a global device
  directory.
- iroh update and reconnection fixes after a restart or an address change.
  Cluster sync uses a change log, and model recovery takes peer availability and
  startup time into account.
- One way of handling the default conversation flow. Node configuration refreshes
  correctly in the editor; selected voice and image-processing paths were fixed.

### Build, dependencies and cache

- One Cargo workspace, a shared lockfile and central dependency versions and
  profiles, checked by CI. Source exports keep self-contained container and SDK
  contexts; unused dependencies were removed and code adapted to new APIs.
- ThinLTO for releases, an incremental `release-fast` profile and automatic
  artefact retention in the shared scripts; less duplicated output and fewer
  needless rebuilds. [Measurements and cache rules](docs/build-performance.md)
  are documented separately.

### Addons and integrations

- Outlook, SharePoint RAG and Teams moved into the shared addons directory. The
  separate `teams-bot` WASM addon was removed; the native Meeting Bot remains.

### Installation and distribution

- An explicit Full/Slim choice also when the installer is piped. A new
  configuration has mesh enabled and HTTPS reachable from the LAN; the Linux
  system installer opens the ports in an active UFW/firewalld, keeps an existing
  configuration and warns about a loopback bind.
- The macOS Metal archive requires the Meeting Bot: before publishing, the
  workflow checks it is present, executable, built for the right architecture,
  has its dependencies and starts with `--help`.

## [0.1.0-beta] - 2026-09-02

### Added
- Installer for Linux and macOS (`curl … | sh`) with an explicit edition choice: hardware
  detection proposes, the user decides. Installs into `/opt/tentaflow/versions/<ver>` behind a
  `current` symlink, keeps configuration and data across updates, registers a systemd unit
  (Linux) or a LaunchDaemon (macOS) and starts it.
- `tentaflow start|stop|restart|status` — status reports service state, autostart, PID, config
  and probes `/health`.
- `tentaflow update` — own updater over GitHub Releases: mandatory checksum verification,
  whole-version-directory swap, previous version kept for rollback, service restarted only if it
  was running.
- `tentaflow init-config` — writes the default configuration from `NodeConfig`, pinning all three
  listeners to the chosen bind address.
- `slim` distribution edition (`--no-default-features`): gateway, mesh, flows, dashboard, addons
  and containers with no local inference engine. Its catalog keeps every cloud provider and the
  utility infrastructure — 21 entries instead of 94.
- CI `verify` job installs the built archive on a runner with real systemd and asserts autostart,
  liveness and `/health` before publishing; `scripts/ci-local/` reproduces the whole release build
  and the install test locally, across Ubuntu, Debian, Fedora and Arch.

### Changed
- ROCm/HIP removed from the application: AMD and Intel run on the portable Vulkan path, the same
  one Burn uses for vision. NVIDIA keeps CUDA.
- Release builds on Ubuntu 22.04, making glibc 2.35 / GLIBCXX 3.4.30 the supported floor; the
  installer refuses to install below it instead of leaving an unrunnable service enabled.
- Whisper is target-gated, not only feature-gated, so `full` on Apple no longer links whisper.cpp
  next to MLX.
- Vision model runners moved to `vision/runners.rs`; they are shared by the flow vision node,
  local CV and the inference batcher, and no longer live behind the camera feature.

### Fixed
- `mesh.frame_rejected` from unpaired peers no longer floods the audit log (it was 98.5% of
  1.15M rows).
- Reasoning deltas are forwarded over the mesh reverse stream.
- Mesh peer trust survives pairing, boot prune and `persisted_version` domains.

## [0.0.2-alpha] - 2026-04-23

### Added
- Added the new `www/` dashboard SPA and migrated the app to the binary WebSocket protocol with generated browser codecs.
- Added the service manifest registry, universal service catalog, and deploy wizard for Docker, native, and external engines.
- Added embedded and bundled deployment flows for AI engines, including live deployment progress streaming and shared model storage.
- Added a full meeting-bot stack with protocol support, database persistence, per-session container lifecycle, and dedicated frontend screens.
- Added multi-hop mesh topology propagation, route awareness, peer liveness tracking, and richer model and service visibility across nodes.
- Added QR-based pairing flows for mobile and tablet devices, including camera scanning and invite/PIN confirmation improvements.
- Added mobile-focused improvements across iOS and Android, including native discovery integration, QR scanning fallback, and mobile web packaging groundwork.
- Added installer and release automation, including GitHub Releases, packaged artifacts, and install scripts for Unix and Windows.
- Added IAM foundations with users, groups, role metadata, and resource permission protocol and handler support.

### Changed
- Switched the main dashboard static asset pipeline from `wwwroot/` to `www/`.
- Reworked deployment execution to use manifest-driven jobs and streamed deployment status instead of the older direct service deploy path.
- Upgraded the deploy wizard UI from simple radio inputs to richer option cards and per-GPU selection controls.
- Expanded mesh model and topology views so the UI can show backend, size, route, and peer-derived fallback data more consistently.

### Fixed
- Fixed native embedded deploys so `llama.cpp`, `MLX`, and `Whisper` create persistent service records, reappear in `Services`, and restore correctly after app restart.
- Fixed iOS and Xcode build issues around toolchain setup, Metal platform support, and mobile startup behavior.
- Fixed container bundle deployment path resolution and Docker build context handling for manifest-based deploys.
- Fixed multiple mesh pairing and discovery regressions, including duplicate connect/disconnect events, pairing completion handling, peer identity propagation, and reconnect behavior.
- Fixed mobile window sizing, fullscreen handling, and QR scanner error behavior.

## [0.0.1-alpha] - 2026-04-14

First public alpha. Everything listed below has been implemented,
compiled on Linux x86_64 + RTX 4090, and test-bootstrapped.

### Added — deploy and containers
- Generic `tentaflow-sidecar` crate (role-based QUIC bridge) with
  built-in keep-alive, idle detection, graceful shutdown. 7
  integration tests cover request/response, server shutdown notifying
  clients, client disconnect, handler errors, parallel streams, and
  long-idle keepalive.
- `ReverseProxy` sidecar role translating `ModelRequest` ↔ OpenAI /
  llama.cpp / sherpa / raw HTTP, with SSE → CBOR stream passthrough.
- Dockerfile + config + entrypoint for every model container:
  `llm-llamacpp`, `llm-vllm`, `llm-sglang`, `llm-ollama`, `stt-whisper`,
  `stt-parakeet`, `stt-qwen-asr`, `tts-sherpa`, `tts-xtts`, `tts-voxcpm`,
  `embeddings`, `reranker`, `comfyui`.
- `tentaflow-core/build.rs` embeds the container contexts as a single
  `tar.gz` (~26 MB) so a vanilla tentaflow binary can build and run any
  of them without git clone.
- `tentaflow-core/src/deploy/` module: `bundle::extract_to`,
  `docker::deploy` (bollard build + run), REST endpoints
  `GET /api/deploy/containers` and `POST /api/deploy/<name>`.

### Added — Docker-free deploy (Python bundles)
- `tentaflow-containers/python-bundles/` with one `bundle.toml` per
  engine (vLLM, SGLang, XTTS, VoxCPM, Parakeet, Qwen-ASR, ComfyUI) that
  pins python version, source (git head or pypi), launch command with
  `${MODEL}` / `${VENV_DIR}` substitution, required platforms, and per-
  backend install variants (CUDA / ROCm 7 / Metal / XPU).
- `deploy::python_venv::bootstrap` and `deploy::python_venv::deploy`:
  downloads `python-build-standalone` and `uv` into
  `~/.cache/tentaflow/`, creates a venv, installs the engine with the
  correct `--extra-index-url` and extras, then spawns it. All 7 bundles
  bootstrap end-to-end on a host with only system Python 3.14 present.
- Upstream compatibility fixes: `install_subdir` (SGLang's `python/`),
  `install_mode = "requirements_txt"` (ComfyUI), `extras_no_build_isolation`
  (flash-attn needs torch to be installed first), and a defensive
  `patch_pyproject_if_needed` that strips the `license` field so both
  old and new setuptools can build the cloned repos.

### Added — Docker-free deploy (native C/C++ binaries)
- `tentaflow-containers/native-binaries/` build scripts for
  llama.cpp, whisper.cpp, sherpa-onnx, text-embeddings-inference, and
  stable-diffusion.cpp. Each script auto-detects CUDA / Metal / Vulkan /
  CPU and produces a tarball of binary + required shared libs.
- Successful builds on the reference host: `llama-server` (CUDA, 27 MB),
  `whisper-server` (CUDA, 2 MB), `sd-server` (CUDA, 58 MB), sherpa CLI
  bundle (CPU, 36 MB).

### Added — system detection
- `system_check::collect()` reports CPU features (AVX2/AVX512/NEON), RAM,
  NVIDIA GPUs (via `nvidia-smi`), AMD GPUs (via `rocminfo` and
  `/opt/rocm/.info/version`), Intel XPU (via `sycl-ls`), Metal, Vulkan,
  plus runtime versions (`docker`, `podman`, `python`, `nvcc`).
- `GpuBackend` enum with `preferred_backend` resolution
  (CUDA → ROCm → Metal → XPU → CPU) used by `pick_install_variant`.
- Per-engine capability matrix returned to the GUI wizard so users see
  what will and will not run on their hardware.
- REST endpoint `GET /api/system/capabilities`.
- `cargo run --example system_check` CLI helper.

### Added — GUI integration
- `ws_deploy.rs` recognises both backends: for engines mapped to an
  embedded container it builds and runs via `deploy::docker::deploy`; if
  `deploy_mode == "native"` it hands off to `deploy::python_venv::deploy`.
  Falls back to legacy `docker compose` path when the engine is not
  recognised.
- Respects every wizard field by parsing the wizard's generated
  `compose_yaml` — container name, ports (TCP/UDP mix), volumes, env
  (`HF_TOKEN`, `MODEL_ID`, `GPU_MEMORY_UTILIZATION`, `GGUF_PATH`,
  `shm_size`) and GPU selection.
- LLM deploy wizard GPU picker replaced with a multi-checkbox dropdown
  — users can target any subset of their cards; the compose emits
  `device_ids: ['0','4']` and the sidecar passes `NVIDIA_VISIBLE_DEVICES`
  through.
- Three unit tests covering GPU multi-select + compose parsing.

### Added — meeting bot persistence
- Transcripts are now stored in SQLite (tables `meeting_sessions` and
  `meeting_transcripts`) instead of process memory or a JSONL file.
  Survives restart, indexed by `(session_id, timestamp_ms)`.
- Endpoints `GET /api/meeting-bot/sessions`,
  `GET /api/meeting-bot/sessions/{id}/transcripts`,
  `GET /api/meeting-bot/sessions/{id}/download`.
- Meeting bot GUI panel: download button fetches the full session;
  transcript list re-renders incrementally without resetting scroll.
- Speaker match thresholds retuned to cut false positives
  (`MATCH_CONFIDENT 0.55`, `MATCH_VERY_CONFIDENT 0.70`, strict
  `is_match()`, `INCREMENTAL_LEARN_THRESHOLD 0.65`, tracker
  similarity 0.50).

### Added — release pipeline
- `.github/workflows/release.yml`: tag `v*` triggers a matrix build
  (`x86_64-linux`, `aarch64-linux`, `aarch64-macos`, `x86_64-windows`)
  and publishes a GitHub Release with tarballs, SHA-256 sidecars,
  `install.sh`, and `install.ps1`. Tags with `-alpha`/`-beta`/`-rc`
  are marked as pre-release automatically.
- `scripts/install/install.sh` + `install.ps1` one-liner installers
  that detect platform, download the archive, verify SHA-256, install
  to `/opt/tentaflow` (or user path), and register auto-start via
  systemd / launchd / Scheduled Task.
- `scripts/release.sh` helper that bumps `tentaflow/Cargo.toml`, adds
  a CHANGELOG section, commits, tags, and pushes.
- `tentaflow update [--check|--force]` subcommand using `axoupdater` to
  swap the running binary from the latest GitHub Release.
- `RELEASING.md` documents the whole flow.

### Added — shutdown hardening
- SIGTERM + SIGINT both handled in `tentaflow/src/main.rs`.
- Unified HTTPS server now selects on the service-manager shutdown
  channel, so port 8090 is released immediately instead of sitting in
  `TIME_WAIT`.
- `MetricsCollector` background tasks join on the shutdown channel
  instead of looping forever.
- `db::checkpoint_wal` invoked on exit so SQLite WAL is flushed before
  the process dies.

### Changed
- Container images use `FROM rust:slim-bookworm` (no pinned Rust
  version) so sidecar builds always use the current stable toolchain.

### Fixed
- `tentaflow-voice` build no longer requires a system `protoc`; the
  build script falls back to `protobuf-src` when `PROTOC` is not set.
