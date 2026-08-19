# Stan pętli — śledzenie zdarzeń + RAG

Rejestr rund. Jeden wiersz na rundę toru. Werdykt bez dowodu = brak werdyktu.
Fail-closed: kryterium niezweryfikowane = NIESPEŁNIONE.

## Punkt odniesienia

- `HEAD` startowy: `d9d98c1fd`
- Baseline `cargo check --lib` (tentaflow-core): **czysty**. Liczba ostrzeżeń zależy od formatu
  wyjścia (86 przy `--message-format=short`, 30 przy domyślnym) — **liczba bezwzględna jest
  bezużyteczna jako kryterium**. Obowiązuje DELTA mierzona metodą stash-and-compare: odłóż
  własne pliki, zmierz, przywróć, zmierz ponownie. Tak zrobił M129 i uzyskał deltę 0.
- Najwyższa migracja bazy głównej: **128** `agents_delegation_roster`.
- `MessageBody`: 311 wariantów, kodowanie **po nazwie** (golden testy), nie po indeksie.

## Bramki człowieka

| Bramka | Stan | Data |
|---|---|---|
| Pierwszy filtr / retencja / widoczność | rozstrzygnięte: origin / 30 dni / user-swoje+admin-wszystko | 2026-08-19 |
| §1.2 G2 (R4) | **poza pętlą**, pytanie wraca po G1 | 2026-08-19 |
| Migracja bazy głównej nr 129 (correlation_id + uprawnienia + retencja) | **ZATWIERDZONA** — z zastrzeżeniem poniżej | 2026-08-19 |
| Migracja danych addona RAG (`ingest_jobs` → `jobs.db`, zmienia bundle hash) | NIEZAPYTANA — przed R1 faza 2 | — |
| Push / kasowanie danych | NIEZAPYTANE | — |
| Migracja 131 (`flow_executions` + `agent_runs` — kolumny pochodzenia) | **ZATWIERDZONA** (`agent_runs` dołożone przeze mnie, zgłoszone jawnie) | 2026-08-19 |
| Pochodzenie `Scheduler` przeciągnięte przez harmonogram | **ZATWIERDZONE** | 2026-08-19 |
| Polityka językowa (angielski wszędzie, tłumaczenie przy edycji) | **ROZSTRZYGNIĘTA** | 2026-08-19 |
| Migracja naprawcza 130 (`addon_graph_collections.file_path`) | **ZATWIERDZONA** | 2026-08-19 |
| `SCHEMA_VERSION` 22 → 23 | **ZATWIERDZONE** | 2026-08-19 |

## Rundy

| Runda | Tor | Wykonawca | Krytyk | Werdykt | Notatka |
|---|---|---|---|---|---|
| 1 | T1 `origin`+`actor` | zdany | ślepy krytyk #4 | **DOES NOT MEET BAR** — kryt. 3,4,6,7,11 | pułapka rustfmt: PASS (krytyk odtworzył w worktree, 18 linii wcięć, nic nie zgubione) |
| 2 | T1 poprawka | w toku (nowy wykonawca) | — | — | pusty test forgery, furtki domyślne, degradacja klucza API, `correlation_id` od klienta |
| 1 | T5a `tf-run-timeline` | zdany | ślepy krytyk #1 | **MEETS BAR** 14/14 | dowody odtworzone niezależnie; odrzucenie `UnifiedTimeline` potwierdzone (5/470 linii wspólnych) |
| 1 | R2a `graph_home` w `GraphManager` | zdany | ślepy krytyk #2 | **DOES NOT MEET BAR** — kryt. 7, 8 | 1–6 i 9 zdane; krytyk potwierdził testy MUTACJAMI (3 mutacje, każda złapana) |
| 2 | R2a poprawka | w toku (nowy wykonawca) | — | — | sprzeczny komentarz w `tests.rs` + angielski w nowych liniach |
| 1 | M129 migracja | zdany | ślepy krytyk #3 | **DOES NOT MEET BAR** — kryt. 12 | 1–11 zdane z własnymi dowodami krytyka; polska nazwa testu i komentarz w nowym kodzie |
| 2 | M129 poprawka | w toku (nowy wykonawca) | — | — | angielski + `'events'` w `INITIAL_SCHEMA` (precedens v65) |

## Kolejka (zablokowane zależnościami)

| Tor | Blokada |
|---|---|
| T2 `events.db` | **ODBLOKOWANE, w toku** |
| T3 `FirstToken` | zdany, ślepy krytyk #5 w toku |
| T4 metryki | czeka na T2 |
| T5b rejestr + inspektor + moduł | czeka na T5a i na protokół z T2 |
| T6 Code Studio + odsyłacz | czeka na T5, T2, M129 |
| R1 `jobs.db` | odblokowane (T1 skończył w `project_studio/ingest.rs`) |
| R2b węzeł `graph_extract` | czeka na R2a i na T1 (`graph_home` w meta) |
| R3 jedna powłoka | odblokowane (T1 skończył w `stream_handlers.rs`) |

## Konflikty plikowe do pilnowania

- `dispatch/stream_handlers.rs` — T1 i R3
- `project_studio/ingest.rs` — T1 i R1
- `flow_engine/dispatcher.rs` / `node_adapter.rs` — T1, potem R2b
- `db/migrations.rs` — M129 (wyłączność)

## Znaleziska poboczne (do raportu końcowego, nie naprawiane z własnej inicjatywy)

1. **Kopia audytowa Code Studio nigdy nie jest drenowana.**
   `code_studio/audit_outbox.rs:108 spawn_delivery_loop` nie ma ŻADNEGO wołającego; tak samo
   `code_studio/workspace_db.rs:149 spawn_idle_sweeper` i `:183 checkpoint_all`. Wpięte są tylko
   odpowiedniki Project Studio (`project_studio/mod.rs:51`, `main.rs:1117`). Skutek: zdarzenia
   security-relevant Code Studio zapisują wiersz do `audit_outbox`, ale nic go nie przenosi do
   `audit_log`. Wzorzec, który spec §2.8 każe kopiować, jest w tej instalacji martwy.
   **Nie naprawiam z własnej inicjatywy** — to poza zleconym zakresem; `events.db` wpina swoje
   pętle jawnie, żeby nie powielić tej luki.
2. **Skill `/browse` nie jest zainstalowany** w tym środowisku, mimo że `CLAUDE.md` go nakazuje
   do pracy z przeglądarką. Weryfikacja UI idzie przez lokalnie obecne Playwright.

## Dług z T5a — do domknięcia w T5b, nie blokuje poprzeczki

Znalezione przez ślepego krytyka, poza listą kryteriów, ale realne:

1. `_moveTip(e, id)` wołane z dwoma argumentami, zadeklarowane jako `_moveTip(e)`. W JS przechodzi
   po cichu — dokładnie ten rodzaj rozjazdu, który reguły jakości mają wyłapywać.
2. `_palette()` i `_hatchPattern()` cache'ują bezterminowo. Pulpit jest dziś tylko ciemny, więc
   skutku nie widać, ale „na zawsze" to zły domyślny czas życia cache'u.
3. **Rekord w locie, który zaczął się PRZED oknem widoku, znika** — `_visible()` używa
   `endOf(r) >= t0`, a `endOf` rekordu w locie to jego start. Zgodne z inwariantem 6 (nie
   fabrykujemy końca), ale znaczy, że długo trwające wywołanie wypada z kadru dokładnie wtedy,
   gdy się w nie wpatrujesz. Do rozstrzygnięcia: znacznik przy lewej krawędzi z oznaczeniem
   „zaczęło się wcześniej" jest uczciwy i nie fabrykuje czasu.

## Świadomie poza zakresem (spec §2.10 to wymienia)

Obsługa klawiatury, widok mobilny i zachowanie przy tysiącach rekordów — prototyp tego NIE
dowodzi, a spec mówi wprost, że wirtualizacja i agregacja pasm są **do zmierzenia na realnych
danych, nie do zaprojektowania z góry**. Krytyk potwierdził brak ARIA i ścieżki klawiaturowej na
torze. Trafia do sekcji „czego nie zrobiłem".

## Sprostowanie do bramki M129

Prosząc o zgodę napisałem „zero zmian w istniejących wierszach, zero DROP-ów". **To było
nieścisłe.** Poszerzenie ograniczenia `CHECK` w SQLite wymaga przebudowy tabeli:
`CREATE …_new` → `INSERT…SELECT` → `DROP TABLE` → `RENAME`. Migracja robi to na
`compliance_retention_policies`. Precedens istnieje w tym repo — migracja v65 zrobiła to samo,
dodając `agent_runs`. Dane są przenoszone w całości; krytyk ma osobno zweryfikować, że przebudowa
wiernie odtwarza WSZYSTKIE kolumny, indeksy, wyzwalacze, klucze obce i wartości domyślne — bo
przebudowa gubiąca indeks albo trigger to realna utrata zachowania, nie kosmetyka.

## Zadeklarowane luki T1 (wykonawca zgłosił sam — do rozstrzygnięcia, nie do przemilczenia)

1. **Trzy handlery strumieniowe bez testu.** `flow_invoke`, `project_studio_chat`,
   `project_studio_code_assist` wymagają żywego `FlowDispatcher`, subskrypcji i sesji, a w repo nie
   ma dla nich uprzęży testowej. Wykonawca **nie sfabrykował testu** — dobrze. Ich poprawność stoi
   na wymogu kompilacji plus diffie. Krytyk ma ocenić, czy to wystarcza pod kryterium T1.3.
2. **`origin` sub-agenta degraduje do `Agent`.** `agent_runs` trzyma `user_id`/`org_id`, ale nie
   punkt wejścia, więc dziecko dziedziczy AKTORA wiernie, a pochodzenie traci. Odzyskanie
   wymagałoby kolumny w `agent_runs` — czyli migracji poza zakresem tego toru.
   **Do decyzji: czy to jest akceptowalne, czy R4-owa robota na później.**
3. **Pięć ostrzeżeń martwego kodu w dotkniętych plikach jest sprzed zmiany** — wykonawca ich nie
   skasował, uznając za poza zakresem. Reguła 5 `CLAUDE.md` mówi kasować martwy kod na bieżąco;
   krytyk ma rozstrzygnąć, czy „dotknąłem pliku" to już powód.

## Ryzyko operacyjne zgłoszone przez krytyka M129 — DO DECYZJI CZŁOWIEKA

`compliance_retention_policies` **oraz** `roles` są w `sync/core_registry.rs`, czyli replikują się
przez Sync Ledger. Węzeł z migracją 129 wyśle wiersz polityki o zakresie `events`, którego węzeł
na 128 **odrzuci na swoim ograniczeniu CHECK**, a jego `retention_policy_from_row` wywróci się na
nieznanym zakresie. Zmianie nie towarzyszy podbicie `SCHEMA_VERSION` (dziś 22).

Ekspozycja jest identyczna jak przy migracji v65 (`agent_runs`), a `CLAUDE.md` już dziś mówi
„rebuild all mesh nodes together". Mimo to: **podbicie `SCHEMA_VERSION` 22→23 zerwałoby handshake
ze starymi binarkami świadomie i głośno, zamiast pozwolić im cicho odrzucać wiersze.**
To jest decyzja operacyjna, nie techniczna — nie podejmuję jej sam.

## Obserwacje krytyka M129 nie będące usterkami (odnotowane)

- Nic jeszcze nie ZAPISUJE `audit_log.correlation_id` i nic nie sprawdza `events.read*`.
  Zgodne z zakresem etapu, ale odsyłacz audyt→oś czasu **nie działa z samej tej zmiany**.
- Cofnięcie skasowania organizacji zostawia ją BEZ domyślnych polityk, więc
  `resolve_retention_policy` zwróci błąd. Luka zastana, identyczna dla `agent_runs`, nie wniesiona
  tą zmianą.

## Zatrzymanie pętli — to samo kryterium dwa razy z rzędu

Zadziałała reguła: kryterium „angielski w nowym kodzie" padło u DWÓCH niezależnych wykonawców
(M129 i R2a). Diagnoza przewidziana przez regułę okazała się trafna — problem był w otoczeniu, nie
w wykonawcach: pliki, które edytowali, są po polsku (`services/graph/collection.rs` — setki linii,
wszystkie sąsiednie testy w `compliance/repository.rs`), więc dopasowanie się do stylu było
naturalnym odruchem.

**Rozstrzygnięcie człowieka: angielski wszędzie, tłumaczenie zastanych treści przy okazji edycji
pliku.** Wykonanie rozbite na dwa kroki, żeby tłumaczenie nie zagrzebało zmian funkcjonalnych:
1. nowy kod po angielsku — egzekwowane od razu, w każdym zleceniu,
2. tłumaczenie zastanych komentarzy — osobne przejście na końcu, po wszystkich torach.

Reguła wysłana do agentów w locie (T2, T3, poprawka M129).

## Jakość krytyki — warte odnotowania

Krytyk R2a nie poprzestał na przeczytaniu testów: **wprowadził trzy mutacje do kodu** (usunięcie
uwzględniania katalogu, powrót do wyliczania ścieżki z klucza, usunięcie wywołania walidatora)
i sprawdził, że każdą łapie inny test. To jest różnica między „test istnieje" a „test coś pilnuje".
Mutacja z walidatorem zostawiła po sobie prawdziwy katalog `tentaflow-core/relative/` w drzewie
źródeł — czyli dowód, że guard jest nośny, a nie dekoracyjny.

## Recenzja T1 — trzy znaleziska warte zapamiętania poza tą pętlą

1. **Pusty test bezpieczeństwa jest gorszy niż brak testu.** `envelope_meta_cannot_forge_...`
   sprawdzał `make_context`, funkcję, która NIE PRZYJMUJE envelope'a, po czym podstawiał
   sfałszowany envelope już po wywołaniu. Zielony przy każdej regresji. Granica trzyma — krytyk
   próbował ją złamać samodzielnie i nie dał rady — ale dowodu nie było.
2. **Wymuszenie w konstruktorze działa tylko na tej warstwie, na której je postawisz.**
   `FlowRequestMeta::new` jest 3-argumentowy, ale `runtime::ExecutionContext::new(user)` i
   `AgentPrincipal::new(user, org)` piętro wyżej nadal domyślają po cichu — i dwa miejsca
   produkcyjne już na tym straciły stempel.
3. **`correlation_id` pochodził od klienta.** Identyfikator ramki protokołu binarnego, per
   połączenie, niepowtarzalny tylko w obrębie jednego gniazda. Dwa połączenia na ramce 7 dają ten
   sam klucz korelacji. To jest ten sam rodzaj błędu co przyjmowanie `origin` z treści modelu,
   tylko mniej oczywisty, bo wartość wygląda na serwerową.

---

# INCYDENT 2026-08-19 12:48 — masowe cofnięcie drzewa roboczego

## Co się stało

Jeden z procesów wykonał zbiorczy `git checkout` w **współdzielonym** drzewie roboczym. Cofnięte
zostały WSZYSTKIE zmodyfikowane pliki śledzone — zniszczona praca trzech ukończonych i już
zrecenzowanych torów:

- **T1** (`origin`/`actor`, ~38 plików),
- **R2a** (`graph_home` w `GraphManager`, 4 pliki),
- **M129** (migracja, 4 pliki),
- oraz część **T5a** (`www/css/controls.css`, 5 plików i18n — `tf-run-timeline.js` przetrwał, bo
  jest nieśledzony).

Git nie miał po tym śladu: brak stash, brak nowych obiektów, brak dangling commitów, brak gałęzi.
Zmiana nigdy nie była zacommitowana ani zastage'owana — bo commit jest bramką człowieka, o którą
nie prosiłem.

## Jak odzyskane

W scratchpadzie leżał `full_tracked.patch` (253 KB) — pełny `git diff` drzewa z 12:48, zrobiony
przez jednego z agentów tuż przed cofnięciem. Przywrócenie: `git apply` z wykluczeniem sześciu
plików, które tory T2/T3 zmodyfikowały PO incydencie (ich nowsza wersja musiała zostać).
Stan bieżący zabezpieczony przed operacją w `rescue-125736/`.

Odtworzone i zweryfikowane znaczniki: `pub enum FlowOrigin` (T1), `ensure_collection_at` (R2a),
`events_tracking_foundation` (M129), `FirstToken` (T3), `pub mod events` (T2).
65 plików, +3180/−310.

## Przyczyna źródłowa — moja, nie wykonawców

Puściłem do pięciu agentów naraz na JEDNYM drzewie roboczym i pozwoliłem krytykom mierzyć deltę
ostrzeżeń metodą „cofnij pliki do HEAD, zmierz, przywróć". Ta metoda jest poprawna w izolacji i
katastrofalna we współdzieleniu. Sam ją zaleciłem w instrukcjach, po tym jak M129 użył jej
prawidłowo — nie zauważyłem, że to, co bezpieczne dla jednego agenta, jest bronią przy pięciu.

## Wnioski wdrożone natychmiast

1. **Żaden agent nie wykonuje operacji git zapisującej do drzewa roboczego** — zakaz `checkout`,
   `restore`, `stash`, `reset`, `clean`, `apply`. Wysłane do wszystkich agentów w locie.
2. **Pomiar delty ostrzeżeń**: kopiuj własne pliki do katalogu tymczasowego, cofaj WYŁĄCZNIE
   ścieżki wymienione z nazwy, mierz, kopiuj z powrotem. Nigdy operacja na całym drzewie.
3. **Migawka przed każdą falą** — `git diff > snapshot.patch` zanim ruszy kolejna partia agentów.
4. Do rozważenia z człowiekiem: commity pośrednie na gałęzi roboczej jako sieć bezpieczeństwa
   (commit nie jest pushem; dziś brak commitów oznacza, że jedyną kopią jest drzewo robocze).
