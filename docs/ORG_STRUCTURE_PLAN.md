# Struktura organizacyjna — plan platformowy (rewizja po wstrzymanym „O1”)

> **Rewizja 0.6** (2026-09-29: mechanizm anonimizacji i retencji §6.4; po przeglądzie krytyka: semantyka dat i strefa czasowa, jednoznaczna projekcja, algorytm eskalacji, import, odejście pracownika, RODO, zakres organizacji; eDokumenty przesunięte na koniec jako dodatek; zastępca kierownika tylko pod nieobecność kierownika; zapis tylko administratorzy, eDokumenty wyłącznie dla urlopów, inicjały → zdjęcia dodawane przez użytkowników, zastępcy kierowników opcjonalni, strukturę widzą wszyscy z dostępem do programu). Odgrzebanie i przeprojektowanie struktury organizacyjnej, zaczętej jako
> „O1” w planach CRM (`tentaflow-core/addons/crm/plans/01-platform-org-structure.md`, mockup
> `mockups/crm-v1/o01-org-structure.html`) i nigdy niezbudowanej. Ten dokument **zastępuje** tamten
> plan. Większość jego decyzji zostaje (stanowiska, macierz, historia przypisań, zdarzenia `org.*`,
> zapis tylko przez uprawnionych). Zmiany i powody są w §0.
>
> Konsumenci od pierwszego dnia:
> - **Projekty** — widoczność utylizacji, eskalacje SLA, zastępstwa, nieobecności
>   (`PROJECT_STUDIO_WORKFLOW_PLAN.md` §4.2, §14),
> - **uprawnienia** (`sync_user_org_profiles`, zakresy `department` / `manager_subtree`),
> - **zatwierdzenia** (TentaNas, CRM).
>
> Stan faktyczny repo (2026-09-29):
> - **działa:** katalog ról (`role_catalog`, pełny stos), organizacje i członkostwa, płaskie grupy,
>   rekurencyjne sprawdzanie `manager_subtree` w silniku uprawnień (`db/repository.rs:15866`);
> - **w połowie:** `sync_user_org_profiles` (dział, przełożony) z funkcją zapisu, której **nic nie
>   wywołuje**; `department_id` to wolny tekst bez tabeli działów; w `roles_catalog.js:270-279`
>   cztery zakładki (Drzewo / Lista / Widoczność / Historia) wyłączone;
> - **brak:** stanowisk, jednostek, linii raportowania, przypisań z historią, zastępstw,
>   nieobecności, synchronizacji z katalogiem (AD/Entra) i widoku graficznego.

---

## 0. Decyzje (co zmieniamy względem O1 i dlaczego)

1. **Struktura jest częścią platformy (core), a nie planem addonu CRM.** Korzystają z niej Projekty,
   uprawnienia i zatwierdzenia. Addony dalej konsumują ją przez `org.*` i zdarzenia, jak w O1.
2. **Jedna encja jednostek zamiast `departments` + `sections`.** `org_units` z typem (pion / dział /
   zespół / sekcja / dowolny własny) i zagnieżdżeniem o dowolnej głębokości. Dwie tabele wymuszały
   stałe dwa poziomy, a firmy mają ich różną liczbę.
3. **Osoba = konto użytkownika platformy** (`user_accounts`), a nie `contacts.persons` z addonu
   Contacts. Core nie może zależeć od addonu. Osoby spoza systemu (kontraktor bez konta, wakat
   z kandydatem) są wpisem `external_person` bez logowania.
4. **Dochodzą zastępstwa i nieobecności.** Bez nich eskalacja SLA trafia w pustkę (przełożony na
   urlopie), a zatwierdzenie stoi. O1 ich nie miał.
5. **Struktura ma daty obowiązywania (effective dating).** Każda linia raportowania, przypisanie
   i przynależność do jednostki ma `valid_from` / `valid_to`. Daje to widok „stan na dzień”
   (kto był przełożonym rok temu — RODO, audyt, spory o eskalację) i **reorganizację planowaną**
   (zmiany z datą przyszłą, podgląd „stan na 1.01”). O1 odkładał to na „kiedyś, trigger z audytu”.
6. **Zapis: wyłącznie administratorzy** (decyzja 2026-09-29, zgodna z O1). Uprawnienie `org.admin`
   jest częścią roli administratora. Delegacji na kierowników ani na kadry nie ma.
7. **Strukturę prowadzą ręcznie administratorzy, a eDokumenty dostarczają wyłącznie urlopy**
   (decyzja 2026-09-29) — jako **dodatek na samym końcu** (S6). Do tego czasu nieobecności wpisuje
   się ręcznie, a nic w aplikacji od eDokumentów nie zależy. Synchronizacja AD/Entra jest ewentualną późniejszą opcją, a nie planem v1 (§5).
8. **`sync_user_org_profiles` zostaje jako projekcja.** Jest przeliczana ze struktury (dział główny,
   przełożony główny), dzięki czemu istniejący silnik uprawnień (`manager_subtree`, `department`)
   zaczyna działać bez przepisywania.
9. **Dwa tryby widoku:** prezentacja (czyste drzewo do pokazywania i przeglądania, domyślny dla
   wszystkich) i edycja (dla `org.admin`, z przeciąganiem). Mockup O1 łączył je w jeden edytor
   w stylu Flow Buildera, z portami na każdej karcie — do pokazywania struktury jest to za gęste.
10. **Model ma obsłużyć dowolną strukturę, a nie jeden szablon firmy** (§1.1): kierownik jednostki
    z zastępcami albo bez nich, stanowiska sztabowe (asystent, pełnomocnik) poza linią, jednostki
    dowolnego typu i głębokości, opcjonalne linie funkcjonalne.

## 1. Model danych

| Tabela | Pola kluczowe | Uwagi |
|---|---|---|
| `org_units` | id, org_id, name, code, **type**, parent_unit_id, color, head_position_id, valid_from/to | dowolna głębokość; typy konfigurowalne (słownik) |
| `org_unit_deputy_heads` | unit_id, position_id, order, valid_from/to | 0..n zastępców kierownika jednostki, w kolejności |
| `org_positions` | id, unit_id, name, role_id (→ `role_catalog`), is_manager (z roli, nadpisywalne), **is_staff**, valid_from/to | stanowisko istnieje także jako **wakat**; `is_staff` = sztabowe, rysowane z boku |
| `org_reporting_lines` | position_id, parent_position_id, **kind** (`primary` / `functional`), priority, valid_from/to | dokładnie jedna linia `primary` w danym dniu; `functional` = macierz (kropkowana) |
| `org_assignments` | id, position_id, user_id **albo** external_person_id, type (`permanent` / `acting` / `contractor`), **share** (część etatu), **is_primary**, valid_from/to | historia; osoba może mieć kilka stanowisk; suma `share` = wymiar pracy dla planowania zasobów |
| `org_deputies` | user_id (zastępowany), deputy_user_id, scope (`all` / `approvals` / `escalations` / `project:<id>`), valid_from/to | zastępstwo czasowe albo stałe |
| `org_absences` | user_id, from/to, kind (`leave` / `training` / `other`), source (`manual` / `edokumenty` / …), external_id | powodu nieobecności **nie zapisujemy** (decyzja 2026-10-09); pozostali widzą tylko „niedostępny” (§6.3) |
| `org_sources` + `org_field_provenance` | skąd pochodzi każda wartość, kiedy zsynchronizowana | pierwszeństwo źródeł i konflikty (§5) |
| `org_change_requests` (opcjonalnie) | zaplanowana reorganizacja: zestaw zmian z datą wejścia, stan, zatwierdzający | §2.4 |

**Daty obowiązywania — semantyka:**
- typ `DATE` (bez godziny), przedział **półotwarty** `[valid_from, valid_to)`; `valid_to = NULL` = bezterminowo;
  `CHECK (valid_to IS NULL OR valid_from < valid_to)`,
- „dzień” liczony w **strefie czasowej organizacji** (pole organizacji, domyślnie `Europe/Warsaw`),
  tej samej, której używa kalendarz roboczy Projektów; zmiana strefy przelicza projekcję,
- zmiana przypisania „od 1.03” = stary wpis `valid_to = 2026-03-01`, nowy `valid_from = 2026-03-01`
  — bez luki i bez nakładania,
- nakładanie się przedziałów tam, gdzie reguła mówi „dokładnie jeden” (linia `primary` stanowiska,
  kierownik jednostki), jest sprawdzane w **transakcji zapisu** (SQLite nie ma ograniczeń
  wykluczających), a zmiany wsteczne (data w przeszłości) wymagają potwierdzenia i trafiają
  do audytu jako korekta. **Nieobecności i zastępstwa z datą początku wcześniejszą niż dziś (w strefie
  organizacji) wpisuje wyłącznie administrator** (decyzja 2026-10-09): pozostali zaczynają od dziś, a serwer
  odmawia (`backdating_admin_only`) także z `confirm_backdated`; administrator nadal je potwierdza. Osoba
  kończy trwającą nieobecność wcześniej, ustawiając koniec na dziś — nie usuwa jej.

Walidacja:
- brak cykli w liniach `primary` w żadnym dniu (sprawdzane dla przedziałów dat, nie tylko „dziś”),
- dokładnie jeden przełożony główny na stanowisko w danym dniu,
- suma `share` osoby > 1,0 to ostrzeżenie (a nie błąd — bywają nadgodziny i okresy przejściowe),
- jednostka bez kierownika to ostrzeżenie w widoku.

**Projekcja** `sync_user_org_profiles` jest przeliczana:
- przy każdym zapisie struktury (dla osób, których zmiana dotyczy),
- zadaniem harmonogramu (`scheduler/`) o 00:05 w strefie organizacji, dla zmian datowanych
  wchodzących w życie tego dnia — wykonuje je jeden węzeł (lider synchronizacji), a pozostałe
  dostają wynik przez replikację,
- na żądanie administratora („Przelicz”), np. po imporcie.

Osoba z kilkoma stanowiskami ma jedno oznaczone jako **główne** (`org_assignments.is_primary` —
dokładnie jedno w danym dniu, wybierane przez administratora; przy jednym stanowisku ustawiane
samo). Projekcja bierze wyłącznie stanowisko główne: `department_id` = jednostka stanowiska głównego,
`manager_user_id` = osoba na stanowisku przełożonego głównego (albo jej zastępca w zakresie
`all`, jeśli stanowisko jest nieobsadzone), `is_department_manager` z `head_position_id`.
Osoba bez przypisania głównego nie ma wiersza w `sync_user_org_profiles` — dział i przełożony
pochodzą wyłącznie ze struktury.

### 1.1 Uniwersalność — jak model opisuje różne organizacje

| Potrzeba | Jak to wyrazić |
|---|---|
| Dyrektor **z zastępcą** (albo kilkoma) | jednostka ma **kierownika** (`head_position_id`) i **0..n zastępców kierownika** w kolejności (`org_unit_deputy_heads`). Zastępca to zwykłe stanowisko w jednostce, podległe kierownikowi. W eskalacjach i zatwierdzeniach działa **wyłącznie pod nieobecność kierownika** |
| Dyrektor **bez zastępcy** | pusta lista zastępców — nic więcej nie trzeba |
| Zastępca stały a zastępstwo czasowe | **zastępca kierownika** (stanowisko w strukturze, stały) ≠ **zastępstwo** (`org_deputies`: osoba za osobę na czas urlopu). Eskalacja: kierownik nieobecny → pierwszy obecny zastępca kierownika → zastępstwo czasowe → przełożony wyżej |
| Stanowisko **sztabowe** (asystent zarządu, pełnomocnik ds. jakości, IOD) | flaga `is_staff`: raportuje do przełożonego, ale na drzewie jest rysowane **z boku linii** i nie ma podwładnych w linii |
| Zarząd wieloosobowy / współprowadzący | jednostka z kilkoma równorzędnymi stanowiskami kierowniczymi; eskalacja do wszystkich albo wg kolejności — ustawienie jednostki |
| Płaska mała firma | jedna jednostka, właściciel jako kierownik, wszyscy mu podlegają |
| Struktura macierzowa (opcjonalnie) | linia `functional` (kropkowana) obok `primary`; domyślnie ukryta w widoku i **nie wpływa na eskalacje** (chyba że jednostka tak ustawi) |
| Własne typy jednostek | słownik typów per organizacja (pion, dział, zespół, sekcja, oddział, biuro…) z kolorem i ikoną |
| Stanowisko nieobsadzone | wakat: widoczny, liczony w statystykach, pomijany w eskalacjach |
| Kontraktor / osoba zewnętrzna | przypisanie typu `contractor`; osoba z kontem w programie widzi strukturę (§6.1) |

Szablony startowe struktury (do edycji po wczytaniu): „Mała firma”, „Firma z działami”, „Firma
z pionami i zastępcami”. Szablon tworzy wyłącznie jednostki i stanowiska, bez osób.

## 2. Operacje i kontrakt dla aplikacji

### 2.1 Host functions / API (rozszerzenie O1)

- odczyt z datą (`at?`): `org.get_unit`, `org.list_units(parent?)`, `org.get_position`,
  `org.get_assignment(user, at?)`, `org.get_reports_chain(user|position, up|down, at?)`,
  `org.get_subordinates(transitive, at?)`, `org.is_subordinate_of(user, manager, at?)`;
- **nowe dla konsumentów:**
  - `org.get_manager(user, at?)` z uwzględnieniem zastępstw,
  - `org.escalation_chain(user, scope)` — kolejne osoby do eskalacji: przełożony → (gdy nieobecny)
    jego zastępcy kierownika w kolejności → zastępstwo czasowe → przełożony wyżej; wakaty
    i nieobecni są pomijani,
  - `org.is_available(user, at)`,
  - `org.can_view_person_data(viewer, subject, kind)` (§6),
  - `org.get_structure_as_of(at)` (migawka drzewa na dzień — Historia, eksport),
  - `org.list_change_sets()`, `org.preview_change_set(id)` (reorganizacja planowana, §2.4).

Wszystkie funkcje działają w kontekście **organizacji z sesji** (`org_id`). Użytkownik należący do
kilku organizacji widzi tylko strukturę tej, w której kontekście pracuje, a każda tabela ma `org_id`.

### 2.1a Algorytm łańcucha eskalacji (`org.escalation_chain`)

```text
eskaluj(osoba, dzień):
  stanowisko := stanowisko główne osoby w dniu
  odwiedzone := {}
  powtarzaj (maks. 20 poziomów — ochrona przed błędem danych):
    przełożony := stanowisko nadrzędne w linii primary (w dniu); brak → koniec łańcucha
    jeśli przełożony w odwiedzonych → przerwij i zgłoś błąd struktury administratorowi
    kandydaci := [osoba na stanowisku przełożonego]            (wakat lub konto nieaktywne = brak)
    jeśli kandydat obecny (org.is_available)          → zwróć go jako kolejny krok łańcucha
    jeśli przełożony jest kierownikiem jednostki:
        dla zastępców kierownika w kolejności: pierwszy obecny → zwróć go
    zastępstwo czasowe osoby przełożonego (zakres pasuje) i zastępca obecny → zwróć zastępcę
    wszyscy nieobecni → idź poziom wyżej (stanowisko := przełożony)
```

- **Współprowadzący** (kilku równorzędnych kierowników): zależnie od ustawienia jednostki łańcuch
  zwraca wszystkich obecnych naraz albo pierwszego obecnego wg kolejności.
- **Linie funkcjonalne** nie biorą udziału (chyba że jednostka włączy to jawnie).
- **Łańcuch liczony jest w chwili eskalacji, a nie zapamiętywany** przy zadaniu. Zmiana struktury
  w trakcie biegu SLA działa od najbliższego kroku eskalacji, bez przeliczania wstecz, a wpis
  w historii zadania pokazuje, do kogo i dlaczego poszło.
- Testy: kierownik i wszyscy zastępcy nieobecni → poziom wyżej; wakat na drodze; konto
  dezaktywowane; cykl w danych (odrzucony przy zapisie, ale funkcja i tak się nie zapętli).
- **Realizacja (S3, `services/org_structure/{availability,escalation,privacy}.rs`):**
  - jeden poziom łańcucha zwraca JEDNĄ osobę (posiadacz → zastępca kierownika → zastępca czasowy);
    ta sama osoba nie wraca drugi raz, a pytający nigdy nie jest własną eskalacją; poziom bez odpowiedzi
    trafia do `skipped` (`vacant` / `unavailable`, bez powodu), błąd struktury do `problem` (`cycle`, `too_deep`);
  - zastępstwo czasowe działa tylko za osobę z aktywnym kontem i pasującym zakresem (`all` pokrywa każdy
    zakres, pozostałe tylko własny); zastępca kierownika ma pierwszeństwo przed zastępstwem czasowym;
  - **daty końca są wyłączne** (`[od, do)`), także w nieobecnościach — okno mówi „ostatni dzień” i przelicza
    na brzegu;
  - nieobecność **nie rusza rzutowania uprawnień**: `get_manager` zwraca zastępcę (zakres `all`), ale
    `manager_user_id` w profilu wynika wyłącznie ze struktury (przy wakacie na stanowisku kierownika
    jednostki — z jej pierwszego zastępcy kierownika);
  - nieobecność **nie ma powodu** — nie jest pytany, zapisywany, synchronizowany ani pokazywany
    (decyzja 2026-10-09); daty widzi osoba, jej przełożeni w linii głównej i administratorzy; ewidencję czasu — osoba i przełożeni (poddrzewo). Nie zamodelowani: PM projektu
    i zarząd (§6.3) oraz współprowadzący jednostkę (§1.1) — jednostka ma jeden `head_position_id`.

### 2.2 Zapis (tylko `org.admin`)

`create_unit`, `move_unit`, `create_position`, `move_position`, `assign`, `end_assignment`,
`set_deputy`, `set_absence` (także sama osoba dla swoich nieobecności, jeśli źródłem jest wpis
ręczny). Każdy zapis trafia do audytu z łańcuchem skrótów (`audit/chain.rs`) i publikuje zdarzenie
`org.*`.

**Decyzja właściciela (2026-09-30): zastępstwa czasowe (`org_deputies`) zakłada, zmienia i kończy
SAMA osoba zastępowana (tylko wiersze z `user_id` = wywołujący) oraz administrator.** Przełożony tego
prawa nie ma; zastępcy kierownika jednostki (`org_unit_deputy_heads`, stanowiska struktury) zostają
wyłącznie u administratora. Zastępca musi być aktywnym członkiem organizacji i nie może być osobą
zastępowaną; zakresy jak u administratora. Obcy wiersz daje typowane `PolicyDenied`. Zastępca dostaje
powiadomienie (magazyn powiadomień platformy, ten sam co przekazanie pracy) przy ustawieniu i
zakończeniu; to best-effort — błąd powiadomienia nie cofa zapisu.

### 2.3 Zdarzenia

`org.unit_*`, `org.position_*`, `org.person_assigned`, `org.person_unassigned`,
`org.reporting_line_changed` (z poddrzewem, którego dotyczy), `org.deputy_*`, `org.absence_*`.
Projekty reagują przeliczeniem widoczności i przydziałów zastępców.

### 2.4 Reorganizacja planowana

`org.admin` przygotowuje zestaw zmian z datą wejścia (nowy dział, przeniesienie zespołu,
zmiana kierownika). Widok „stan na 1.01” pokazuje strukturę po zmianach, a różnica jest
podświetlona. Zmiany wchodzą w życie automatycznie w dniu obowiązywania (bo są datowane), więc nie
ma nocnego „przełączania”.

**Zatwierdzenie (decyzja 2026-09-30):** reorganizacja przechodzi `szkic → czeka na zatwierdzenie →
zatwierdzona` i wymaga **jednego** zatwierdzenia przez `org.admin` innego niż jej autor (autorem jest
ten, kto ostatnio zmienił treść — edycja cofa do szkicu). **Wyjątek (decyzja 2026-09-30):** gdy w organizacji
jest dokładnie JEDEN aktywny `org.admin`, zatwierdza własną reorganizację (liczone w transakcji zatwierdzenia,
w audycie `self_approval: true`); przy dwóch i więcej znów decyduje inny administrator. Zatwierdzenie wykonuje operacje jako datowane
zapisy w JEDNEJ transakcji razem ze zmianą stanu; jeżeli struktura zmieniła się od szkicu tak, że
jakakolwiek operacja już nie przechodzi, nie zapisuje się nic (`change_set_conflict`, z wynikiem per
operacja). Wycofać można szkic, oczekującą oraz zatwierdzoną **do dnia jej wejścia w życie**: wszystkie jej wiersze
zaczynają się tego dnia, więc wycofanie usuwa dokładnie te wiersze i przywraca daty końca, które skróciła
(zapis skutków powstaje przy zatwierdzeniu), tak że struktura jest taka, jakby zatwierdzenia nie było —
chyba że coś zapisanego później się na nich opiera (`change_set_dependents`). Od dnia wejścia w życie
zmiany robi się zwykłymi edycjami (`change_set_started`). Reorganizacje widzą i prowadzą wyłącznie administratorzy;
historia zmian jest widoczna dla wszystkich, ale bez historii stanowisk innych osób (§6.3).

### 2.5 Import początkowy i masowy

Budowa struktury 200–500 osób ręcznie na płótnie jest niepraktyczna. Import CSV/XLSX:
- kolumny: kod jednostki, nazwa, kod nadrzędnej, typ, stanowisko, sztabowe (tak/nie), login/e-mail
  osoby, część etatu, przełożony (kod stanowiska), zastępca kierownika (kolejność), od kiedy,
- **tryb próbny** zawsze najpierw: raport „dodane / zmienione / błędy” (nieznany login, cykl,
  brak nadrzędnej, dwa stanowiska główne) i podgląd drzewa przed zapisem,
- zapis w **jednej transakcji** (wszystko albo nic), z wpisem w audycie ze źródłem `import`,
- eksport w tym samym formacie, żeby dało się zrobić „eksport → poprawka w Excelu → import”.

**Format pliku i tryby (zrealizowane, `services/org_structure/import/`):**
- jeden wiersz = jeden posiadacz stanowiska; wakat i jednostka bez stanowisk mają własny wiersz. Do kolumn
  z listy dochodzą trzy, bez których struktura nie wraca z pliku: **kod stanowiska** (klucz dopasowania,
  kolumna `code` w `org_positions`), **kierownik jednostki** (tak/nie) i **stanowisko główne** (tak/nie,
  puste = samo). Kolumny „osoba” i „e-mail” są informacyjne (e-mail zastępuje login, gdy ten pusty).
  Nagłówki polskie i angielskie (bez wielkości liter, znaków diakrytycznych i interpunkcji) są zdefiniowane
  w jednym miejscu (`columns.rs`); eksport pisze polskie.
- jednostka i stanowisko bez zapisanego kodu (założone w edytorze) mają w eksporcie kod pochodny od
  identyfikatora (`U-1a2b3c4d`, `P-…`), którego import rozpoznaje — eksport dowolnej struktury da się wczytać.
- **tryb `upsert` (domyślny)** robi prawdą to, co jest w pliku, i **niczego nie kończy**: co pliku nie
  wymienia, zostaje; pusta komórka znaczy „nie podano”, nie „wyczyść”. **Tryb `replace`** czyni plik całą
  strukturą: jednostki, stanowiska i posiadacze, których plik nie wymienia, kończą się w dniu importu,
  a pusta komórka czyści. `replace`, którego plik nie pasuje do żadnej istniejącej jednostki, jest odrzucany.
- dzień: `as_of` (domyślnie dziś w strefie organizacji) i „od kiedy” w wierszu. Zmiany istniejących
  encji wchodzą od `max(od kiedy, as_of)`; nowe encje powstają od najwcześniejszego dnia, jaki ich dotyczy
  (kierownik nie później niż podwładni, jednostka nie później niż jej stanowiska). Wiersz sprzed dziś
  wymaga `confirm_backdated`.
- **próba i zapis to ten sam przebieg**: próba wykonuje wszystkie operacje w transakcji, którą porzuca,
  więc raport (dodane / zmienione / bez zmian / błędy, wiersze, podgląd drzewa) jest tym, co da zapis.
  Zapis zatwierdza go tylko przy braku błędów. Błędy mają numer wiersza, kolumnę, typ i — gdzie się da —
  podpowiedź (nieznany login → najbliższe konto). Decyzje administratora (`use_suggested_login`,
  `leave_vacant`, `skip_row`) idą z żądaniem zapisu i próby.
- audyt i zdarzenia: jedno zdarzenie `org.structure_imported` z licznikami. Projekcja jest liczona
  raz, na końcu. **Źródło pola nie jest zapisywane w strukturze** (tabeli `org_field_provenance` jeszcze
  nie ma) — ślad „import” jest tylko w audycie.
- plik: CSV (UTF-8, UTF-16 z BOM lub Windows-1250 z polskiego Excela; separator `;`, `,` lub tabulator
  rozpoznawany z nagłówka) albo XLSX (pierwszy niepusty arkusz; dane na innych arkuszach dają ostrzeżenie,
  a raport niesie nazwę wczytanego arkusza). Kodowanie bez BOM: poprawne UTF-8, albo Windows-1250 wyłącznie
  gdy nie ma w nim bajtów niezdefiniowanych ani poprawnej sekwencji UTF-8 (plik mieszany = błąd
  `invalid_encoding`). Numer wiersza to numer, który pokazuje arkusz (komórka wielowierszowa to jeden wiersz).
- **limity**: plik do **900 KiB** i **5000 wierszy** (XLSX: arkusz najwyżej 5100 × 64 komórek, liczone
  z XML-a przed alokacją; po rozpakowaniu najwyżej 32 MiB, liczone z faktycznie rozpakowanych bajtów).
  Plik idzie w jednej ramce, a gniazdo WebSocket panelu zamyka połączenie (1009) przy ramce ponad 1 MiB
  (`MAX_FRAME_SIZE` w `ws_binary.rs`; serwer nie ma dziś punktu `/wt/api`, więc przeglądarka zawsze wraca
  na WebSocket, a `transport.js` nie narzuca własnego limitu ramki — jedyny limit to ten),
  więc 900 KiB zostawia zapas na kopertę i decyzje (do 2000). **Serwer nie odpowie na ramkę, którą gniazdo
  odrzuciło — ekran sprawdza rozmiar przed wysłaniem** (`ORG_IMPORT_MAX_FILE_BYTES` w `codec.js`; serwer
  podaje limity w `StructureResponse` i w raporcie). Większy plik dostaje `file_too_large` tam, gdzie
  transport go przepuści (iroh).
- **replace wymaga potwierdzenia**: raport wylicza w `ended`, co się kończy (jednostki, stanowiska z ich
  posiadaczami, pojedynczych posiadaczy), a `assignmentsEnded` liczy też przypisania kończone razem ze
  stanowiskiem. Zapis (i próba) bez `confirm_ended: true` daje błąd `ended_confirmation_required`.
  Encje, które zaczęły się w dniu importu lub później, są wycofywane (usuwane), nie „kończone”:
  poprawka błędnego importu tego samego dnia działa.
- audyt: **jeden wpis** `org.structure.import` (źródło `import`, liczniki i lista operacji jako
  `[akcja, cel]`), bo wpis na operację trzymałby połączenie zapisu przez tysiące wstawień do łańcucha
  skrótów. Próba wykonuje także końcowy etap (przechwycenia, projekcja, audyt) w transakcji, którą porzuca.
- eksport mogą pobrać wszyscy członkowie (strukturę i tak widzą); kolumny login i e-mail są tylko
  w pliku administratora.

### 2.6 Odejście osoby i zmiana stanowiska

Zakończenie przypisania (`valid_to`) nie usuwa niczego, ale:
- **otwarte sprawy osoby** (kroki workflow, zatwierdzenia, eskalacje, alokacje po dacie odejścia)
  trafiają na listę „Do przekazania” jej przełożonego i PM-ów projektów, z propozycją odbiorcy
  (zastępca kierownika / zastępstwo / główna osoba modułu),
- jej stanowisko staje się wakatem (widocznym na drzewie),
- **ekran „Do przekazania”** (mockup F07) pokazuje te sprawy pogrupowane z podpowiedzią odbiorcy
  i pozwala przekazać wszystko albo zaznaczone jednym ruchem, z wymaganą notatką dla przejmujących.
  Ten sam ekran obsługuje nieobecność (przekazanie czasowe, praca wraca po powrocie) i usunięcie
  z projektu (tylko elementy tego projektu) — `PROJECT_STUDIO_WORKFLOW_PLAN.md` §4.5,
- dezaktywacja konta w platformie bez zakończenia przypisania daje ostrzeżenie w widoku
  struktury (niespójność do wyjaśnienia).

**Realizacja (S3, `services/org_structure/handover.rs` + `handover/`, ekran
`www/js/modules/org-structure/handover-*.js`):**
- **Kategorie tylko z realnymi danymi:** zadania (`tasks.assigned_to`, otwarte), otwarte pozycje
  trwających uruchomień testów (`test_run_items`), członkostwa w projektach (`project_members`),
  stanowiska (przypisania w strukturze) i zastępstwa (`org_deputies`, osoba jako zastępca albo
  zastępowana). Kroków workflow, alokacji, obowiązków, wyjątków i spraw PSIRT jeszcze nie ma
  w danych, więc ekranu nie zapełniamy makietą.
- **Uprawnienia:** odejście — `org.admin` (przekazuje pracę we wszystkich projektach organizacji,
  ale tylko elementy TEJ osoby i tylko członkom tego samego projektu; każdy krok trafia do dziennika
  aktywności projektu); usunięcie z projektu — rola managera W TYM projekcie (administrator bez roli
  nie ma prawa edycji treści projektu, zgodnie z regułą Project Studio); nieobecność — sama osoba,
  jej przełożony w linii głównej albo administrator. Odbiorca musi być członkiem projektu
  (testerem albo lepiej przy pozycji testowej), właściciel projektu nie odchodzi bez wskazania
  następcy, managera usuwa tylko właściciel.
- **Atomowość (uczciwie):** struktura i Project Studio to różne bazy, więc jedna transakcja jest
  niemożliwa. Zapis przekazania (`org_handovers`/`org_handover_items`, migracja 182, lokalna dla
  węzła, bo opisuje niereplikowane bazy) powstaje PRZED pierwszym ruchem; wszystkie pozycje
  struktury idą w JEDNEJ transakcji organizacji razem ze stanem w zapisie (jedna odrzucona = żadna
  i nic dalej się nie zaczyna); elementy Project Studio idą po jednym, każdy jednym warunkowym
  UPDATE-em (`WHERE assigned_to = <osoba> AND niezamknięte`), a porażka jednego jest zapisana i nie
  zatrzymuje reszty. Odpowiedź podaje stan każdej pozycji, `HandoverRetry` kończy nieudane z zapisu,
  a powtórzenie kroku po awarii znajduje pracę już przeniesioną i uznaje ją za wykonaną.
- **Nieobecność jest czasowa:** pozycje wracają do osoby w dniu powrotu (pętla `due::run_due`,
  co noc na każdym węźle jak przeliczenie profili), chyba że przejmujący zamknął zadanie
  (`kept: closed`), ktoś zmienił wykonawcę (`kept: changed`) albo osoba nie jest już w projekcie.
  Członkostwo kończące się w dniu odejścia jest `scheduled` i kończy się w tym dniu (o ile osoba
  nie ma już w projekcie otwartej pracy).
- **Ślad:** wpis w łańcuchu audytu `org.handover.apply` niesie liczniki, skrót SHA-256 i długość
  notatki (sama notatka jest w zapisie przekazania i jako komentarz przy zadaniu); zmiany struktury
  audytuje sesja zapisu struktury; przejmujący dostaje powiadomienie z notatką.

## 3. Wizualizacja — „ładnie graficznie, żeby pokazywać”

### 3.1 Tryb prezentacji (domyślny, dla wszystkich z odczytem)

- **Drzewo czyste:** układ tidy tree (algorytm Walkera / Reingold–Tilford z węzłami różnej wielkości),
  pionowy (góra → dół) albo poziomy. Linie ortogonalne z zaokrągleniami. Linie funkcjonalne
  (macierz) kropkowane i domyślnie ukryte (przełącznik), żeby nie zaciemniać obrazu.
- **Karta osoby:** **inicjały** (v1); później **zdjęcie dodane przez samego użytkownika**
  zastępuje inicjały (kto nie doda, zostaje z inicjałami), imię i nazwisko, stanowisko, pasek w kolorze jednostki,
  znaczniki: „p.o.”, „zastępstwo”, „nieobecny” (bez powodu), wakat (przerywana ramka). Liczba
  podwładnych jako **„+N”** przy zwiniętym węźle.
- **Widok jednostek:** jednostki jako duże „ramki” z kierownikiem na górze i zespołem w siatce
  (tak wygląda to w prezentacjach zarządu). Przełączanie „osoby / jednostki”.
- **Nawigacja:** zoom i pan (kółko, gest, klawiatura), minimapa, „dopasuj”, wyszukiwarka z
  **podświetleniem ścieżki od korzenia** do znalezionej osoby, przycisk **„moja pozycja”**. Rozwijanie
  domyślnie do 3 poziomów, a dalej na żądanie.
- **Poziom szczegółów:** przy małym zoomie karty zamieniają się w kafelki z nazwą jednostki i liczbą
  osób, więc tysiąc osób nie zamienia się w szum.
- **Tryb prezentacji pełnoekranowej:** bez paneli, większa typografia, przejścia animowane między
  poziomami (z poszanowaniem `prefers-reduced-motion`).
- **Eksport:** SVG (wektor, do dalszej obróbki), PNG w wysokiej rozdzielczości, PDF A4/A3 z podziałem
  na strony po jednostkach oraz „stan na dzień” w stopce. Wybrane gałęzie albo całość.
- **Statystyki w nagłówku jednostki:** liczba osób, wakaty, średnia rozpiętość kierowania.
- **Motyw:** tokeny z `design/tokens/tokens.json`, pełny motyw jasny i ciemny, ikony SVG, bez emoji.

**Zdjęcia profilowe (faza S5):** użytkownik sam dodaje, zmienia i usuwa swoje zdjęcie w profilu.
Serwer tworzy miniatury (np. 64/128/256 px), usuwa metadane (EXIF z lokalizacją) i przycina do
kwadratu z podglądem kadru. Administrator może zdjęcie usunąć (np. nieodpowiednie), ale go nie
ustawia. Przy dodawaniu użytkownik widzi informację, że zdjęcie zobaczą wszyscy z dostępem do
struktury. To samo zdjęcie trafia wszędzie, gdzie platforma pokazuje awatar.

### 3.2 Tryb edycji (tylko `org.admin`)

Na tym samym płótnie:
- ustawianie kierownika i **zastępców kierownika** jednostki oraz oznaczanie stanowisk sztabowych,
- wczytanie szablonu startowego struktury,
- przeciąganie karty na nowego przełożonego (z walidacją cyklu i potwierdzeniem „zmienisz
  przełożonego dla N osób”),
- przeciąganie osoby z listy na wakat,
- dodawanie stanowisk i jednostek z panelu,
- inspektor po prawej (dane stanowiska, historia, źródło każdego pola),
- cofnij/ponów,
- **data obowiązywania zmiany** (dziś albo przyszła — reorganizacja planowana).

Paleta ról z mockupu O1 zostaje, ale bez portów na kartach: relację ustawia się przeciągnięciem
albo polem „raportuje do”.

### 3.3 Technika

- **Renderowanie SVG** — ostre w każdej skali, eksport za darmo, zdarzenia DOM bez problemów
  z nakładaniem płótna na interfejs (z doświadczeń NextApp: płótno Konvy przepuszczało kliknięcia
  do nasłuchów DOM okna). Dla dużych drzew: renderowanie tylko widocznych i rozwiniętych gałęzi.
- **Układ:** komponent `tf-org-tree` z planu O1. Algorytm tidy tree z węzłami o różnej wielkości to
  niewielki, ale łatwy do zepsucia kod. Do wyboru: własna implementacja z testami na przypadkach
  brzegowych (głębokie i szerokie drzewa, węzły różnej szerokości) albo dołączenie `d3-hierarchy`
  / `d3-flextree` (małe, na licencji ISC/WTFPL) wyłącznie do liczenia układu. **Decyzja przy
  implementacji, po pomiarze na syntetycznym drzewie 500 i 2000 osób (4 poziomy rozwinięte):
  czas pierwszego rysowania (cel < 300 ms), płynność przesuwania i zoomu (cel 60 kl./s na
  typowym laptopie), liczba elementów SVG, czas eksportu PDF.** Jeśli SVG nie spełni celów przy
  2000 osób, rysowanie przechodzi na canvas z eksportem SVG generowanym osobno. Dziś w `www/js/vendor` nie ma żadnej
  biblioteki grafów.
- **Eksport PDF:** po stronie przeglądarki z wygenerowanego SVG (druk do PDF z podziałem na
  strony wg jednostek). Serwer nie renderuje.
- **Dostępność:** nawigacja klawiaturą po węzłach (strzałki = rodzic/dziecko/rodzeństwo, Enter =
  rozwiń), odpowiedniki ARIA drzewa, a zakładka **Lista** jest pełnoprawną alternatywą dla
  czytników ekranu.
- Istniejące komponenty do reużycia: `tf-tree` (widok listy), wzorce zoom/pan z `mesh-diagram.js`
  i `flows-builder/canvas.js`.

### 3.4 Zakładki (odblokowanie `roles_catalog.js:270-279`)

**Drzewo** (3.1/3.2) · **Lista** (tabela: osoba, stanowisko, jednostka, przełożony, od kiedy, źródło;
eksport CSV) · **Widoczność** (inspektor „kto co widzi” — dla wybranej osoby: czyje dane widzi
i dlaczego, np. „poddrzewo: dział Realizacja”) · **Historia** (zmiany z diffem, widok „stan na
dzień”) · **Katalog ról** (bez zmian).

## 4. Użycie w Projektach

- **Widoczność** (`PROJECT_STUDIO_WORKFLOW_PLAN.md` §14.3):
  - **PM** — pełne dane swoich projektów,
  - **kierownik** — utylizacja, nieobecności (bez powodu) i ewidencja czasu swojego **poddrzewa**
    ze struktury,
  - **zarząd** — wszystko przez uprawnienie `project_studio.resources.view_all`,
  - każda osoba — swoje.
- **Eskalacje SLA i obowiązków:** wykonawca → zastępca modułu → PM → **łańcuch przełożonych ze
  struktury** (`org.escalation_chain`), z pominięciem nieobecnych.
- **Przydziały przy nieobecności:** krok workflow przypisany do osoby nieobecnej przechodzi na jej
  zastępcę (zakres `all` albo `project:<id>`). Zmiana trafia do historii zadania.
- **Wymiar pracy do utylizacji** = suma `share` przypisań ze struktury × kalendarz roboczy, minus
  nieobecności.
- **Funkcje w projekcie** (Developer, Tester…) pozostają osobne od stanowiska. Stanowisko mówi, kim
  ktoś jest w firmie, a funkcja — co robi w danym projekcie. Stanowisko może jednak **podpowiadać**
  funkcję przy dodawaniu do projektu.

## 5. Źródła danych i synchronizacja

| Źródło | Co daje | Tryb | Stan |
|---|---|---|---|
| **Ręcznie** (administratorzy) | jednostki, stanowiska, przypisania, linie, zastępcy kierowników, zastępstwa | zawsze | **v1 — jedyne źródło struktury** |
| **eDokumenty** (addon) | **wyłącznie urlopy / nieobecności** | cykliczny odczyt, tylko do odczytu po stronie eDokumentów | S6 — dodatek na końcu; do tego czasu nieobecności wpisuje się ręcznie |
| AD / Entra ID | `manager`, `department`, `title` | synchronizacja katalogu — dziś w TentaFlow nie istnieje | opcja na przyszłość, poza planem |

Dopóki strukturę prowadzi się wyłącznie ręcznie, pierwszeństwo źródeł dotyczy tylko nieobecności
(wpis ręczny kontra eDokumenty: **eDokumenty wygrywają**, a wpis ręczny nakładający się na urlop
z eDokumentów trafia do konfliktów). Mechanizm pierwszeństwa per pole zostaje w modelu na wypadek
przyszłego AD/Entra. Zasady:
- wartość ze źródła o wyższym pierwszeństwie nadpisuje niższe,
- **niejednoznaczność albo sprzeczność nie jest rozstrzygana po cichu**, tylko trafia na listę
  „Do decyzji” — tak jak w synchronizacji przełożonych w NextApp,
- zniknięcie osoby ze źródła **nie usuwa** przypisania, tylko je oznacza (to może być błąd
  synchronizacji, a nie zwolnienie),
- każde pole pokazuje w inspektorze swoje źródło i czas synchronizacji.

### 5.1 eDokumenty — addon integracyjny

- **Zakres (decyzja):** addon czyta **tylko urlopy i nieobecności**. Strukturę prowadzą
  administratorzy w TentaFlow.
- **Dopasowanie osób:** konto eDokumentów ↔ konto TentaFlow przez login domenowy albo e-mail
  (obie strony z tego samego AD). Niedopasowane konta trafiają na listę do ręcznego powiązania
  przez administratora, a urlop nie przypisuje się „na oko”.
- **API:** SOAP (`/eDokumentyApi.php?wsdl`), uwierzytelnienie WS-Security z hasłem kodowanym MD5
  albo parametrami `a1/a2/a3` w adresie. W dokumentacji publicznej są metody użytkowników, grup
  i jednostek organizacyjnych (`getUserAccount`, `getGroup`, `getOrganizationUnit`,
  `assignUserToOrganizationUnit`…), ale **nie ma metod dla urlopów i nieobecności**. Wnioski
  urlopowe w eDokumentach są procesami/dokumentami, więc prawdopodobnie da się je odczytać przez
  `searchProcess` / `searchDocument` / rejestry z filtrem typu. **[niezweryfikowane — trzeba
  sprawdzić na Waszej instancji albo u dostawcy]**. Alternatywy: raport/widok eksportowany
  cyklicznie albo webhook z procedury obiegu (etap „zatwierdzony” wysyła zdarzenie).
- **Bezpieczeństwo:**
  - dedykowane konto techniczne tylko do odczytu (od wersji 4.0 API przyjmuje dowolne konto, więc
    uprawnienia trzeba ograniczyć w samych eDokumentach),
  - **nigdy parametry `a1/a2` w adresie** — hasło trafiłoby do logów serwerów i proxy; tylko
    nagłówek WS-Security,
  - wyłącznie HTTPS (MD5 hasła w nagłówku to nie ochrona),
  - sekret w sejfie platformy,
  - import nieobecności **bez powodu** (sam przedział i rodzaj ogólny), jeśli eDokumenty go
    udostępniają — minimalizacja danych (RODO).
- **Forma:** addon z uprawnieniem do zapisu **wyłącznie** w `org_absences` z `source = edokumenty`,
  przez nową host function `org.import_absences`. **[do decyzji]** Czy addon WASM ma dostęp do wychodzącego
  HTTP/SOAP, czy potrzebny jest konektor natywny — zależy od możliwości sandboksa addonów.

## 6. Uprawnienia i prywatność

### 6.1 Odczyt

Samą strukturę (kto jest kim i komu podlega) widzi **każdy, kto ma dostęp do programu**, także
kontraktorzy i osoby zewnętrzne z kontem (decyzja 2026-09-29). Nie widzą jej osoby bez konta.

### 6.2 Zapis

Wyłącznie **administratorzy**. Sama osoba może wpisać swoją nieobecność ręcznie (np. szkolenie
spoza eDokumentów), ustawić własne zastępstwo czasowe (decyzja 2026-09-30, §2.2) i dodać swoje
zdjęcie. Urlopy przychodzą z eDokumentów.

### 6.3 Dane osobowe w strukturze

| Dane | Kto widzi |
|---|---|
| daty nieobecności | osoba + przełożeni w **linii głównej** (nie funkcjonalnej) + administratorzy; pozostali — tylko „niedostępny” dziś, nigdy daty ani stan na inny dzień. Powodu nieobecności nie przechowujemy wcale (migracja 200 usuwa dotychczasowe) |
| ewidencja czasu i utylizacja | osoba + przełożeni (poddrzewo) + PM projektu (tylko w zakresie projektu) + zarząd |
| historia stanowisk | administratorzy + osoba |

Pozostali widzą tylko „niedostępny” i agregaty.

**Historia a prawo do usunięcia danych (RODO):** historia struktury jest potrzebna do audytu
(kto kiedy miał jakie uprawnienia) przez okres przechowywania ustalony przez organizację (np. czas
wymagany dla dokumentacji kadrowej/audytu). Po jego upływie albo po skutecznym żądaniu usunięcia:
- osoba w historii zostaje **spseudonimizowana** („Osoba #a1b2”),
- przedziały dat i stanowiska zostają,
- zdjęcie i dane kontaktowe są usuwane od razu po odejściu.

Okres przechowywania i podstawę prawną ustala organizacja z prawnikiem **[do decyzji]**.

### 6.4 Mechanizm anonimizacji i retencji (usługa platformy, wspólna dla wszystkich aplikacji)

Decyzje prawne (okresy, podstawy) zapadną później, ale **mechanizm musi istnieć od początku**, żeby
decyzja była zmianą ustawienia, a nie przebudową. Usługa `privacy/` w core:

| Element | Co robi |
|---|---|
| **Rejestr kategorii danych osobowych** | każda aplikacja deklaruje w manifeście, gdzie trzyma dane osób (tabela, kolumna, rodzaj: identyfikator / nazwisko / e-mail / zdjęcie / treść swobodna / nieobecność) i jaką operację wspiera (usuń / pseudonimizuj / zostaw z podstawą prawną). Bez deklaracji aplikacja nie przechodzi przeglądu |
| **Polityki retencji** | per kategoria: okres (np. „historia struktury: X lat od odejścia”, „ewidencja czasu: Y lat”, „dowody CRA: 10 lat”), zdarzenie startowe (odejście, zamknięcie projektu, wydanie), akcja po upływie. Wartości domyślne puste = „nie usuwaj automatycznie” do czasu decyzji prawnej |
| **Pseudonimizacja** | osoba dostaje stały pseudonim (`Osoba #a1b2`) liczony kluczem organizacji; referencje (identyfikatory kont w historii, zadaniach, dowodach) pozostają spójne, więc raporty i łańcuchy audytu dalej działają, a nazwisko, e-mail, zdjęcie i dane kontaktowe znikają. Klucz mapowania **można zniszczyć** (wtedy pseudonimizacja staje się anonimizacją nieodwracalną) — decyzja per organizacja |
| **Treść swobodna** | komentarze, opisy, notatki: wzmianki `@osoba` są zamieniane na pseudonim; pełnotekstowa treść pisana przez osobę zostaje (to praca zespołu), chyba że polityka każe inaczej. Wykrywanie nazwiska w tekście wolnym wspiera LLM (propozycja listy miejsc do przejrzenia, decyzja człowieka) |
| **Blokada prawna (legal hold)** | wstrzymuje retencję dla wskazanych danych (spór, kontrola, sprawa PSIRT) do zdjęcia blokady |
| **Żądanie osoby (RODO art. 15/17)** | rejestr żądań: kto, kiedy, zakres; **raport „co o mnie jest”** zbierany z rejestru kategorii ze wszystkich aplikacji; **przegląd** (co zostanie usunięte, co spseudonimizowane, co zostaje z podstawą prawną i dlaczego); wykonanie jako zadanie w tle z postępem; potwierdzenie z listą wykonanych operacji. Terminy odpowiedzi pilnuje silnik obowiązków |
| **Dziennik** | każda operacja anonimizacji w audycie (bez danych, które usunięto) — dowód wykonania |
| **Kopie zapasowe** | kopie wygasają zgodnie z cyklem rotacji; przy odtworzeniu kopii rejestr żądań jest odtwarzany ponownie (osoby usunięte po dacie kopii są anonimizowane jeszcze raz) |

Aplikacje korzystają z jednego API (`privacy.register_category`, `privacy.pseudonym(user)`,
`privacy.apply(request)` wywołujące handler aplikacji per kategoria). Ekran administratora:
**Prywatność → Żądania / Polityki retencji / Blokady / Rejestr kategorii**. Wszystko liczy jedna funkcja
`org.can_view_person_data`, żeby Projekty, CRM i raporty nie miały własnych kopii reguły.

## 7. Fazy

| Faza | Zakres |
|---|---|
| **S0** | tabele z datami obowiązywania (semantyka §1), walidacje, projekcja do `sync_user_org_profiles` ze stanowiskiem głównym, host functions odczytu (z `org_id`), audyt, **import/eksport CSV/XLSX z trybem próbnym** |
| **S1** | zakładki Drzewo (tryb prezentacji) i Lista, wyszukiwanie ze ścieżką, „moja pozycja”, eksport SVG/PNG/PDF |
| **S2** | tryb edycji (przeciąganie, wakaty, kierownik i zastępcy kierownika, stanowiska sztabowe, szablony startowe struktury, inspektor ze źródłami), Historia i „stan na dzień”, reorganizacja planowana |
| **S3** | zastępstwa, nieobecności (ręcznie), `org.escalation_chain` (§2.1a), `org.can_view_person_data`, odejście osoby i lista „Do przekazania” (§2.6), podpięcie Projektów (widoczność, eskalacje, przydziały) |
| **S4** | zatwierdzenia wg progów roli (otwarta decyzja 4 z O1); synchronizacja AD/Entra tylko jeśli okaże się potrzebna |
| **S3b** | usługa `privacy/`: rejestr kategorii, pseudonimizacja, polityki retencji (puste do decyzji prawnej), żądania osób, blokady prawne (§6.4) — przed pierwszym wdrożeniem produkcyjnym |
| **S5** | zdjęcia profilowe dodawane przez użytkowników (miniatury, usuwanie EXIF, usuwanie przez administratora) |
| **S6 (na końcu, dodatek)** | addon eDokumenty — tylko urlopy (po weryfikacji API), dopasowanie kont, konflikty z wpisami ręcznymi. Aplikacja działa w pełni bez niego: nieobecności wpisuje się ręcznie |

**Kolejność łączona z Projektami:** `PROJECT_STUDIO_WORKFLOW_PLAN.md` §17.1 — S0 idzie zaraz po P0,
bo odblokowuje wymiar pracy i widoczność; S3 przed P4 (eskalacje SLA ponad PM, zastępstwa).

## 8. Decyzje i otwarte pytania

Rozstrzygnięte 2026-09-29:
- strukturę prowadzą **administratorzy**,
- eDokumenty są źródłem **wyłącznie urlopów** i są **dodatkiem na koniec** (S6),
- na kartach **inicjały**, a później zdjęcia dodawane przez samych użytkowników,
- kierownik jednostki z **opcjonalnymi zastępcami** — model uniwersalny (§1.1),
- strukturę widzi **każdy z dostępem do programu**,
- zastępca kierownika dostaje eskalacje i zatwierdzenia **wyłącznie pod nieobecność kierownika**.
  Kierownik obecny = zastępca nie jest powiadamiany.

Otwarte:
1. Okres przechowywania historii struktury i podstawa prawna (z prawnikiem) — §6.3.

Odłożone (nie blokuje niczego):
- weryfikacja API eDokumentów dla urlopów — dopiero przy fazie S6; addon jest dodatkiem, a cała
  aplikacja działa bez niego.
