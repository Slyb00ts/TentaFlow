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
| 2 | M129 poprawka | zdana | **wstrzymany** | — | dowód negatywny OK; testy do POWTÓRZENIA na kompilującym się drzewie |

## Kolejka (zablokowane zależnościami)

| Tor | Blokada |
|---|---|
| T2 `events.db` | **ODBLOKOWANE, w toku** |
| T3 `FirstToken` | **MEETS BAR** 11/11 (macierz 4 mutacji) — 3 usterki poza kryteriami |
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

## Dług weryfikacyjny po incydencie

Trzy tory mają dowody zebrane PRZED wywaleniem drzewa albo na drzewie, które chwilowo się nie
kompilowało. **Nie uznaję ich za potwierdzone**, dopóki nie zostaną powtórzone na drzewie zielonym:

| Tor | Co wymaga powtórzenia |
|---|---|
| M129 poprawka | pełny przebieg `cargo test -- db::migrations compliance::repository` |
| R2a poprawka | `cargo test --features graph -- services::graph` + `--test graph_host_functions` |
| T3 `FirstToken` | krytyk pracował na drzewie w trakcie incydentu — werdykt do potwierdzenia |

Zasada: fail-closed. Dowód zebrany na drzewie, które w międzyczasie zostało cofnięte, jest dowodem
o nieznanej podstawie.

## T3 — usterki poza kryteriami, do domknięcia w rundzie 2

1. **TTFT, który kłamie, na ścieżce bloku `loop`.** `terminal_stream_from` (`loop_block.rs:592-607`)
   syntetyzuje JEDNĄ deltę z całą zakumulowaną odpowiedzią, gdy pętla kończy się bez ostatniego
   przebiegu. Finalizer widzi niepustą deltę i emituje `FirstToken` — już po zakończeniu pętli.
   `NodeStarted → FirstToken` daje wtedy czas CAŁEJ pętli. **Inwariant 6 mówi: brak wyniku to luka
   w logu, nie zmyślony wynik.** Emisję na tej ścieżce trzeba stłumić.
2. **Błędne uzasadnienie w komentarzu.** `chunk_carries_first_token` powołuje się na „bramkę
   przekazywania" w `finalize_streaming_flow`, która tam nie istnieje (finalizer przekazuje każdy
   chunk bezwarunkowo). Reguła 6 — komentarz ma trafiać w DLACZEGO.
3. **`first_token` jako surowy identyfikator w UI.** `tf-agent-activity.js` ma gałąź `default:`,
   więc nie pada, ale renderuje nieprzetłumaczony `first_token` w osi aktywności Code Studio.

## Luki pomiarowe zgłoszone przez krytyka T3 (nie usterki — ograniczenia do świadomej akceptacji)

- **Ścieżka audio nie ma TTFT w ogóle.** Finalizer patrzy tylko na `EnvelopeDelta::Llm`; flow z
  `tts_stream_bridge` zamienia LLM→Audio przed finalizerem, więc przebieg głosowy jest niemierzalny.
- **Blok `loop`, iteracje pośrednie** jadą blokująco (`run_budgeted_iterations`) — brak delt, brak
  `first_token`. TTFT per krok istnieje tylko w inline'owym regionie pętli, nie w bloku `loop`.
- **Zagnieżdżone producenty strumienia raportują podwójnie** — `subflow`/`agent` dziedziczą
  `progress_scope` przez klon kontekstu, więc jeden fizyczny strumień daje dwa `FirstToken`,
  rozróżnialne tylko po `node_id`. Naiwne zapytanie z §2.7 zobaczy oba.

## NOWY TOR: T3b — mostek `ProgressEvent` → `events.db`

Kryterium etapu 3 („z logu da się policzyć TTFT") **NIE jest spełnione** samym T3. `src/events/`
definiuje `EventKind::FirstToken`, ale nie ma ANI JEDNEGO wołającego — punkt emisji istnieje,
ujścia brak. Dodatkowo `AgentRunEvent` nie niesie znacznika czasu, więc konsument drutu może
stemplować tylko moment odbioru. Bez tego toru T4 (metryki jako zapytania) nie ma na czym stanąć.

## R2a runda 2 — krytyk znalazł błąd, którego nie było w zleceniu

Werdykt: **DOES NOT MEET BAR** (kryt. 6 zdanie dwujęzyczne, kryt. 9 regresja formatowania).
Obie porażki mechaniczne. Wartość raportu leży w dwóch rzeczach, o które nikt nie prosił:

**F1 — wyścig kasujący pliki bez wiersza.** `seal_key_for_delete` czyta wiersz rejestru PRZED
wzięciem blokady slotu i kasuje spod lokalnie zapamiętanej ścieżki. Dopóki ścieżka była czystą
funkcją klucza, niezmiennik „nigdy osierocone pliki bez wiersza" (udokumentowany na
`delete_collection`) trzymał się z konstrukcji. `ensure_collection_at` usunął tę przesłankę:
równoległy twórca z własnym katalogiem wstawia wiersz i tworzy pliki pod blokadą, a kasujący
usuwa nic spod ścieżki z klucza i kasuje cudzy wiersz. **Komentarz napisany przez poprawkę
rundy 1 twierdzi, że to niemożliwe.**

**F5 — centralne zachowanie zmiany nie jest pilnowane przez żaden test.** Krytyk zmutował ścieżkę
kasowania tak, by ignorowała wiersz — **wszystkie 44 testy przeszły**. Nic w repo nie kasuje
kolekcji utworzonej przez `ensure_collection_at`.

**Wniosek metodyczny:** krytyk rundy 1 przeczytał testy i uznał je za nie-puste, bo każdy łapał
swoją mutację. Krytyk rundy 2 zapytał inaczej — „czy JAKIKOLWIEK test łapie tę zmianę" — i
odpowiedź brzmiała nie. To są dwa różne pytania i drugie jest ważniejsze.

---

# INCYDENT 2 — mutacje testowe w zacommitowanym kodzie

## Co się stało

Limit sesji ubił siedmiu agentów jednocześnie, w tym **dwóch krytyków w trakcie testowania
mutacyjnego**. Mutacje nie zostały przywrócone, bo agent nie zdążył. Ja zacommitowałem drzewo
(`06b52c59d`) bez sprawdzenia — i wprowadziłem do historii:

1. `events/store.rs:413` — `// MUTATION M2: seq allocated OUTSIDE the insert transaction`.
   **Wprost złamany inwariant 2.** Krytyk przebudował `write_event` na wariant z prealokacją i
   przeniósł alokację `seq` przed transakcję, żeby sprawdzić, czy test to złapie.
2. `services/graph/collection.rs:1360` — `// MUTATION B: always derive from the key, ignoring the
   registry row`. Defeat dokładnie tego zachowania, którego tor R2a broni.

## Jak wykryte

**Nie przeze mnie.** Zgłosił to wykonawca T3b, przy okazji własnej pracy — zauważył marker w pliku,
którego nie był właścicielem, i napisał o tym w raporcie zamiast zignorować.

## Naprawa

`store.rs` przywrócony z `5fb01dad5` (czysta wersja, zweryfikowane że nic z zewnątrz nie zależy od
`write_event_preallocated`). `collection.rs` posprzątany przez żywego agenta R2a po ostrzeżeniu.

## Wnioski wdrożone

1. **Skan przed każdym commitem** — `MUTATION|MUTANT|dbg!\(|todo!\(\)|unimplemented!\(\)` plus
   atrybucja AI. Zacommitowanie mutacji jest gorsze niż jej brak, bo wygląda jak kod produkcyjny.
2. **Każdy agent robiący mutację ma obowiązek zweryfikować grepem, że po nim nie został marker** —
   dopisane do instrukcji.
3. **Testowanie mutacyjne na współdzielonym drzewie jest z natury niebezpieczne.** Mutacja żyje
   w plikach, które w tym czasie widzi każdy inny agent i każdy commit. Docelowo krytycy powinni
   mutować na kopii, nie w miejscu — ale kopia wymaga własnego `target/`, co przy tym repo kosztuje
   dziesiątki GB. Świadomy kompromis, nie przeoczenie.

## Notatki wydaniowe — ryzyka do zakomunikowania przy wdrożeniu

1. **Okno osieroconych danych grafu.** Instalacja, która chodziła na pośredniej wersji (wiersz
   `addon_graph_collections` już autorytatywny, migracji 130 jeszcze nie), otworzyła PUSTY graf pod
   nieaktualną ścieżką. Migracja 130 przekieruje wiersz na prawdziwe dane, ale zapisy dokonane
   w tym pustym grafie zostają osierocone na dysku. Migracja nie może zrobić lepiej.
2. **`foreign_key_check` migracji 129 obejmuje CAŁĄ bazę.** Dowolne zastane naruszenie klucza
   obcego gdziekolwiek blokuje aktualizację i węzeł się nie uruchomi. Nie jest to nowa klasa
   ryzyka (osiem takich wywołań już istnieje), ale przy tej migracji trzeba to wiedzieć wcześniej.
3. **`SCHEMA_VERSION` 22 → 23** — stare binarki odpadają na handshake'u. Wszystkie węzły mesh
   muszą zostać przebudowane razem.

## Zastane usterki repo — znalezione po drodze, NIE przez nas spowodowane, NIE naprawione

1. `tentaflow-protocol`: `code_studio_variant_names_are_pinned` i
   `code_studio_response_field_names_are_pinned` padają na czystym HEAD (lista pinów 139, enum 141).
   Dowód niezależny od nas: `git diff d9d98c1fd -- tentaflow-protocol/src/code_studio.rs` jest
   PUSTY, a enum i lista pinów żyją w tym jednym pliku. **Blokuje T6** — bez naprawy nie odróżnimy
   własnej regresji od zastanej.
2. Kopia audytowa Code Studio nigdy nie jest drenowana — `audit_outbox::spawn_delivery_loop`,
   `workspace_db::spawn_idle_sweeper` i `checkpoint_all` nie mają wołających.
3. Sześć testów `sync::` pada tylko przy równoległym pełnym przebiegu; pod `--test-threads=1`
   przechodzą. Zagłodzenie sesji mesh z realnymi timeoutami, nie regresja.

---

# PRZEKAZANIE STANU — 2026-08-19 21:57

## Jeśli sesja zostanie przerwana, zacznij TUTAJ

**HEAD:** `f03dfa768` na gałęzi `feat/zdarzenia-rag`. 15 plików zmienionych, niezacommitowanych.
Migawka: `scratchpad/snap-2157/{tracked.patch,untracked.tgz}`.

### 1. NAJPIERW: sprawdź skażenie mutacjami

W chwili pisania **żyje jedna mutacja testowa**: `agents/principal.rs:81`, znacznik
`TF_REVIEW_MUTATION_4`, zostawiona przez krytyka T1 w trakcie pracy. Jeśli agent nie zdążył jej
cofnąć — **cofnij ręcznie** przed jakimkolwiek commitem.

```
grep -rn "MUTATION\|MUTANT\|MUTATED\|TF_REVIEW" tentaflow-core/src/ tentaflow/src/ tentaflow-protocol/src/
```
Zero trafień = można commitować. Cokolwiek innego = najpierw posprzątać.

### 2. Agenci, którzy byli w locie (mogą mieć niedokończoną pracę na dysku)

| Agent | Zadanie | Pliki |
|---|---|---|
| krytyk T1 | ocena pochodzenia, runda 2 | tylko odczyt + mutacje w `agents/`, `flow_engine/` |
| krytyk R3 | ocena jednej powłoki | tylko odczyt + mutacje w `node_adapters/`, `flows/` |
| T2 runda 2 | strażnik inwariantu 2, fabrykacja `org_id`, pula odczytu | `src/events/` |
| R1 runda 2 | tor robotnika, okna awarii | `services/ingest_jobs.rs`, `project_studio/ingest.rs`, `db/repository.rs`, `main.rs` |
| zapis do `flow_executions` | domknięcie kryterium etapu 1 | `flow_engine/executor.rs`, `db/{repository,models,migrations}.rs` |

### 3. Co jest domknięte i potwierdzone (nie ruszać)

- **T5a** oś czasu — 14/14, `tf-run-timeline.js` + `controls.css` + i18n ×5
- **T3** `FirstToken` — 11/11, macierz 4 mutacji
- **Migracje 129/130/131** + `SCHEMA_VERSION` 23 — schemat zdany, brakuje ZAPISU do `flow_executions`
- **R2a** graf — 3 rundy, wyścig naprawiony, pokrycie dodane
- **Plomba protokołu Code Studio** — przywrócona, T6 odblokowane

### 4. Kolejność, gdyby wznawiać

1. sprzątnij mutacje, zacommituj to, co przechodzi
2. dokończ rundy 2 (T2, R1, `flow_executions`) i ich krytyków
3. **T5b** — rejestr, inspektor, protokół, moduł nawigacji (największy pozostały kawałek)
4. **T6** — Code Studio + odsyłacz z audytu (odblokowane)
5. **R2b** — węzeł `graph_extract`
6. przejście porządkowe: tłumaczenia, „14 dni" → 30 w §2.9, komentarz „133 variants"

### 5. Bramki NIEZAPYTANE

- migracja danych addona RAG (§1.1 części 4–5) — **zmienia bundle hash**
- wejście w §1.2 G2
- **push** — nic nie było wypychane
- regres promptu czatu projektu (patrz niżej) — czeka na potwierdzenie krytyka

## INCYDENT 3 — mutacja bez markera przeszła przez skan

Trzeci raz mutacja krytyka trafiła do commita. Tym razem **skan jej nie wykrył**, bo była
podmianą wartości (`ctx.origin` → `FlowOrigin::default()`) bez żadnego komentarza-markera.

**Wniosek: grep po markerach jest niewystarczający.** Łapie tylko mutacje uprzejme.
Skuteczna zasada: **po masowym ubiciu agentów NIE commituj, dopóki każdy tor nie potwierdzi
stanu własnych plików.** Commit tuż po ubiciu trafia dokładnie w moment, w którym mutacje są
najbardziej prawdopodobnie żywe.

Przy tej samej okazji wyszła druga, cudza: `TF_REVIEW_MUTATION_1` w `executor.rs` pozwalała
`envelope.meta["origin"]` nadpisać `ctx.origin` — żywe złamanie inwariantu 1.

---

# SESJA 2026-08-20 — wznowienie

## Metoda: izolacja mutacji zamiast dyscypliny

Trzy incydenty tej pętli mają jedną przyczynę: mutacja żyjąca w drzewie, które w tym czasie
kompiluje ktoś inny. Skan po markerach jej nie łapie (incydent 3), a dyscyplina agentów zawodzi
przy ubiciu sesji (incydent 2). Zamiast czwartej prośby o ostrożność wprowadzone są dwa
mechanizmy:

1. **Jeden `flock` na wszystkie wywołania cargo.** Agenci wołają `tfcargo` zamiast `cargo`,
   a CAŁY cykl mutacji (kopia → mutacja → test → przywrócenie → odczyt z powrotem) jedzie w
   jednym skrypcie pod `tfmutate`, czyli pod tym samym zamkiem. Nikt nie skompiluje drzewa,
   w którym ktoś inny ma żywą mutację — to własność mechanizmu, nie obietnica agenta.
2. **Migawka SHA-256 1495 plików źródłowych** przed falą; `verify-tree.sh` wypisuje każdy plik
   różny od migawki. Łapie mutację bez markera, czyli dokładnie ten przypadek, który przeszedł
   przez skan w incydencie 3.

Do tego rozłączne zbiory plików do mutacji per agent. Koszt: cargo jest zserializowane, więc
fala trwa dłużej (jeden krytyk odczekał ~90 min na niefiltrowany `cargo test --lib` sąsiada).
Wniosek na przyszłość: **każdemu krytykowi narzucać filtr testów**, nie zostawiać wyboru.

## Baseline zmierzony na tym drzewie (2026-08-20)

- `cargo check --lib` / `--tests` / `--tests --features test-support` — wszystkie kompilują się.
  30 ostrzeżeń lib, 37 lib-test — wszystkie zastane.
- `tentaflow-protocol`: `cargo test --lib` = **200 passed, 0 failed**. Plomba Code Studio
  (zastana usterka blokująca T6) jest naprawiona — T6 odblokowane.
- Zastane czerwone, nie nasze: `e2e_smoke_cbor_test` (3), `vector_gate_enforcement` (izolacja),
  6 testów `sync::` (tylko równolegle), `code_studio_lifecycle_e2e` (1).

## Runda: T3b mostek `ProgressEvent` → `events.db` — ślepy krytyk

**Werdykt: DOES NOT MEET BAR** — kryteria B4 i B12 z 12. Jedenaście mutacji, każda złapana.

**Odpowiedź na pytanie 2 (czy COKOLWIEK pilnuje sensu toru): TAK.** Mutacja `attach()` → no-op
(mostek przestaje subskrybować, wszystko inne kompiluje się i działa) czerwieni 8 testów, w tym
oba liczące TTFT i dekodowanie. To jest tor z realnym pokryciem własnej tezy — w odróżnieniu od
R2a rundy 2, gdzie analogiczna mutacja przeszła przez 44 testy.

**B4 NIE SPEŁNIONE i jest to wada konstrukcyjna, nie kosmetyka.** `ProgressEvent` nie niesie
ŻADNEGO znacznika czasu, więc `at_ms` to czas ODBIORU w batchującym konsumencie, nie czas
zdarzenia. Autor zapisał to wprost (`progress_log.rs:27`) zamiast ukryć — to na plus — ale
kryterium mówi o czasie zdarzenia. Objaw potwierdzający: asercje TTFT dopuszczają `200..600` ms
dla pauzy 220 ms, a dekodowanie `240..700` dla 260 ms. Tolerancja 2–3× wartości mierzonej to
dokładnie cena stemplowania przy odbiorze.

**B12 NIE SPEŁNIONE** — trzy komentarze mijające się z kodem (`stop()` „wołane obok
`checkpoint_wal`" nie ma wołacza; trzy miejsca twierdzą, że TTFT liczy się
`request_started → first_token`, a liczy się `step_start → first_token`), nieużywane API i jedna
nowa linia po polsku (`db.rs:5`).

### Znaleziska krytyka poza kryteriami — do rozstrzygnięcia

1. **Kolizja scope'u może podmienić atrybucję wierszy.** Scope rozgłoszenia to `session_id` —
   identyfikator mintowany przez KLIENTA. `bind_session_owner` jest first-writer-wins, ale
   `bind_run_provenance` nadpisuje BEZWARUNKOWO. Drugi principal wysyłający pod cudzym
   `session_id` przestemplowuje pochodzenie żywego scope'u i pozostałe zdarzenia pierwszego
   użytkownika lądują z `actor_id`/`org_id` drugiego. Wartości nadal są mintowane przez serwer
   (inwariant 1 stoi), ale KTÓRY stempel wyląduje, da się sterować kluczem od klienta.
   To ta sama klasa co `correlation_id` od klienta z rundy T1.
2. **`EventPayload::redacted()` kończy się arm-em `other => other`**, podczas gdy `translate()`
   i `to_wire()` są celowo wyczerpujące. Przyszły wariant z wolnym tekstem cicho ominie
   redakcję — czyli dokładnie ta awaria, przed którą broni B2, w jedynej funkcji, gdzie kosztuje
   najwięcej (inwariant 3).
3. **`tool_durations` nie bierze PIERWSZEGO wyniku po wywołaniu** (`step_latencies` bierze),
   więc `call_id` powtórzony w jednym przebiegu daje duplikaty i pary krzyżowe.
4. **Kopia audytowa jest martwa w produkcji.** `security_relevant()` jest prawdziwe tylko dla
   `RequestStarted`, a nikt nie zapisuje `RequestStarted` ani `AssistantMessage`. Więc
   `events::init` startuje pętlę dostarczania nad trwale pustym outboxem — a doc modułu
   reklamuje ten plik właśnie jako naprawę nigdy-nie-drenowanego outboxu Code Studio.
   Razem z tym martwa jest CAŁA maszyneria opt-inu na treść odpowiedzi (`ResponseBody`,
   `BodyOmission`, `assistant_body_setting_key`) — commit, który ją dodał, nie ma czego włączać.
   **Skutek dla etapu 4: T4.3 i T4.4 nie mają na czym stanąć w obecnym kształcie.**
5. **`run_provenance` to mapa bez ograniczenia** z jedną ścieżką sprzątania.
