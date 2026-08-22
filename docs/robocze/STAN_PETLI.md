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

---

# INCYDENT 4 — 2026-08-20: limit sesji ubił falę, mutacje odżywały same

## Co się stało

Limit sesji ubił trzech krytyków naraz (T1, R2a, R3). Po ubiciu w drzewie ŻYŁY dwie mutacje,
obie bez markera:

1. `flow_engine/executor.rs` — cztery miejsca, blok preferujący `envelope.meta` nad kontekstem
   dla `origin`/`actor_*`/`correlation_id`. **Złamany inwariant 1.** To jest dokładnie ta sama
   mutacja, która trafiła do historii w incydencie 3.
2. `flow_engine/dispatcher.rs` — `make_context` przestaje kopiować `meta.origin`/`meta.actor_kind`,
   podstawia `FlowOrigin::System` / `ActorKind::System`.

**Nowość względem incydentów 1–3: mutacje ODŻYWAŁY po sprzątnięciu.** Krytyk T1 uruchomił
`supervise.sh` — odczepiony proces (PPID 1), który po każdym ubiciu relaunchował `all.sh`,
a `all.sh` na starcie nakładał mutacje od nowa. Trzy rundy sprzątania minęły, zanim znalazłem
nadzorcę zamiast jego dzieci. Skan po markerach nie miał czego znaleźć.

## Co zadziałało

**Migawka SHA-256 całego drzewa źródeł, robiona PRZED falą.** To ona wskazała oba pliki —
`git status` też je pokazał, ale migawka jest jedynym mechanizmem, który odróżnia „agent to
zmienił" od „tak miało być", niezależnie od tego, czy zmiana ma marker. Przywrócenie poszło
z migawki, **nie z kopii zapasowych krytyka** (`bk-*.rs`): jego kopie powstawały na starcie
jego skryptu, więc mogły utrwalić mutację z przerwanego wcześniej przebiegu.

## Co zawiodło w MOIM narzędziu

`flock <plik> cargo …` bierze deskryptor, a `cargo` odpala `sccache` (globalny `rustc-wrapper`
w `~/.cargo/config.toml`), który **demonizuje się i dziedziczy ten deskryptor**. Blokada żyła
po wyjściu cargo i zakleszczyła całą falę na godziny. Poprawka: `flock -o`, który zamyka
deskryptor przed `exec`, plus wstępne wystartowanie serwera `sccache` POZA blokadą.

Druga wada: jeden niefiltrowany `cargo test --lib` trzymał wspólną blokadę ~90 minut i zagłodził
pozostałych krytyków aż do ich limitu sesji. Sufit równoległości narzucił limit, nie konflikt
plików — dokładnie jak mówi §2.6 briefu.

## Wnioski wdrożone (mechanizmy, nie prośby)

1. **Zakaz procesów odczepionych i samorestartujących** — `setsid`, `nohup`, nadzorca, pętla
   ponawiania, `&` przy czymkolwiek mutującym. Brak blokady = czekaj na pierwszym planie albo
   zgłoś kryterium jako NIEZWERYFIKOWANE.
2. **Jedna mutacja na skrypt**, przywrócenie przed wyjściem. Skrypt z sześcioma mutacjami, który
   pada na czwartej, zostawia cztery żywe.
3. **Obowiązkowy filtr testów.** Goły `cargo test --lib` jest zakazany.
4. **`flock -o`** + rozgrzany `sccache` przed falą.
5. **Praca SEKWENCYJNA** przy napiętym limicie — wznawiam krytyków po jednym. Utrata trzech
   torów naraz jest ceną równoległości, a nie ceną konfliktu plików.

## Zastana pułapka repo, znaleziona przy okazji (NIE nasza)

**Śledzone artefakty builda przepisywane przez lokalny build.** `www/js/voxel/voxel_glue.js`
(hashe symboli wasm-bindgen zależne od wersji narzędzia) i `vllm-recipes/recipes.json.gz`
(rekompresja) zmieniają się po każdym `cargo build`, choć są w gicie. Wyglądają jak cudza zmiana
w `git status` i kosztowały mnie osobne śledztwo, czy któryś agent nie wyszedł poza zakres.
Nie naprawiam — zgłaszam.

---

## Runda: T1 pochodzenie (runda 2) — ślepy krytyk, po scaleniu main

**Werdykt: MEETS BAR** — T1.1–T1.8 wszystkie SPEŁNIONE. Pomiary wzięte po scaleniu, na
`505fed924`; baseline toru: **34 testy, wszystkie zielone**.

**Odpowiedź na pytanie 2 (czy COKOLWIEK pilnuje sensu toru): TAK, na dwóch warstwach.**
Mutacja: wszystkie CZTERY punkty wejścia egzekutora (`execute_blocking`,
`execute_direct_blocking`, `execute_direct_streaming`, `execute_streaming`) zaczynają
przedkładać `envelope.meta` nad kontekst dla wszystkich pięciu pól. Czerwienią się dwa
niezależne testy: co węzeł widzi w czasie wykonania ORAZ co ląduje w wierszu
`flow_executions`. Oba są prawdziwe — nie ten pusty test bezpieczeństwa z rundy 1, który
sprawdzał funkcję nieprzyjmującą envelope'a.

**Pułapka `correlation_id` od klienta jest ROZBROJONA i udokumentowana w kodzie.** Na bazie
`flow_invoke_handler` używał identyfikatora ramki protokołu — per połączenie, wybieranego przez
klienta. Tor zamienił to na UUID mintowany PO autoryzacji, z uzasadnieniem zostawionym
w `stream_handlers.rs:410-414`.

**Krytyk zweryfikował także MOJĄ poprawkę scaleniową** (`tests/openai_embeddings.rs` →
jawny kontekst zamiast `ExecutionContext::default()`) i potwierdził niezależnie, że nie jest to
zaklejenie jednego miejsca: zero wystąpień `*::default()` dla typów niosących pochodzenie,
żadna struktura z polem pochodzenia nie wywodzi `Default`, a wszystkie cztery konstruktory biorą
pochodzenie jako argument obowiązkowy.

### Dług T1 do domknięcia (zlecony, nie zamknięty tym werdyktem)

1. **`Default` nadal wywodzone na `FlowOrigin`/`ActorKind`/`FlowActor` z `#[default] System`.**
   Martwe (usunięcie kompiluje się czysto, liczniki ostrzeżeń bez zmian) i **przywraca dokładnie
   ten cichy stempel `system`, którego cała reszta projektu zakazuje** — `ExecutionContext`,
   `AgentPrincipal`, `CallProvenance`, `TtsRequest`, `VisionOcrRequest`, `VisionClassifyRequest`
   świadomie `Default` straciły. Jedno `..Default::default()` w przyszłej strukturze i inwariant
   znów jest otwarty.
2. **Dziedziczenie pochodzenia przez sub-agenta (T1.6) NIE MA ŻADNEGO TESTU.** Mutacja
   zamieniająca dziedziczenie na `Agent` + `system()` **przeżyła 26 testów** (`subagent_reactor`
   5, `principal` 7, `run_manager` 14 — zero porażek). Przyczyna jest w dublerze:
   `SpyDispatch::dispatch(..., _principal)` wyrzuca principala, więc żadna asercja nie ma czego
   zobaczyć. To ten sam wzorzec co F5 z R2a: test istnieje, ale nie ma jak upaść.
3. **Per-call `correlation_id` compliance nadal czytany z `envelope.meta`** w sześciu adapterach
   (`llm.rs:548`, `llm.rs:865`, `agent_router.rs:255`, `compact_context.rs:547`,
   `vision_llm.rs:231`, `vision_parse.rs:159`), podczas gdy ten na poziomie przebiegu jest już
   polem struktury. Dziś się zgadzają, bo punkty wejścia piszą obie wartości tak samo — ale
   `envelope.meta` jest zapisywalne przez bloczek addona, więc skompromitowany bloczek może
   wskazać `compliance_ai_events` na sfałszowaną korelację, podczas gdy `run_events`,
   `audit_log` i `flow_executions` trzymają prawdziwą. `ctx.correlation_id` jest dostępny w
   każdym z tych miejsc. **Luka zastana (§3.4), nie wniesiona tym torem** — ale to jedyne
   miejsce, gdzie nowy inwariant nie jest zastosowany.
4. Luki pokrycia: punkty wejścia **tts / document / stt** bez testu pochodzenia; trzy handlery
   strumieniowe (Chat / Project / CodeStudio) bez testu jednostkowego (zgłoszone już przez
   wykonawcę rundy 1 — krytyk potwierdza, że stoją na kompilacji i przeglądzie).

---

## Runda: R2a `graph_home` w `GraphManager` (runda 3) — ślepy krytyk, po scaleniu main

**Werdykt: DOES NOT MEET BAR — wyłącznie C7** (cztery nowo dopisane linie komentarza po polsku
w akapicie, który w połowie zdania przechodzi na angielski). C1–C6, C8, C9 spełnione z własnymi
dowodami krytyka. Dziewięć cykli mutacyjnych, każdy w osobnym skrypcie pod blokadą, każdy
zakończony porównaniem bajtowym z migawką.

Baseline po scaleniu: `services::graph` **46 passed**, `graph_host_functions` **18 passed**.

**Odpowiedź na pytanie 2: TAK — teraz jest pilnowane.** Mutacja, która w rundzie 2 przeszła
przez 44 testy (kasowanie ignoruje wiersz rejestru i wylicza ścieżkę z klucza), czerwieni dziś
dwa testy — dokładnie te dwa, które poprawka dołożyła. To jest domknięcie znaleziska F5 z
rundy 2.

Odnotowane dla porządku: **integracyjny `graph_host_functions` (18 testów, w tym oba testy
odinstalowania) przechodzi tę mutację bez mrugnięcia** — używa wyłącznie drzewa addona, więc cały
scenariusz osieroconych plików jest dla niego niewidoczny. Pilnują tylko testy lib.

**Wyścig z rundy 2 (F1) naprawiony i udowodniony.** Kod czyta wiersz DWA razy: raz przed
blokadą (tylko do zasiania kanonicznego wpisu) i raz PO wzięciu blokady, i to ten drugi odczyt
rozstrzyga, co zostanie skasowane. Mutacja przywracająca stary kształt (rozstrzygnięcie przed
blokadą, działanie na zapamiętanej ścieżce) czerwieni
`test_delete_resolves_path_after_lock_no_orphan`. Stare testy wyścigu nie mogły tego złapać —
używają wyłącznie ścieżki domyślnej, gdzie zasiew i wiersz są takie same.

### Znaleziska poza kryteriami — do rozstrzygnięcia

1. **F1: `ensure_collection_at` NIE MA ŻADNEGO WOŁAJĄCEGO W PRODUKCJI.** Publiczne API, jego
   ścieżka kwot, walidacja i migracja v130 wyszły przed konsumentem. Konsumentem ma być węzeł
   `graph_extract` (tor R2b, §5c briefu, w zakresie ale NIEZACZĘTY). Dopóki R2b nie wyląduje,
   jest to „wire this up later", czyli reguła 1 `CLAUDE.md`. **Wniosek dla kolejności prac:
   R2b przestaje być opcjonalne — albo wchodzi, albo R2a trzeba wycofać.**
2. **F2: walidator dopuszcza DOWOLNĄ ścieżkę bezwzględną, a kasowanie grafu jest rekurencyjne.**
   `validate_custom_dir` wymaga tylko: bezwzględna + bez `..`. Nie zamyka katalogu w obszarze
   danych i nie kanonizuje, więc dowiązanie symboliczne ucieka. Kolekcje grafu są KATALOGAMI, a
   `remove_cozo_files` robi na katalogu `remove_dir_all`. Kto w końcu zawoła `ensure_collection_at`,
   dostaje prymityw „utwórz i rekurencyjnie skasuj `<dowolny katalog>/<kolekcja>.cozo`".
   **To jest wymaganie projektowe dla R2b, nie kosmetyka:** `graph_home` musi być mintowane przez
   serwer i zamknięte w katalogu projektu (inwariant 1), bo dziś nie egzekwuje tego NIC.
3. **F3:** na ścieżce `ensure_collection_at` `org_id` nie wchodzi do ścieżki pliku, więc dwie
   organizacje z tym samym katalogiem trafiają w ten sam plik. Uczciwie udokumentowane w nagłówku
   pliku, ale nieegzekwowane kodem — znów: obowiązek przyszłego wołającego.
4. **F4:** `test_delete_cache_miss_resolves_nonempty_path` pilnuje czegoś innego, niż mówi jego
   nazwa (mutacja `None => PathBuf::default()` zostawia całe 46 testów zielonych). Zlecone.
5. **F5 (ZASTANE, nie z tej zmiany, nieudowodnione):** wzajemne wykluczenie kasowania i tworzenia
   opiera się na tym, że oba wątki trafią na ten sam kanoniczny `Arc<GraphEntry>`. `evict_to_cap`
   może go usunąć z mapy w oknie między `canonical_entry_for` a wzięciem blokady slotu;
   `seal_key_with_seed` nie sprawdza po wzięciu blokady, czy jego wpis nadal jest kanoniczny.
6. **F6 (ZASTANE):** `tests.rs::tempdir()` ma zaszyty fallback `/mnt/e/repos/rust/_scratch/...`,
   więc na maszynie bez tej ścieżki CAŁY zestaw grafu pada na `PermissionDenied` zamiast pominąć
   się albo wytłumaczyć. Trzeba ustawiać `TMPDIR`, żeby cokolwiek uruchomić.
7. **F7:** praca towarzysząca jest zdrowa — migracja v130 naprawia wiersze osierocone przez starą
   migrację katalogu danych i ŚWIADOMIE nie rusza wierszy niekończących się ogonem z klucza
   (czyli kolekcji z `ensure_collection_at`); `storage_admin::run_live_migration` przepisuje
   `file_path`. Bez tego uczynienie wiersza autorytatywnym byłoby cichą utratą danych na każdej
   instalacji, która przeniosła katalog danych.

**Sprzątnięte:** `tentaflow-core/relative/` — pozostałość po mutacji zdejmującej walidator
(test utworzył prawdziwy katalog). Ta sama pozostałość co odnotowana wcześniej; tym razem usunięta.

---

## Runda: R3 jedna powłoka retrievalu — ślepy krytyk, po scaleniu main

**Werdykt: DOES NOT MEET BAR — wyłącznie R3.8** (dziesięć nowo dopisanych linii po polsku:
komunikaty asercji, `expect`, fixtury i jeden doc comment). R3.1–R3.7 spełnione, każde
z mutacją dowodzącą, że pilnujący test jest czuły. Dziesięć mutacji, każda w osobnym skrypcie
pod blokadą.

**Odpowiedź na pytanie 2 jest ROZSZCZEPIONA i to jest najważniejsze zdanie tego raportu.**

- **Strona definicji flow i adapterów: TAK.** Obie inwersje sensu toru czerwienią testy —
  „węzeł `output` ignoruje tryb i zawsze emituje blok z cytatami" oraz „fallback modelu
  ignoruje envelope i zawsze bierze `rag-llm`". Ten tor NIE powtarza porażki R2a rundy 2.
- **Strona wołającego: NIE. Zero pokrycia.** Czat projektu wnosi do tego toru dokładnie trzy
  linie i **żadna nie jest pilnowana**: skasuj `dispatch_by_flow_id_streaming(RAG_QUERY_FLOW_ID)`
  i wskaż cokolwiek innego — zestaw zielony; skasuj stempel modelu — każdy czat projektu po
  cichu odpowiada na `rag-llm`, zestaw zielony; skasuj stempel `output_mode` — zielony jest
  i zestaw, i RUNTIME, bo czat czyta tylko `meta["rag_citations"]`, nigdy payloadu.

  Czyli **dokładnie ta regresja, przed którą stoi R3.4 — czat po cichu tracący własny model —
  jest łapana przez testy kontraktu flow i przez NIC na okablowaniu.** Krytyk nie mógł tego
  zmutować: `dispatch/stream_handlers.rs` był poza jego zakresem. Żaden test integracyjny nie
  przechodzi wspólnej powłoki end-to-end z żadnej z dwóch stron.

**R3.5 potwierdzone: reranker NIE został „naprawiony".** Cały diff `reranker.rs` to dwie linie
(stempel pochodzenia), a `retrieval_round.flow.json` jest bajtowo identyczny. Kształt wejścia
niezmieniony, degradacja do kolejności wektorowej nadal obecna i nadal ograniczona do tej
ścieżki. Błędna diagnoza z §1.3 specyfikacji pozostaje błędna — dobrze, że nikt jej nie „poprawił".

### Decyzje właściciela po raporcie R3 (2026-08-20)

| Znalezisko | Decyzja |
|---|---|
| **Persona czatu projektu zmieniła się po cichu** — stary `ps-chat` pozwalał odpowiadać poza bazą wiedzy, wspólna powłoka zabrania | **ZOSTAJE nowe zachowanie.** Do notatek wydaniowych + test pinujący, żeby nie zmieniło się znowu niezauważenie |
| **Projekt bez przypisanego agenta czatu po cichu odpowiada na modelu platformowym** (`model_fallback: rag-llm`) zamiast błędu | **ZDJĄĆ fallback dla czatu projektu.** Głośny błąd zamiast cichej odpowiedzi na modelu, którego właściciel nie wybrał. Addon zachowuje `rag-llm` jako swój prawdziwy domyślny — fallback zostaje we flow, refuse jest po stronie handlera (addon to bundle WASM, którego nie wolno przebudować) |
| **`recent_conversation_messages` kluczowane wyłącznie po `session_id`**, bez zakresu org/user, a `/v1` bierze `session_id` dosłownie od wołającego | **ZASCOPE'OWAĆ do właściciela.** Wzorzec: czaty Project Studio filtrowane twardo po `user_id` bez obejścia dla admina. **To naprawa usterki ZASTANEJ** — magazyn był nieoscope'owany od dawna, ten tor tylko poszerzył osiągalną powierzchnię (powłoka `query` nie miała wcześniej węzła historii) |

### Znaleziska R3 poza kryteriami — nie naprawione, do raportu

- **F6: nowe klucze konfiguracji `llm` nie istnieją w palecie Flow Buildera.**
  `model_meta_key`, `model_fallback`, `require_session`, `output_mode` — `grep` po `www/` pusty.
  Użytkownik edytujący węzeł `llm` w builderze nie ma UI dla kluczy, na których stoi flow platformowy.
- **F7: cała unifikacja wylądowała w `06b52c59d [wip]: checkpoint after session interruption`**,
  a nie w opisanym commicie `[feat]`. Konwencja repo `[type]: description` nie została zachowana.
- Zastane `ps-chat` w treści produktowej: `seed.rs:883` opis `config_schema` węzła
  `project_knowledge` (widoczny w Flow Builderze) i prefiks identyfikatora żądania
  `format!("ps-chat-{correlation_key}")` — ten drugi świadomie nietknięty, bo może występować
  w złączeniach audytu, więc zmiana nazwy to zmiana zachowania, nie porządki.

---

## Decyzje R3 wykonane (2026-08-21)

| Decyzja | Wykonanie | Dowód |
|---|---|---|
| Persona czatu ZOSTAJE | test `shell_answer_prompt_grounds_the_model_in_the_retrieved_context` pinuje ZOBOWIĄZANIA promptu (kontekst + „nie wiem" + zakaz zmyślania), z alternatywnymi sformułowaniami — przeredagowanie przechodzi, usunięcie warunku pada | mutacja: podmiana promptu na ps-chatowy → test czerwony; po przywróceniu sha256 zgodny |
| Fallback modelu ZDJĘTY dla czatu projektu | odmowa w punkcie wejścia, PRZED dispatchem i PRZED utrwaleniem wiadomości użytkownika; komunikat nazywa brak („przypisz agenta czatu w ustawieniach projektu"). Flow zachowuje `model_fallback` — to prawdziwy domyślny model addona, a addona (bundle WASM) nie wolno przebudować | mutacja: przywrócenie cichego fallbacku → test czerwony, i to z komunikatem „dispatch failed: pre-producer node 'hops' failed" — czyli dowód, że bez bramki właściciel dostaje błąd wewnętrzny zamiast instrukcji |
| Historia ZASCOPE'OWANA | **zmiana planu po rekonesansie** — patrz niżej | 6 testów + 2 mutacje |

### Dlaczego historia NIE dostała predykatu, tylko mintowany klucz

Wykonawca zatrzymał się i **nie zrobił niczego**, zamiast dowieźć naprawę pozorną. Powód:
`conversation_messages` **nie ma ŻADNEJ kolumny właściciela** — ani user, ani org, ani tabeli
sesji do złączenia (`id, session_id, seq, role, content, tool_calls, tool_call_id, name,
payload_ref, payload_kind, node_id, created_at, reasoning_content, citations_json`). Nie ma czego
filtrować. Predykat po stronie odczytu albo by się nie skompilował, albo traktowałby „brak
właściciela" jako wildcard — czyli nie naprawiałby nic.

Dodatkowo zauważył, że `org_id` **nigdy nie jest ustawiane na ścieżce `/v1`**, więc filtr po
organizacji zdegenerowałby się do „NULL pasuje do NULL" **dokładnie na ścieżce ataku**.

Decyzja właściciela: zamiast migracji — **mintowanie klucza w punkcie wejścia**. Skoro problem
brzmi „adres wybiera wołający", to serwer zaczyna składać adres. Cudzej rozmowy nie da się
zaadresować Z KONSTRUKCJI, bez kolumny, bez migracji, bez zmiany ścieżki zapisu.

Preimage jest **prefiksowany długościami obu części**, więc nie istnieje separator do przesunięcia:
`("u", "a:b")` i `("u:a", "b")` kodują się różnie. Wykonawca sam zgłosił, że własność bezpieczeństwa
opiera się na odporności SHA-256 na kolizje, a nie na bijekcji, i że wariant jawnotekstowy jest
obronialną alternatywą — odnotowane, wybór zostaje (ogranicza długość klucza i nie wpuszcza
identyfikatorów użytkowników do tabeli rozmów).

**Koszt zaakceptowany świadomie:** zewnętrzny klient `/v1` liczący na pamięć między żądaniami raz
zaczyna od pustej rozmowy (stare wiersze są nieosiągalne, nie skasowane), a status przebiegu
agenta odpytywany po identyfikatorze, który klient wysłał, nie rozwiąże się. Do notatek wydaniowych.

### NOWE ZNALEZISKO: druga furtka tej samej klasy — NIE naprawiona

Wykonawca przeszukał drzewo i znalazł **drugi punkt wejścia o tym samym kształcie**, w pliku poza
jego zakresem, więc go nie ruszył:

- **`dispatch/stream_handlers.rs:113`** — czat pulpitu. `stream_req.session_id` (od klienta, po
  protokole binarnym) opakowane w `MemoryOptions` i podane temu samemu builderowi.
- **`dispatch/stream_handlers.rs:430`** — ścieżka `FlowInvoke`, przypisanie wprost.

Ta sama tabela, ten sam brak właściciela. Różnica w osiągalności: `/v1` wymagało tylko klucza API,
te dwa wymagają uwierzytelnionej sesji pulpitu — więc to user-do-usera, nie najemca-do-najemcy.
Ale zalogowany użytkownik nadal może nazwać cudzy identyfikator rozmowy.

**Rekomendacja wykonawcy (podzielam): NIE naprawiać przez przekluczowanie** — osierociłoby to każdą
istniejącą rozmowę pulpitu, a te identyfikatory są już serwerowymi UUID-ami pokazywanymi w UI.
Wzorzec w repo: `dispatch/run_events.rs:196-211` odmawia `AgentRunEventScope::Session`, dopóki
`progress_broker.session_owner(session_id)` nie zmapuje jej na wołającego. Sprawdzenie własności
na tym samym modelu pasuje tu bez zmiany żadnego zapisanego klucza.

**Do decyzji człowieka.** Nie wchodzę w to z własnej inicjatywy.

Zweryfikowane jako poprawnie mintowane przez serwer i świadomie nietknięte: czat Project Studio
(`chat.session_id` z wiersza `project_chats`, tworzony `Uuid::new_v4()`, wyszukiwany z twardym
filtrem po `user_id`) oraz `meeting/flow_turn.rs` (`meeting_id`). Test
`non_api_origins_keep_their_server_chosen_session_id` pinuje, że te ścieżki zostają nietknięte.

---

## Wznowienie po limicie sesji (2026-08-21, po południu)

**Stan drzewa po ubiciu krytyka `flow_executions`:** czysty. Krytyk zginął ze słowami
„teraz próba podrobienia E2", czyli **przed** nałożeniem mutacji podrabiającej `envelope.meta`
w `flow_engine/executor.rs`. Potwierdzone nie grepem, tylko porównaniem: zmodyfikowane pliki to
dokładnie zestaw T3b rundy 2 (966 linii = raportowane +781/−185), a `flow_engine/executor.rs`
i `db/**` nietknięte. Żadnych osieroconych procesów.

**T3b runda 2 zacommitowana** jako `41db12484`.

**Odbudowa mechanizmu izolacji mutacji.** Katalog roboczy sesji został wyczyszczony przy
restarcie maszyny, więc blokada i migawka z INCYDENTU 4 przepadły. Odtworzone:
`snapshot.sh` (migawka wszystkich śledzonych plików, własność ORKIESTRATORA),
`verify-tree.sh` (porównanie drzewa z migawką — wykrywa mutację bez markera, czego grep nie robi),
`with-mutation-lock.sh` (`flock -o`, żeby sccache nie odziedziczył deskryptora blokady).

Dwie pułapki po drodze, obie tej samej klasy co wcześniejszy błąd `sha256sum -c`:
`cp -p` na śledzonym SYMLINKU do katalogu spoza repo (`mockups/component-catalog/face-data`)
próbuje kopiować katalog — potrzebne `cp -Pp`; oraz **zlokalizowany `diff` pisze „Tylko w",
nie „Only in"**, więc filtr plików nieśledzonych przepuszczał wszystko. `LC_ALL=C` jest
nośne, nie kosmetyczne — bez niego `verify-tree.sh` raportowałby czyste drzewo zawsze.

---

## Rekonesans przed trzema niezaczętymi torami (2026-08-21)

Trzy agenty tylko czytające, bez kompilacji i bez edycji. Wynik zmienia treść trzech zleceń,
a jedno zadanie z §5b briefu **kasuje**.

### §5b.1 — „`CODE_STUDIO_PLAN.md` §16.2 mówi 9 bloków, kod ma 10" — **NIEPRAWDA, zadanie odpada**

Wszystkie trzy człony zarzutu obalone dowodem:
- §16.2 (`:1485`) mówi **10 bloków**, graf `:1487-1498` wymienia dokładnie dziesięć, a kod
  (`seed.rs:1850-1874`, 7 wspólnych + `patch_review` + `persist_turn` + `output`) daje 10.
  Doc comment `seed.rs:1844` też mówi „10 blocks". **Zgodność, nie rozjazd.**
- `patch_review` **NIE MA** w tabeli „czego tu nie ma" (`:1536-1542`, cztery wiersze). Został
  z niej usunięty świadomie, z uzasadnieniem wypisanym wprost w `:1544-1551`.
- §16.4 istotnie pisze „opcjonalny", ale w znaczeniu „opcjonalny jako blok dla flow SPOZA
  harnessu" — §9.1 (`:983`) godzi obie sekcje wprost.

Jedyny nieaktualny tekst to dwa wiersze changelogu 1.7→1.8 (`:31-32`), opisujące stan sprzed
poprawki. Changelog z definicji opisuje przeszłość. **Nie ruszam.**

### `audit_log.correlation_id` — brief mówi „nikt nie pisze"; to już nieaktualne

Pisarz **istnieje**: `events/audit_outbox.rs:242-274`, z testem
`the_delivered_row_carries_the_correlation_link_and_the_chain` (`:414-435`). Powstał na TEJ
gałęzi, więc brief opisywał stan sprzed własnej pracy. Brakuje **wyłącznie strony odczytu**:
`AuditLogEntry` (`message_body.rs:1074`) nie ma pola, `list_audit_logs`
(`repository.rs:12269`) go nie SELECT-uje, `audit.js:331` nie pokazuje.

**Pułapka:** `correlation_id` jest ŚWIADOMIE poza łańcuchem haszy (`audit_outbox.rs:266-269`).
Dodanie go do `AuditRowHashInput` unieważniłoby każdy istniejący hasz. Nie wolno.

### Pozostałe ustalenia, które wchodzą do zleceń

- **`ensure_collection_at` ma ZERO produkcyjnych wołających** — potwierdzone wyczerpująco
  (wszystkie trafienia to `services/graph/tests.rs` albo komentarze). R2b będzie pierwszym.
- **`graph_home` nie istnieje nigdzie** — trzeba odtworzyć całą nić `vector_home`
  (`dispatcher.rs:378`, `:533`, `node_adapter.rs:186` + `IngestRequest` + `stub_ctx`).
- **`dispatch/run_events.rs` to pułapka nazwy** — to strumień postępu agentów, nie `events.db`.
  Handler przeglądarki musi trafić gdzie indziej.
- **Dziennik audytu NIE jest wzorcem filtra widoczności** — `audit_log_list` jest płaskim
  `#[policy(Admin)]`. Wzorzec to `agent_runs_list` (`handlers.rs:8061-8083`): filtr wchodzi
  **do SQL**, a `agent_run_detail` (`:8112-8140`) zwraca `not_found`, nie `PolicyDenied`,
  żeby nie wyciekło istnienie cudzego przebiegu.
- **Pasek zakładek Code Studio jest w `code-studio-session.js`**, nie `code-studio.js`
  (`DOCK_CATEGORIES:40-47`, `VIEW_MAP:58-67`, `mountExternalPanes:724-756`).
- **`CodeStudioPayload` ma 141 wariantów i DWIE plomby** — nazw wariantów (`:1769-1789`)
  i nazw pól struktur wire (`:1792-1871`). Dodanie wariantu rusza obie.
- **`tf-run-timeline` nie ma ANI JEDNEGO produkcyjnego konsumenta** — tylko fixture.

### Sprzeczność w regule protokołu — do rozstrzygnięcia, nie zgadywana

`CLAUDE.md:334` mówi „ciborium tags by variant NAME", a komentarze w pięciu plikach
(`camera.rs:6`, `legal.rs:7`, `vision.rs:6`, `pii.rs:5`, `profiling.rs:891`) mówią o twardym
limicie **256 wariantów w `MessageBody`** i przypisują go „CBOR 0.8". Obie rzeczy nie mogą
być prawdą naraz, a dwie z nich są sprawdzalne:

- `MessageBody` ma **312 wariantów** i działa w produkcji — czyli limitu 256 nie ma.
- Zależność to **ciborium 0.2.2**, nie 0.8. Wersji 0.8 ciborium nie ma.

Plomba `code_studio.rs:1781` mówi wprost „ciborium tags variants by name, so a rename silently
breaks every deployed browser" — czyli krytyczne jest **nieprzemianowywanie**, a nie kolejność.
Komentarze indeksowe sugerują odwrotnie (kolejność krytyczna, nazwa obojętna) i to jest rada
niebezpieczna. **Nie poprawiam pięciu plików na podstawie rozumowania** — to wymaga dowodu
empirycznego z kodowania, a ten należy do toru protokołowego.

---

## Runda: zapis pochodzenia do `flow_executions` — ślepy krytyk (2026-08-21)

**Werdykt: DOES NOT MEET BAR.** Czternaście mutacji, **jedenaście złapanych, cztery żywe**.
P1, P2, P3, P6, P7 spełnione i realnie pilnowane. **P4 i P5 niespełnione.**

Co ważne: w każdym z czterech przypadków **kod produkcyjny jest poprawny** — brakuje testu,
który zauważyłby, gdyby przestał być. To luka pokrycia, nie usterka zachowania.

| Mutacja | Miejsce | Skutek |
|---|---|---|
| **M7** | `run_manager.rs:667`, `handle_agent_spawn` bierze stałą zamiast `&caller.principal` | **64 testy agentów zielone.** Sonda `eprintln!` dowiodła, że DZIEWIĘĆ testów wykonuje zmutowaną linię i żaden nie asertuje na jej wyniku |
| **M8/M8b** | `run_manager.rs:1620`, autokontynuacja pod stałym principalem albo wyprowadzonym z `user_id` | zielone. `user_id` to **dokładnie ten błąd, przed którym ostrzega komentarz obok** — przeżywa, bo fikstura testu to `AgentPrincipal::user("u1")`, gdzie błąd i poprawność dają tę samą wartość |
| **M11** | skasowanie ramienia `"meeting" => FlowOrigin::Meeting` z `parse` | zielone. Dwa testy „pilnujące" round-tripu iterują **listę pisaną ręcznie**, w której brakuje `Dashboard` i `Meeting`. `Meeting` jest mintowany produkcyjnie (`meeting/flow_turn.rs:244`) — czyli wariant dodany W TEJ SESJI nie ma pokrycia round-tripu |
| **M14** | `ContextFactory::make_context` daje `begin_run` inny `correlation_id` niż `ExecutionContext` | **1025 testów zielonych.** Dwa istniejące testy sprawdzają po jednej stronie, każdy wobec literału wpisanego we własnym ciele |

**Odpowiedź na pytanie 2 jest ROZSZCZEPIONA**, tak samo jak w R3. Strona flow: TAK — inwersja
sensu toru (pisarz utrwala stałą; stempel jednak z `envelope.meta`) czerwieni testy. Strona
sub-agenta: NIE — inwersja „dziecko dziedziczy stały `system` zamiast rodzica" przechodzi
niezauważona mimo dziewięciu testów wykonujących tę linię.

**Nauka z M11, ważniejsza niż sama mutacja:** test iterujący ręczną listę wariantów ma tę samą
wadę co brak testu, tylko odroczoną — nowy wariant po prostu nie trafia na listę. Poprawka musi
być **strukturalna** (wyczerpujący `match`, który nie kompiluje się po dodaniu wariantu), nie
dłuższa lista.

### Znaleziska poza kryteriami

1. **Flow syntetyczne NIE tworzą wiersza przebiegu w ogóle.** `create_execution_record` zwraca
   wartownika `0`, gdy `flow_id.is_empty()`, a tak właśnie powstają flow syntetyczne
   (`dispatcher.rs:831`, `:1070`). Czyli wywołanie `/v1` spadające na flow syntetyczne odpowiada
   na „skąd i kto" **brakiem wiersza**. Świadome (klucz obcy do `flows`) i okomentowane, ale
   istotnie kwalifikuje tezę „dla każdego przebiegu", a żaden test tego nie pinuje.
2. **Nowa zastana czerwień, nie z naszej listy:** `dispatch::stream::tests::lidar_local_robot_subscribe_streams_frame`
   pada na czystym HEAD (niezgodność bajtów 20–27 ładunku). Niezwiązane z tym torem.
3. **Rozjazd numeru migracji w prozie:** `repository.rs` i `principal.rs` mówią o „pre-v131",
   podczas gdy kolumny dodaje **v134**. Kosmetyczne, ale wysyła czytelnika do złej migracji.

Zlecona poprawka (świeży wykonawca, nie autor, nie krytyk) obejmuje S1–S4, prozę i test
pinujący wartownika. Nowa krytyka po poprawce musi iść do **kolejnego** świeżego krytyka.

---

## INCYDENT 5 (mój błąd, nie agenta): blokada mutacji użyta jako blokada buildu

Kazałem obu wykonawcom owijać **każde** wywołanie `cargo` w `with-mutation-lock.sh`. Blokada
istnieje po to, żeby jeden agent nie kompilował się na ZMUTOWANYM drzewie drugiego — a ja
zrobiłem z niej globalną blokadę buildu. Skutek: pełny `cargo test --lib` jednego agenta
(43 min, 1225% CPU, 25 GB RSS — pracował, nie wisiał) zagłodził drugiego na 40 minut, a ten
oddał **kod niezweryfikowany**: 12 zmienionych plików i 1 nowy, zero kompilacji, zero testów,
zero dowodów mutacyjnych.

Agent zachował się poprawnie — trzymał się reguły 3 zamiast ją obejść, i powiedział wprost
„traktuj kod jako niezwalidowany". Wina jest po stronie zlecenia.

**Reguła poprawiona:**
- blokada TYLKO wokół okna mutacji: nałóż → jeden odfiltrowany test → przywróć → odczytaj kod
  z powrotem. Zwolnij między oknami.
- zwykły `cargo check` / `cargo test` na niezmutowanym drzewie: **bez blokady**. Własna blokada
  katalogu `target` w cargo i tak serializuje buildy bezpiecznie.
- **nigdy nieodfiltrowany `cargo test --lib`** — ~45 min na tej maszynie i wiesza się na
  `services::storage_proxy::server::tests::central_kv_proxy_writes_and_reads_on_authority`.

Wniosek ogólny, ten sam co przy INCYDENCIE 4: mechanizm, który wymusza dyscyplinę, musi być
wąsko zakresowany. Zbyt szeroki działa jak awaria.

### Znalezisko R2b, które zmienia projekt węzła

Addon RAG **już buduje własny graf** przez host-funkcje, z własnym rejestrem sprzątającym
(`graph_artifacts`), i domyślnie ustawia `graph_enabled: true` (`addons/rag/src/lib.rs:807-817`).
Wpięcie `graph_extract` we WSPÓLNY flow ingestu podwójnie zapisywałoby graf dla ścieżki addona,
a przy moim pierwotnym rozstrzygnięciu („jawnie włączony graf na buildzie bez `feature = graph`
= głośny błąd") **hard-failowałoby każdy ingest RAG na domyślnym buildzie**.

Wykonawca rozwiązał to przypinając `"graph_enabled": false` w configu zasianego węzła i dając
configowi pierwszeństwo nad meta. To usuwa błąd, ale czyni funkcję **martwą** — a kryterium
brzmi „wpięty w platformowy flow ingestu **sterowany `graph_enabled`**".

**Rozstrzygnięcie właściciela: sygnałem opt-in jest OBECNOŚĆ `graph_home`.** Węzeł ekstrahuje
tylko gdy `ctx.graph_home.is_some()` ∧ graf nie jest jawnie wyłączony ∧ feature skompilowany.
Ścieżka addona nie niesie `graph_home` (pisze do drzewa addona przez host-fn), więc węzeł
degraduje się tam do no-opu **z powodu widocznego w kodzie**, a nie przez zamrożoną flagę.
Project Studio `graph_home` ustawia — i to ta ścieżka dostaje ekstrakcję, domyślnie wyłączoną
zgodnie z decyzją człowieka. Głośna odmowa zostaje, ale wyzwala ją wyłącznie **własny config
węzła**, nigdy odziedziczone `meta` domyślnie ustawione na ON.

### Skutek uboczny INCYDENTU 5: mutacja żyła ~10 minut w cudzym oknie buildu

Zagłodzony wykonawca R2b uruchomił okno mutacyjne ręcznie; `flock` odpadł na timeoucie
**po nałożeniu mutacji**, więc zmutowany plik został w drzewie przez ok. 10 minut, podczas gdy
drugi agent się budował. R2b przywrócił plik i zweryfikował odczytem kodu, a okno przerobił na
jeden atomowy skrypt, który **najpierw bierze blokadę, dopiero potem mutuje**, i przywraca
w `finally` przed zwolnieniem.

Mutacja była w plikach spoza własności drugiego agenta, więc nie mogła zmienić zachowania jego
kodu — ale mogła wywrócić build albo test integracyjny przechodzący przez flow. Dlatego
**wyniki zebrane przez agenta `events` w tym oknie są prowizoryczne**; kazałem powtórzyć pełną
weryfikację na czystym drzewie przed raportem.

Kolejność w skrypcie okna (blokada → mutacja → test → przywrócenie → zwolnienie) jest właściwym
kształtem tego mechanizmu i powinna być domyślna w każdym następnym zleceniu. Wariant „nałóż
mutację, potem czekaj na blokadę" jest z definicji niebezpieczny.

---

## INCYDENT 6: weryfikacja mutacji była PUSTA — `cmp` porównywał dwa równie zmutowane pliki

Najpoważniejszy incydent tej sesji, bo trafił dokładnie w mechanizm, który miał chronić przed
poprzednimi trzema.

Harness okna mutacyjnego R2b robił kopię zapasową **per MUTACJA**, nie **per PLIK**:

```python
for m in spec["mutations"]:
    backups[path] = path + ".gebak"
    shutil.copy2(path, backups[path])   # <- wykonuje sie ZNOWU dla tego samego pliku
```

Przy kilku mutacjach w jednym pliku druga kopia łapała drzewo **z już nałożoną pierwszą
mutacją**. Przywrócenie wpisywało z powrotem plik nadal zmutowany, a kontrola `cmp` porównywała
dwa równie zmutowane pliki — i **przechodziła**. Każde okno raportowało „RESTORED (byte-identical)".

**Wyciekło 9 mutacji** (7 w `graph_extract.rs`, 1 w `collection.rs` — dopasowanie provenance do
DOWOLNEGO `doc_id`, czyli błąd międzydokumentowej utraty danych; +1 z ubitego okna d1).
Nic nie zostało zacommitowane. Agent zauważył dopiero dlatego, że G4 **nie skompilowało się**,
a on przeczytał źródło, żeby zdiagnozować — i zobaczył mutację w kodzie właśnie ogłoszonym
jako przywrócony.

### Wniosek, który wchodzi do reguł na stałe

**`cmp` z kopią zapasową NIE JEST weryfikacją przywrócenia — nie wykrywa złej kopii.**
Reguła §2.2 mówiła „przywróć z kopii i zweryfikuj CZYTAJĄC KOD Z POWROTEM". Odczyt kodu jest
niezastąpiony właśnie dlatego, że jest niezależny od kopii. Harness zastąpił go `cmp`, czyli
kontrolą zależną od tego samego zepsutego artefaktu, i zrobił z weryfikacji rytuał.

**Nowy wymóg w każdym zleceniu:** po przywróceniu sprawdź, że **oryginalna kotwica każdej
mutacji jest obecna w pliku** (asercja na tekst źródłowy), a nie że plik równa się kopii.
Kontrola musi być niezależna od artefaktu, któremu nie ufamy.

Dodatkowo: kopia zapasowa **raz na plik**, nigdy raz na mutację.

### Status dowodów R2b

- **G1 i G2 STOJĄ** — okna jednomutacyjne, więc błąd kopii ich nie dotyczył; miejsca
  ponownie odczytane. G1: obie połowy kryterium (licznik `0 → 1` oraz panika `StubLlm`).
  G2: bramka `graph_home` udowodniona licznikiem.
- **G3 ODRZUCONE i do powtórki** — czerwienie były prawdziwe, ale powstały pod harnessem
  kumulującym mutacje w rundzie, więc atrybucja jest niewiarygodna. Sam fakt „test spadł"
  nic nie mówi, jeśli nie wiadomo, KTÓRA zmiana go spowodowała.
- G4 (naprawione), D1, D2, S1 lecą pod poprawionym harnessem z kontrolą kotwic.

Agent zgłosił to sam, zanim ktokolwiek zapytał, podał dokładną przyczynę źródłową i sam
unieważnił własne wcześniejsze dowody. To jest zachowanie, którego ta pętla wymaga —
raport bez tej sekcji byłby wart mniej niż nic.

---

## Decyzja: NIE podłączam `delete_file_graph` — podłączenie uzbroiłoby utratę danych

Planowałem sam dopiąć pięć wywołań `delete_file_graph` / `drop_project_graph` w
`dispatch/project_studio.rs`, obok istniejących `delete_file_vectors`. **Wycofuję to.**

Wykonawca R2b zgłosił w raporcie ograniczenie, którego wcześniej nie znałem: `provenance_json`
jest **jednowartościowe**. Encja zapisana przez dwa dokumenty pamięta tylko OSTATNIEGO pisarza,
więc skasowanie tego dokumentu kładzie tombstone na węzeł, którego drugi dokument nadal używa.
Test `per_document_provenance_makes_one_document_deletable` pilnuje przypadku rozłącznego
(`(2,1)` zamiast `(4,2)` pod poluzowaną mutacją) — ale nie przypadku encji współdzielonej.

Addon RAG rozwiązał to inaczej: **licznikiem referencji** w tabeli `graph_artifacts`
(`addons/rag/src/lib.rs:1101-1128`) — węzeł znika dopiero, gdy żaden inny `document_id` go nie
trzyma. Nasza ścieżka core takiego rejestru nie ma, a dodanie go to **zmiana schematu**, czyli
bramka człowieka.

Dopóki funkcje są **nieużywane**, ograniczenie jest udokumentowanym długiem. W chwili
podłączenia pięciu wywołań staje się aktywną ścieżką cichej utraty danych w Projektach.
Dlatego zostają nieużywane, świadomie, i jest to zapisane tutaj oraz w kodzie — a nie
„zapomniane do dokończenia".

To jest też odpowiedź na regułę 5 CLAUDE.md („kasuj nieużywany kod"): funkcje NIE są
pozostałością po usuniętym wołającym, tylko połową mechanizmu, którego drugiej połowy nie wolno
zbudować bez zgody na migrację. Skasowanie ich byłoby utratą przetestowanej pracy, a podłączenie
— uzbrojeniem błędu.

---

## Runda: przeglądarka zdarzeń (backend) — ślepy krytyk (2026-08-21)

**Werdykt: DOES NOT MEET BAR.** E1, E2, E6, E7, E8 spełnione i realnie pilnowane.
E3 częściowo, **E4 wcale**, E5 częściowo. Kod produkcyjny wygląda poprawnie w każdym z tych
trzech przypadków — brakuje testu, który zauważyłby, gdyby przestał.

**Odpowiedź na pytanie 2 znowu jest ROZSZCZEPIONA, i tym razem rozszczepienie jest groźne:**

- **Ograniczenie DZIAŁA i jest pilnowane.** Usunięcie warunku `actor_user_id = ?` czerwieni dwa
  testy, na obu warstwach. Ten zestaw NIE przeżywa inwersji własnego sensu.
- **Ale nikt nie pilnuje ŹRÓDŁA TOŻSAMOŚCI.** Mutacja `Scope::OwnRuns(_) if req.actor_id.is_some() => None`
  — czyli „ładunek żądania unieważnia ograniczenie" — daje **116 zielonych**. Zmiana pozwalająca
  DOWOLNEMU wołającemu przeczytać cudze wiersze przez podanie `actor_id` jest dla tego zestawu
  niewidzialna.

To dokładnie inwariant §6.1 briefu („wartości pochodzenia NIGDY nie pochodzą od wołającego") —
pilnowany przez konstrukcję typów, ale nie przez ani jeden test.

| Luka | Mutacja | Wynik |
|---|---|---|
| **E3** — grant tylko `read_all` | odwrócenie kolejności w `resolve_scope`, tak że `events.read` jest wymagane pierwsze | 116 zielonych. Każdy test `read_all` nadaje OBA uprawnienia, więc kolejność jest niepilnowana |
| **E4** — poszerzenie własnego zakresu | `actor_id` z ładunku kasuje ograniczenie | 116 zielonych |
| **E5** — redakcja na warstwie wire | `to_wire` wskrzesza pominiętą treść | żaden test nie padł. Istniejący test czerwieni się od mutacji PISARZA, czyli dubluje test ze `store.rs`, a nie pilnuje ścieżki odczytu |

**Test przechodzący z niewłaściwego powodu:** `events_browse_roundtrip` zostaje zielony przy
`#[serde(skip)]` na `EventRowWire.actor_user_id` — polu, na którym stoi cała historia ACL —
bo fikstura zostawia je `None`. Każde inne pole tej fikstury jest wypełnione i by się zaczerwieniło.

### Znalezisko poza kryteriami, potencjalnie ŻYWA dziura, nie luka w testach

**Dwa różne modele własności.** `browse` ogranicza per WIERSZ (`actor_user_id = ?`), a odczyt
pojedynczego przebiegu rozstrzyga per PRZEBIEG, z wiersza o najniższym `seq`
(`store.rs:794`, `ORDER BY seq LIMIT 1`). Jeżeli jeden przebieg może nieść wiersze ostemplowane
DWOMA różnymi `actor_user_id`, to właściciel pierwszego wiersza czyta całą oś czasu — łącznie
z wierszami drugiego principala — podczas gdy `browse` pokazałby każdemu tylko jego własne.

Test `events::progress_log::tests::rebinding_a_scope_moves_later_events_onto_the_new_run`
sugeruje, że przepinanie zakresu WEWNĄTRZ przebiegu jest realną operacją. **Zlecone do zbadania
z jawnym zakazem przeprojektowywania ACL z własnej inicjatywy** — jeśli to żywy defekt, chcę
dowód i opis, a nie cichą przebudowę modelu uprawnień.

Drobniejsze: komentarz w `tentaflow-protocol/src/events.rs` twierdzi, że wiersz niesie
`payload_json` **„VERBATIM"** — nieprawda, `to_wire` re-serializuje zdekodowaną wartość, więc
wiersz zapisany przez nowszy build gubi nieznane pole znanego rodzaju. Komentarz mijający się
z prawdą jest gorszy niż jego brak.

### Poprawka luk E3/E4/E5 + rozstrzygnięcie dwóch modeli własności

Wszystkie trzy luki zamknięte testami z dowodem mutacyjnym; **żadne zachowanie produkcyjne nie
zostało zmienione** — w każdym przypadku kod był poprawny, brakowało wyłącznie strażnika.

Najważniejszy dowód, E4: przy mutacji „`actor_id` z ładunku kasuje ograniczenie" czerwień
wypisuje **sam wyciek** — strona marka zawierająca przebieg anny. Test wysyła kolejno cudzy
`actor_id`, `search` trafiający wyłącznie w cudzy `run_id`, i wszystkie filtry naraz wycelowane
w cudze dane; na koniec marek podaje WŁASNY `actor_id` i dostaje swój wiersz — żeby odmowy były
dowodem działania ACL, a nie filtra, który po prostu nic nie trafia.

**Dwa modele własności: NIE ma dziury.** Rozstrzygnięcie oparte na sprawdzalnym inwariancie,
nie na intuicji:

1. `run_events` ma **dokładnie jednego** pisarza produkcyjnego (`progress_log::write_batch`);
   żadne miejsce w drzewie nie robi UPDATE na `actor_user_id`.
2. Każdy wiersz jest stemplowany z `RunProvenance` zakresu, nigdy wyprowadzany ze zdarzenia.
3. `bind_run_provenance` dopuszcza przepięcie **tylko przy `same_principal`**; inaczej slot
   przechodzi w `ScopeProvenance::Contested`, `run_provenance()` zwraca `None`, a subskrybent
   przestaje cokolwiek zapisywać. Przepinanie zakresu jest realne, ale zmienia `run_id`,
   nigdy właściciela.

Test `a_second_principal_cannot_add_rows_to_a_live_run` pinuje to w PISARZU (tam taki przebieg
by się urodził), z kontrolą pozytywną, żeby „brak nowych wierszy" nie mógł wynikać z martwego
subskrybenta, i z asercją międzymodelową: `run_actor_user_id` (odpowiedź z najniższego `seq`)
równa się `actor_user_id` KAŻDEGO wiersza.

### Dwie miny na przyszłość — nie do naprawy dziś, do zapamiętania

- **`mesh/inference_proxy.rs` przekazuje `request_id` OD SIDECARA** do `meeting::flow_turn::run_flow_turn`
  → `FlowRequestMeta::new`. To `run_id` **podany przez klienta**. Dziś nie dociera do logu
  zdarzeń, bo `flow_turn` nie ustawia `progress_sink`, a `begin_run` wychodzi wcześniej.
  **Gdyby tura spotkania kiedykolwiek dostała progress sink, dwa boty mogłyby złożyć tury pod
  jednym `run_id` z różnymi `owner_user_id` — i to byłby żywy odczyt międzyprincipalowy.**
  To jest DOKŁADNIE ta jedna zmiana, która łamie inwariant powyżej.
- `services/camera_ingest/vision_analysis.rs::camera_flow_request_meta` buduje
  `request_id = format!("cam-{camera_id}")` — `run_id` nieunikalny per przebieg. Nieszkodliwe
  (aktor to zawsze `system_component`, więc `actor_user_id` jest NULL, i ścieżka kamery nie
  ustawia progress sinka), ale to jedyne miejsce, gdzie założenie „run id to świeży uuid"
  nie trzyma się z konstrukcji.

---

## Runda: T4 metryki jako zapytania — ślepy krytyk (2026-08-21)

**Werdykt: MEETS BAR.** M1–M6 spełnione, 13 mutacji, każda w osobnym oknie, każda z kontrolą
kotwicy.

**Najważniejszy wynik tej rundy dotyczy METODY, nie kodu.** Kazałem krytykowi polować na
asercje na PRZEDZIAŁ zamiast na WARTOŚĆ. Znalazł dokładnie taki przypadek i to nie teoretyczny:

> Mutacja 5 podmieniła wzór dekodowania na `request_started → assistant_message`. Wynik ~300 ms
> **zmieścił się w oknie `(259..380)`** i test przedziałowy **przeszedł**. Złapała to wyłącznie
> fikstura o ustalonych znacznikach czasu, asertująca `ttft_ms == Some(150)` i
> `decode_ms == Some(490)` — wartości dokładne, bez tolerancji.

Odwrotnie: mutacje 8 i 12 (stała równa oczekiwaniu fikstury) przechodzą fiksturę i łapie je
tylko test przedziałowy. **Oba style są komplementarne i żaden nie jest zbędny** — to jest
wniosek do przeniesienia na inne tory, bo tolerancja szersza niż różnica między dobrym a złym
wzorem nie jest testem, tylko dekoracją.

Drugi wynik metodyczny: mutacja 7 (start TTFT z pierwszego `step_start` zamiast
`request_started`) zostaje zielona, ale to **mutant równoważny, nie dziura** — `progress_log::translate`
wypycha oba wiersze z jednego `NodeStarted` z tą samą zmienną `at_ms`, więc dziś są równe
z konstrukcji. Krytyk to rozróżnił zamiast policzyć jako lukę.

### Znaleziska poza kryteriami

1. **`events::metrics` NIE MA ANI JEDNEGO WOŁAJĄCEGO.** Ani handlera, ani UI, ani innego modułu —
   `step_latencies`, `tool_durations`, `StepLatency`, `ToolDuration` są `pub` i martwe poza
   własnymi testami. Derywacja jest poprawna i nieskonsumowana.
   **To jest realny problem architektoniczny, nie kosmetyka:** przeglądarka zdarzeń wyprowadza
   pasma **po stronie JS**, więc mamy DWIE derywacje tych samych wielkości — jedną w Ruście
   (nieużywaną) i jedną w JS (używaną). Reguła 4 CLAUDE.md („sprawdź, czy funkcja już istnieje,
   zanim napiszesz nową") jest tu złamana w skali modułu. Rozstrzygnięcie, która strona ma
   liczyć, jest decyzją projektową — odnotowuję, nie rozstrzygam sam.
2. **Etykieta kroku niepowiązana z węzłem** — mutacja 10 (`s.node_id = s.node_id`) zielona.
   `StepLatency.step` mógłby pochodzić z `step_start` INNEGO węzła i żadna fikstura by nie
   zauważyła. To niepilnowana połowa kryterium M3 („w tym samym kroku").
3. **Żaden test nie asertuje przypadków `None`.** Dokumentacja twierdzi, że `ttft_ms`/`decode_ms`
   są `None`, nigdy zerem, gdy brakuje `request_started` albo wiadomość się nie skończyła.
   Nigdzie nie ma asercji na `None` — a to ta sama własność „uczciwości", którą M2 dostało.

---

## Runda: R1 trwała kolejka `jobs.db` — ślepy krytyk (2026-08-21)

**Werdykt: DOES NOT MEET BAR.** R1.1–R1.4 spełnione, **R1.5 i R1.6 nie**.
Tor był w briefie oznaczony jako „gotowy, runda 2" — nie był.

**Odpowiedź na pytanie 2 znowu rozszczepiona:**
- „przeżywa restart i jest dokańczane" — **PILNOWANE**. Rozbicie `claim` na `SELECT`+`UPDATE`
  w dwóch osobnych uchwytach pisarza (czyli to, co widzi drugi PROCES) czerwieni test
  komunikatem „a job was claimed twice — left: 72, right: 64". Test wymusza REALNĄ rywalizację
  (64 zadania, dwa wątki kręcące się do wyczerpania), a trwałość jest sprawdzana na PRAWDZIWYM
  pliku (mutacja „otwórz bazę w pamięci" czerwieni dokładnie jeden test).
- „jest poprawnie zamykane, gdy NIE MOŻE być dokończone" — **NIEPILNOWANE**. Zapisanie sieroty
  jako `"success"` z pustym błędem: **19 zielonych**. `continue` na pierwszej linii pętli, czyli
  reconcile nie robi NIC: **wszystkie realne testy zielone**.

**R1.5 NIE DO NAPRAWY BEZ BRAMKI.** Addon i Projekty mają **dwie niezależne implementacje
kolejki**: inny magazyn (`<data>/jobs.db` vs tabela w SQLite addona), inny `claim`
(`UPDATE … RETURNING` vs `SELECT` + warunkowy `UPDATE` + `rows_affected`), inna reguła sierot
(`owner_instance <> instance_id()` vs czysta heurystyka czasowa `updated_at < now - 2400s`),
inny sterownik, inny stan terminalny (DELETE vs pozostawienie wiersza). Wspólna jest wyłącznie
**bramka współbieżności** `ingest_gate::acquire()` — to jeden punkt zwężenia, ale nie kolejka
i nie czyni ingestu addona trwałym. Ujednolicenie = zmiana migracji `ingest_jobs` addona =
**zmiana bundle hash** = bramka człowieka. **Zatrzymane na bramce, do decyzji.**

**R1.6:** stałe są uporządkowane poprawnie i każda strona ma komentarz nazywający drugą, ale
zmiana `max_runtime_seconds` z 1800 na 3000 (czyli DŁUŻEJ niż okno reclaimu 2400 s — legalnie
działający drain zostaje odebrany i jego artefakty sprzątnięte w locie) zostawia **9 zielonych**.
Dwa komentarze w dwóch crate'ach to nie jest asercja.

### Znaleziska poza kryteriami — w tym DWA prawdziwe błędy produkcyjne

1. **Źródło zostaje NA ZAWSZE w stanie `indexing` po awarii.** `reconcile_orphans` najpierw
   kasuje osierocony wiersz kolejki, potem `project_db::open` odpala hook, który widzi, że
   zadania już nie ma w kolejce, i zapisuje `failed`. Po powrocie strzeżony `finish_ingest_job`
   zwraca **false**, więc `set_source_status(…, "error", …)` w tym samym strażniku jest
   **pomijany**, a `recover_orphaned_jobs` źródła nie dotyka. Zadanie kończy jako `failed`,
   źródło zostaje `indexing` **na stałe**. To jest ten sam martwy kod, który mutacja M9
   pokazała jako niepilnowany — jest martwy także w produkcji.
2. **Kolejka NIE JEST FIFO poniżej milisekundy.** `enqueued_at_ms` ma rozdzielczość
   milisekundy, a `claim` i `jobs_ahead` rozstrzygają remisy przez `ORDER BY enqueued_at_ms, job_id`,
   gdzie `job_id` to **losowy UUID**. Zadania z tej samej milisekundy są obsługiwane w losowej
   kolejności, a `jobs_ahead` zaniża głębokość kolejki. Stąd test
   `a_job_queued_behind_others_reports_the_wait` jest **~50% flaky** (zmierzone: 16/30 przebiegów
   izolowanych). Flaky test w drzewie jest gorszy niż brak testu.
3. `announce_queue_wait` połyka błędy kolejki: `jobs_ahead(...).unwrap_or_default()` zamienia
   nieudany odczyt w `ahead = 0`, czyli „brak kolejki".
4. **`services/ingest_gate.rs` nie ma ANI JEDNEGO testu** — a jest jednym z pięciu plików toru.
5. Kolejka nie ma puli odczytu (`Db::from_connection`), więc każdy `is_pending`/`jobs_ahead`
   serializuje się z każdym `claim` — wbrew temu, co `db/mod.rs:112-130` opisuje jako właściwy
   kształt bazy pomocniczej.

---

## Runda: T2 `events.db` — ślepy krytyk (2026-08-22)

**Werdykt: MEETS BAR.** T2.1–T2.10 spełnione, każde z mutacją. Baseline i stan końcowy:
`cargo test --lib events::` → **83 passed, 0 failed**.

**Krytyk poprawił SAMĄ POPRZECZKĘ, i miał rację.** Kryterium T2.1 mówi „4 indeksy"; artefakt ma
pięć nieunikalnych plus częściowy unikalny — i to jest **dosłownie** zgodne ze specyfikacją
§2.3 (linie 210–216). Nieaktualna była poprzeczka, nie schemat. Sprawdził wobec ŹRÓDŁA PRAWDY,
a nie wobec zdania, które dostał ode mnie.

Najmocniejszy dowód, T2.5 (redakcja PRZED zapisem): mutacja przeniosła `.redacted()` z zapisu
do odczytu. Test padł, **wypisując surowy token `ghp_…`, hasło w URL-u i JWT prosto z kolumny**.
Asercja na strukturze w pamięci przeszłaby pod tą mutacją — ta nie przeszła, bo czyta dysk.

T2.3 udowodnione jako realna rywalizacja: dwa wątki na **dwóch niezależnych połączeniach**
(nie jeden `DbPool` za muteksem), zwolnione barierą. Mutacja `Immediate → Deferred` dała
**28 odmów „database is locked"**, więc kontencja jest prawdziwa, a nie pozorna.

### Test przechodzący z niewłaściwego powodu — złapany i poprawnie zaklasyfikowany

`a_service_key_is_stored_as_a_null_binding_not_as_an_empty_one` asertuje `actor_user_id IS NULL`
— czyli dokładnie to, co zwróci kolumna, która **nigdy nie jest zapisywana**. Krytyk zmutował
insert na bezwarunkowe `None::<String>`: ten test został zielony, ale **pięć innych** padło.
Kolumna jest więc pilnowana — tylko nie przez test, którego nazwa to obiecuje.

### Znalezisko poza kryteriami — PRAWDZIWY DEFEKT, potwierdzony przeze mnie osobno

**`RequestStarted` nie niesie żadnego pochodzenia na ścieżce produkcyjnej.**
`progress_log.rs:331-336` zapisuje ZAWSZE `model: None, flow_id: None, service_type: None,
modality: None`. Kopia audytowa ma odpowiadać na „który aktor, z jakiego pochodzenia,
**przeciwko któremu modelowi**" — i model jest tam zawsze pusty. Testy magazynu wypełniają te
pola ręcznie, więc nic tego nie zauważa. Sprawdziłem kod sam: tak jest.

Pozostałe, odnotowane bez naprawy: doc `retention.rs` mówi „ograniczony `DELETE`", a `LIMIT`
nie ma; liczba 30 dni to łańcuch tautologii (trzy testy porównują ją z tą samą stałą, którą by
za sobą pociągnęły); test schematu sprawdza kolumny tylko dwóch z sześciu indeksów; brak
dzierżawy na wierszach outboxu (dziś jeden wołający, więc utajone); „exactly once" jest w
rzeczywistości at-least-once i kod to przyznaje.

---

## Runda: R2b `graph_extract` — ślepy krytyk (2026-08-22)

**Werdykt: DOES NOT MEET BAR.** Spełnione: R2.1, R2.2, R2.3, R2.4, R2.7, R2.10.
**Niespełnione: R2.5, R2.6, R2.8, R2.9.** Osiemnaście mutacji, siedemnaście zabitych.

**Jednozdaniowe podsumowanie, które trzeba przeczytać w całości:**
**ścieżka ekstrakcji jest kompletna, ale NIEOSIĄGALNA, NIECZYTANA i NIESPRZĄTANA.**

- **R2.9 — czat projektu nadal wymusza `graph_enabled=false`** (`dispatch/stream_handlers.rs:2059-2062`),
  z komentarzem „Projekt nie ma kolekcji grafowej", który strona ingestu właśnie uczyniła
  nieprawdziwym. Tura dispatchuje `RAG_QUERY_FLOW_ID` z `addon_id = ps-<project_id>` — dokładnie
  ten zakres, do którego ekstrakcja pisze — a węzły retrievalu zwierają się na tej fladze.
  **Graf projektu, raz zbudowany, jest z czatu projektu nie do odczytania.**
- **R2.6 — projektu NIE DA SIĘ włączyć.** `PROJECT_GRAPH_EXTRACTION_DEFAULT` to `const`
  bez ustawienia, bez pola protokołu, bez UI. Nie istnieje test integracyjny i **nie da się go
  dziś napisać jako przechodzącego**, bo żaden projekt nie może włączyć grafu.
- **R2.8 — brak refcountu, potwierdzony sondą.** Dwa dokumenty nazywające „ETH Zurich",
  `delete_document_in(file-2)` → **`swept 2 nodes / 1 edges`**, `eth_zurich` otombstonowany mimo
  że file-1 nadal go używa. Istniejący test używa **rozłącznych** zbiorów encji, więc przypadku
  współdzielonego nigdy nie dotyka. Dodatkowo `delete_file_graph`/`drop_project_graph` mają
  **zero wołających**, więc graf przeżywa i skasowanie pliku, i skasowanie projektu.
- **R2.5 — brak `chunk_id` i wersji ekstraktora.** `chunk_id` jest **strukturalnie nieosiągalny**:
  `batches()` skleja teksty chunków i porzuca `index`. Mutacja **G6** (skasowanie `source_id`
  i `path` z provenance) **przeżyła** — 16 testów zielonych.

### Ile z tego jest moją winą jako orkiestratora

Uczciwie: **większość**.

- **R2.9 jest moim błędem zakresu.** Wyciąłem `dispatch/**` z własności R2b (bo pracował tam
  agent przeglądarki zdarzeń), a to jest jedyne miejsce, gdzie ta flaga jest przypinana.
  Kryterium było w poprzeczce od początku; ja go nie zleciłem.
- **R2.6 jest moim przeoczeniem.** Kazałem Projektom domyślnie WYŁĄCZYĆ graf (zgodnie z decyzją
  właściciela) i **nigdy nie zleciłem drugiej połowy — sposobu na włączenie**. Domyślnie
  wyłączone bez możliwości włączenia to nie jest „domyślnie wyłączone", to jest martwe.
- **R2.8(c) jest moją świadomą decyzją**, zapisaną wyżej: nie podłączam `delete_file_graph`,
  bo przy jednowartościowym provenance podłączenie uzbroiłoby cichą utratę danych. Krytyk
  niezależnie potwierdził sondą, że ta obawa była słuszna — i że addon RAG robi to poprawnie
  refcountem, więc poprzeczka nie jest hipotetyczna.
- **R2.5 jest realnym ograniczeniem projektu węzła**, nie przeoczeniem zakresu — wsadowanie
  chunków dla oszczędności wywołań LLM wyklucza atrybucję per chunk. To jest kompromis do
  rozstrzygnięcia, nie błąd do naprawienia w locie.

### Co jeszcze krytyk znalazł poza kryteriami

- `pick_model` spada na platformowy alias `rag-llm`, a nie na model czatu projektu — projekt,
  który włączyłby ekstrakcję, po cichu płaciłby za inny model. To ta sama klasa usterki, którą
  właściciel kazał usunąć z czatu projektu („zdjąć fallback, głośny błąd zamiast cichej odpowiedzi").
- `graph_extract` propaguje błąd, więc nieudana ekstrakcja **wywraca cały ingest pliku**,
  łącznie ze ścieżką wektorową.
- Zero testów na czapki `MAX_ITEMS_PER_BATCH` (128) i `MAX_NAME_CHARS` (120) — a to jest
  host-side guard przed wstrzykniętym mega-grafem.

**Sprzątnięte:** `tentaflow-core/relative/` (pozostałość po teście walidacji ścieżki) — ta sama
pozostałość co dwie sesje temu, usunięta ponownie. Warta wpisu do `.gitignore`.

---

## Domknięcie R2b: graf osiągalny, kasowalny i czytelny (2026-08-22)

Cztery niespełnione kryteria R2b zamknięte. **R2.8** — provenance jest teraz zbiorem `doc_ids`
scalanym przy upsercie i zmniejszanym przy kasowaniu; tombstone dopiero gdy zbiór pustoszeje,
a zmniejszony zbiór **zapisywany z powrotem** (bez tego wiersz przeżyłby swój ostatni dokument
na zawsze). **R2.9** — czat projektu nie przypina już `graph_enabled=false`. **R2.6** — ustawienie
per projekt, domyślnie wyłączone, za bramką Managera. **Sprzątanie przy re-ingeście** —
odpowiednik tego, co ścieżka wektorowa robiła od dawna. Bez zmiany schematu.

Weryfikacja orkiestratora: **233 zielone** (`--features graph`), **153** (domyślne), parzystość
i18n 1487 kluczy w pięciu lokalizacjach, zero surowych elementów HTML.

### Lekcja 1: mutacja wykryła błąd w TEŚCIE, nie w kodzie

Pierwsza wersja testu na współdzieloną encję kazała nowej wersji pliku ponownie nazwać
`eth_zurich`. Mutacja „tombstone zamiast zapisu zmniejszonego zbioru" **przeżyła**, bo kolejny
zapis po prostu wskrzeszał węzeł — asercja nie była nośna. Wykonawca przepisał test tak, żeby
nowa wersja nazywała inny podmiot, i dopiero wtedy mutacja padła. **Zgłosił to sam**, pisząc,
że przeżywający mutant jest jedynym powodem, dla którego ta asercja jest teraz prawdziwa.

To jest przypadek, dla którego warto robić dowody mutacyjne nawet gdy kod jest poprawny:
mutacja pilnuje nie tylko implementacji, ale i tego, czy test w ogóle coś mierzy.

### Lekcja 2 (ZASTANA PUŁAPKA): `services::graph::tests` wymaga `TMPDIR`

Moja niezależna weryfikacja pokazała **sześć czerwonych** testów kwot i współbieżności —
dokładnie tej ścieżki, którą refcount przerabiał. Wyglądało to jak regresja i wstrzymałem
drugiego agenta, żeby odróżnić regresję od skażenia.

Przyczyna: helper `tempdir()` (`services/graph/tests.rs:36`) przy nieustawionym `TMPDIR` spada
na **zaszytą ścieżkę z cudzej maszyny** `/mnt/e/repos/rust/_scratch/tf-graph-tests`. Na tym
pudle `/mnt/e` należy do roota (755), więc `create_dir_all` zwraca `PermissionDenied` i KAŻDY
test w module ginie w tej jednej linii, zanim dotknie logiki grafu.

**Zawsze eksportuj `TMPDIR` przy `services::graph::tests`** — i nie na `/tmp` (tmpfs 31G,
w tej sesji zapełniony do 100% przez pozostałości po testach; komentarz helpera wprost chce
prawdziwego dysku). Zielony przebieg tego modułu bez `TMPDIR` jest **niemożliwy**, więc to
sprawdzalna deklaracja — każę ją agentom podawać przy cytowanym wyniku.

Helper NIE został naprawiony: to zastana usterka przenośności, zasługująca na własne
uzasadnienie, a nie na doklejenie do commitu z funkcjonalnością.

### Czego ten push nie obejmuje

Nikt nie przeklikał przełącznika przez żywy serwer. Ścieżka `codec.js` → wasm →
`SettingsSaveRequest` kompiluje się, a zregenerowany `wasm_glue.js` niesie nowy parametr, ale
runtime'owo nie została wykonana. Zrzut ekranu dowodzi renderu markupu, komponentów i tekstów —
nie przejścia przez protokół binarny. Wykonawca napisał to wprost, zamiast sprzedać zrzut jako
dowód end-to-end.
