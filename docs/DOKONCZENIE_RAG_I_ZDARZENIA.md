# Dokończenie: RAG + śledzenie zdarzeń

Jeden plik do prowadzenia dalszych prac. Zawiera **stan faktyczny na dziś**, to co zostało z
unifikacji RAG, oraz **pełną specyfikację śledzenia zdarzeń** — na tyle dokładną, żeby nie trzeba
było niczego wyprowadzać od nowa.

Dokumenty źródłowe (kontekst, nie instrukcje): [`RAG_UNIFICATION_PLAN.md`](RAG_UNIFICATION_PLAN.md),
[`PLAN_SLEDZENIE_ZDARZEN.md`](PLAN_SLEDZENIE_ZDARZEN.md),
[`POROWNANIE_HARNESS_DEEPSEEK.md`](POROWNANIE_HARNESS_DEEPSEEK.md),
[`ANALIZA_DEEPSEEK_HARNESS.md`](ANALIZA_DEEPSEEK_HARNESS.md).
**W razie sprzeczności ten plik wygrywa** — tamte opisują stan z chwili pisania.

---

## 0. Czego NIE robić drugi raz

Zrobione i zweryfikowane testami. Wypisane, żeby nikt nie zaczął od nowa:

| Obszar | Stan |
|---|---|
| Aliasy `rag-*` | platformowe (seed rdzenia), właściciel `manual`, widoczność `public`, migracja starych instalacji + reaktywacja aliasu związanego przez admina |
| Flowy `ingest` / `query` / `retrieval-round` | platformowe, systemowe, stałe UUID; addon woła je po `core:rag-*` i nie wozi własnych kopii |
| Prymitywy wektorowe | `services/vector/doc_vectors.rs`: `ref_id_for`, `delete_doc_vectors` (wieloprzebiegowy), `search_namespace` |
| Ingest Projektów | jedzie wspólnym flow; skany i obrazy działają; kod i URL-e **zostają natywne** (świadoma decyzja 3c) |
| Anulowanie + limit współbieżności | `services/cancel_registry.rs` (3 kopie → 1) i `services/ingest_gate.rs` (limit w `execute_ingest`, liczy DOKUMENTY) |
| Czat projektu | jedzie **tym samym ciałem pętli** co addon (`retrieval-round`), zachowując streaming i model z `meta` |
| Pętla harnessu | równoległe narzędzia (allowlista), budżet czasu per narzędzie, ponowienie wewnątrz kroku, lepkie `max-tokens` |
| Agent | poziom rozumowania i temperatura, poziomy **z katalogu modelu** |

**Uwaga o nieaktualności:** §5 „Etap 5" w `RAG_UNIFICATION_PLAN.md` twierdzi, że przejście
`ps-chat` jest zablokowane. **To już nieprawda** — przeszło (`053386f19`), tylko inną drogą niż
zakładano: reużywamy CIAŁO pętli, a nie zewnętrzny flow `query`.

---

## 1. RAG — co zostało (W ZAKRESIE, nie odłożone)

Trzy pozycje. **Decyzja właściciela: wszystkie trzy wchodzą.** Projekty mają dostać graf wiedzy i
trwałą kolejkę, a graf ma być dostępny **przez bloczki flow**, a nie zaszyty w addonie.

Kolejność jest wymuszona zależnościami: 1.1 i 1.3 są tanie i niezależne, 1.2 dzieli się na część
tanią (G1) i duży projekt (G2).

### 1.1 Trwała kolejka ingestu — wspólna usługa core

**Stan:** addon ma `ingest_jobs` w swoim SQLite, drenowane przez Scheduler. Projekty mają zadania w
procesie (`start_job` → `tokio::spawn`), ginące przy restarcie. Wspólne są już limit współbieżności
(`services/ingest_gate.rs`) i rejestr anulowania (`services/cancel_registry.rs`).

**Do zrobienia:**
1. **Osobny plik `<data>/jobs.db`** — ten sam argument co przy `events.db` (§2.2): główna baza ma
   jedno połączenie pisarza. **Osobny od `events.db`**, bo log zdarzeń ma retencję i rotację, a
   kolejka nie — wspólny plik znaczyłby, że rotacja zabiera zadania.
2. Usługa `services/ingest_jobs.rs`: `enqueue`, `claim` (atomowe `UPDATE … RETURNING`, wzorzec z
   `project_studio` pool-claim), `heartbeat`, `finish`, `reconcile_orphans` przy starcie.
3. **Projekty**: `start_job` zapisuje zadanie i wraca; osobny worker drenuje. Zadanie przeżywa restart.
4. **Addon**: `ingest_drain` staje się cienkim wywołaniem host-fn do tej usługi zamiast własnego SQL.
5. **Migracja `ingest_jobs` addona**: przenieś wiersze `queued`/`running` do `jobs.db`, potem skasuj
   tabelę w migracji addona. **Bundle hash się zmieni** — przebudowany addon zostaje wyłączony do
   zatwierdzenia nowego hasha; to normalny tryb, nie awaria.

**Odbiór:** zadanie ingestu w Projektach przeżywa restart procesu i jest dokańczane; addon i Projekty
przechodzą przez tę samą kolejkę; osierocone zadanie po zabiciu procesu jest zamykane przy starcie.

### 1.2 MemGraphRAG dla Projektów — graf przez bloczki flow

**Stan:** ekstrakcja (`extract_chunk_graph`) żyje w **wasm addona** i jest wołana PO flow, czytając
teksty chunków z powrotem z `passages`. Flow ingestu nie ma ŻADNEGO węzła grafowego. Strona
retrievalowa jest już w core (`rag_graphrag.rs`, `graph_search.rs`) i wpięta w `retrieval-round`.

Stan SQL, na którym stoi MemGraphRAG (12 tabel w migracjach addona): `graph_artifacts`,
`graph_outbox`, `schema_registry`, `fact_schema`, `fact_evidence`, `fact_state`, `conflicts`,
`conflict_members`, `conflict_scan_cursor`, `entity_aliases`, `entity_merge_log`,
`entity_merge_scan_cursor`, `relation_cardinality`.

#### G1 — graf w Projektach i ekstrakcja jako węzeł flow **(rób najpierw)**

To daje 80% wartości przy ułamku kosztu, bo **infrastruktura już jest**: `GraphManager` kluczuje po
`(org, addon_id, collection)` dokładnie tak jak `NamespaceManager`, a Projekty już używają
`ps-<project_id>` jako pseudo-addon-id.

1. **`GraphManager` dostaje wariant z katalogiem** — dokładnie ten sam zabieg, który zrobiliśmy dla
   wektorów w Etapie 0a/0b: kolekcja NIEISTNIEJĄCA powstaje w katalogu wskazanym przez wołającego
   (`<project>/graph/`), a dla istniejącej wygrywa zapisana ścieżka. Przeciągnij `graph_home` przez
   `ExecutionContext` i `FlowRequestMeta` **jako osobne pole, nigdy przez `meta`** — ta sama zasada
   co przy `vector_home`.
2. **Nowy węzeł `graph_extract`** (`flow_engine/node_adapters/graph_extract.rs`): wejście = chunki,
   wywołuje `rag-llm` promptem ekstrakcji, upsertuje węzły i krawędzie do kolekcji `kg_active` w
   scope z `ctx.addon_id`. Zapisuje provenance (`chunk_id`, wersja ekstraktora) — bez tego kasowanie
   dokumentu nie umie cofnąć jego wkładu do grafu.
3. **Wpięcie w platformowy flow ingestu**, za `chunk`, sterowane `graph_enabled` w meta. Domyślnie
   **wyłączone** — ekstrakcja to dodatkowe wywołania LLM na każdy chunk i musi być świadomym wyborem.
4. **`ps-chat` przestaje wysyłać `graph_enabled=false`** i zaczyna korzystać z grafu, gdy projekt go ma.
5. **Kasowanie dokumentu kasuje jego wkład do grafu** — po `graph_artifacts` scope'owanym tak samo.

**Odbiór G1:** projekt z włączonym grafem po ingeście ma niepuste `kg_active` w SWOIM katalogu;
`retrieval-round` zwraca fakty grafowe; usunięcie źródła usuwa jego węzły i krawędzie; wyłączony graf
nie robi ANI JEDNEGO dodatkowego wywołania LLM.

#### G2 — warstwa utrzymania grafu jako usługa core **(duży projekt, osobno)**

Ontologia Candidate→Stable, agenci konfliktów A_det/A_res, entity merge. To jest to, co czyni
MemGraphRAG czymś więcej niż ekstraktorem trójek — i to jest praca wagi całego RAG_ETAP3.

1. **Stan SQL przenosi się do core**, per scope, do własnego pliku obok danych scope'u
   (`<scope>/graph.db`). Ta sama zasada co przy wektorach i zdarzeniach: stan runtime pojedynczego
   węzła, pisany często, poza Sync Ledger.
2. **Logika przenosi się z wasm do `services/graph_memory/`**: rejestr schematów z progiem τ,
   skan konfliktów, rozstrzyganie przez `rag-llm`, entity merge z odwracalnością przez outbox.
3. **Addon staje się klientem** — jego 12 tabel znika, host-fn wołają usługę. Migracja danych
   istniejących instalacji: przenieś per instancja.
4. **Sprzątnij duplikaty przy okazji**: `normalize_entity_name` istnieje dziś w addonie i lustrzanie
   w `rag_graphrag.rs`; stała `kg_active` jest powtórzona w trzech miejscach z komentarzem „MUSI byc
   identyczna".

**Odbiór G2:** te same fakty wchodzące do Projektu i do kolekcji addona dają identyczny graf,
identyczne konflikty i identyczne scalenia encji; cofnięcie merge'u działa po obu stronach.

**Uczciwie o kolejności:** G2 nie jest warunkiem G1. Projekt z samym G1 ma działający graf wiedzy —
bez ontologii i bez rozstrzygania konfliktów, czyli z faktami sprzecznymi obok siebie. To jest
użyteczne i lepsze niż brak grafu, ale trzeba wiedzieć, czego się nie ma.

### 1.3 Jedna powłoka retrievalu

**Stan:** ciało (`retrieval-round`) wspólne. Powłoki dwie: platformowy `query` (addon) i `ps-chat`
(Projekty). Różnice: `query` nie streamuje (`emit_citations`), ma zaszyte `model: rag-llm`, jest
wołany po nazwie zamiast po stałym `flow_id`.

**Do zrobienia:**
1. Węzeł `output` w `query` obsługuje **oba tryby** — streaming i blok z cytatami — sterowane
   `meta`, nie zaszyte w configu.
2. Węzeł `answer` bierze model z `envelope.meta` z **fallbackiem** na `rag-llm`, zamiast go zaszywać.
3. `ps-chat` przechodzi na `query` jako ciało, zachowując streaming i model projektu.

**Ostrzeżenie z doświadczenia:** wcześniej zapisałem tu czwarty warunek — „reranker wywala flow przy
niezwiązanym aliasie". **To była pomyłka.** `retrieval-round` karmi reranker trafieniami ze scorami,
dla których istnieje degradacja do kolejności wektorowej. Błąd dotyczy wyłącznie generycznego
kontraktu `{query, candidates}`, którego ten flow nie używa. **Sprawdzaj kontrakt wejścia, nie
komentarz w teście.**

**Decyzje właściciela — zatwierdzone, nie cofać bez ponownej zgody:**

1. **Czat projektu odpowiada WYŁĄCZNIE z bazy wiedzy.** Prompt węzła `answer` we wspólnej powłoce
   nakazuje: brak odpowiedzi w kontekście = powiedz wprost, że nie wiesz, i nie zmyślaj faktów spoza
   kontekstu. Wycofany `ps-chat` pozwalał domówić resztę „najlepiej jak umiesz". To zmiana persony,
   świadoma i przyjęta. Pilnuje jej test
   `db::seed::tests::shell_answer_prompt_grounds_the_model_in_the_retrieved_context` — sprawdza
   obowiązki promptu, nie jego brzmienie, więc przeredagowanie jest wolne, a usunięcie warunku nie.
2. **Projekt bez przypisanego agenta czatu odmawia głośno.** Handler streamu przerywa turę PRZED
   dispatchem i zwraca `ChatStreamEnd{status:"error"}` z komunikatem, że projekt nie ma
   skonfigurowanego modelu czatu i gdzie go ustawić. `model_fallback: "rag-llm"` zostaje w flow, bo
   to własny domyślny model addona RAG (bundla WASM nie wolno przebudowywać) — ale czat projektu
   już do niego nie dociera. Cichy fallback odpowiadałby właścicielowi modelem, którego nigdy nie
   wybrał, i nic w UI by tego nie powiedziało. Test:
   `dispatch::stream_handlers::tests::project_chat_refuses_a_project_with_no_chat_model`.

**Odbiór 1.3:** to samo pytanie zadane przez addon i przez czat projektu przechodzi tym samym flow;
czat nadal streamuje i nadal używa modelu projektu.

## 2. Śledzenie zdarzeń — specyfikacja

### 2.1 Na jakie pytania ma odpowiadać

1. **Skąd przyszło to wywołanie?** — Code Studio, API zewnętrzne, addon, projekt, czat, kamera,
   scheduler, mesh.
2. **Kto je wywołał?** — użytkownik, klucz API (i **czy klucz jest powiązany z użytkownikiem**),
   addon, system.
3. **Jak przebiegło?** — ile trwał model, ile czekaliśmy na pierwszy token, ile trwało każde
   narzędzie, co się nie udało.
4. **Gdzie to widać?** — jeden moduł globalny + oś czasu przy konkretnym przebiegu w Code Studio.

### 2.2 Miejsce składowania: osobny plik SQLite

`<data>/events.db`. **Rozstrzygający argument nie dotyczy rozmiaru pliku:** główna baza ma JEDNO
połączenie pisarza i zapisy się w nim serializują (`db/mod.rs`: „`write()` bierze jedyne połączenie
pisarza spod `Mutex`"). Log zdarzeń jest wysokoczęstotliwościowy — w głównej bazie konkurowałby o
ten sam zamek z zapisem ustawień, flow, agentów i audytu.

Precedens jest u nas dosłowny: `code_studio/workspace_db.rs` („runtime state of a single node,
written constantly, must not travel through the Sync Ledger"). Maszyneria puli LRU z migracjami przy
otwarciu i `checkpoint_all` istnieje w `project_db.rs` i `workspace_db.rs` — **skopiować, nie
wymyślać**.

**Jeden plik na węzeł, nie na sesję** — przeglądarka pyta w poprzek pochodzeń.

### 2.3 Schemat

```sql
CREATE TABLE run_events (
  run_id           TEXT    NOT NULL,
  seq              INTEGER NOT NULL,
  at_ms            INTEGER NOT NULL,   -- epoch ms
  kind             TEXT    NOT NULL,   -- request_started | first_token | assistant_message
                                       -- | tool_call | tool_result | step_start | step_end
                                       -- | turn_start | turn_end | error
  origin           TEXT    NOT NULL,
  actor_kind       TEXT    NOT NULL,   -- user | api_key | addon | system
  actor_id         TEXT,               -- user_id / nazwa klucza / addon_id
  actor_user_id    TEXT,               -- user stojący za kluczem API; NULL = klucz serwisowy
  org_id           TEXT,               -- organizacja przebiegu; NULL = przebieg bez najemcy
                                       -- (kamera, harmonogram, konserwacja)
  correlation_id   TEXT,
  session_id       TEXT,
  node_id          TEXT,
  call_id          TEXT,               -- paruje tool_call z tool_result
  payload_json     TEXT    NOT NULL,   -- PO redakcji
  idempotency_key  TEXT,
  PRIMARY KEY (run_id, seq)
);
CREATE INDEX ix_run_events_time    ON run_events(at_ms);
CREATE INDEX ix_run_events_origin  ON run_events(origin, at_ms);
CREATE INDEX ix_run_events_actor   ON run_events(actor_id, at_ms);
CREATE INDEX ix_run_events_corr    ON run_events(correlation_id);
CREATE INDEX ix_run_events_org     ON run_events(org_id, at_ms);
CREATE UNIQUE INDEX ux_run_events_idem ON run_events(run_id, idempotency_key)
  WHERE idempotency_key IS NOT NULL;
```

`PRAGMA auto_vacuum = INCREMENTAL`.

### 2.4 Writer — trzy własności, nie do negocjacji

Kopiujemy z `code_studio/events.rs`, bo ten model już się u nas obronił:

1. **`seq` alokowany jako `MAX(seq)+1` WEWNĄTRZ transakcji insertu.** `PRIMARY KEY (run_id, seq)`
   zamienia drugiego równoległego pisarza w **głośny błąd**, a nie w cicho poprzeplatany log.
2. **Idempotencja.** Powtórka pod tym samym `idempotency_key` jest no-opem zwracającym `duplicate`.
   Awaria między „efekt się wydarzył" a „zdarzenie zapisane" rozwiązuje się przez ponowny zapis.
3. **Projekcje, nie prawda.** `flow_executions` i `agent_runs` zostają jako indeksy; przy rozjeździe
   wygrywa oś czasu.

### 2.5 Pochodzenie i aktor — gdzie stemplować

Nowe pola w `FlowRequestMeta` (`flow_engine/dispatcher.rs`) obok istniejących `addon_id` / `org_id`
/ `vector_home`:

```rust
pub origin: FlowOrigin,          // enum, nie String — literówka ma nie przechodzić
pub actor_kind: ActorKind,
pub actor_id: Option<String>,
pub actor_user_id: Option<String>,
```

**Stempel należy do punktu wejścia i NIGDY nie pochodzi z treści modelu** — ta sama zasada co
`ps_generation` i `vector_home`. Punkty wejścia (`FlowRequestMeta::new`):

| Plik | Kontekst | `origin` |
|---|---|---|
| `dispatch/stream_handlers.rs` — `flow_invoke_handler` | dashboard | `chat` |
| `dispatch/stream_handlers.rs` — `project_studio_chat_stream_handler` | czat projektu | `project` |
| `dispatch/stream_handlers.rs` — `project_studio_code_assist_stream_handler` | Code Studio | `code_studio` |
| `routing/mod.rs` — `build_initial_envelope_inner` | `/v1/*` | `api` |
| `services/runtime/executor.rs` — `ingest_request_to_initial_envelope` i sąsiednie | addon / projekt | wg wołającego |
| `agents/subagent_reactor.rs` | sub-agent | dziedziczy po rodzicu |
| `services/camera_ingest/vision_analysis.rs` | kamera | `camera` |

**Klucz API → użytkownik.** `actor_user_id` musi być rozwiązany **po stronie serwera** przy
autoryzacji klucza; z samego wywołania tego nie widać. `NULL` znaczy „klucz serwisowy bez
powiązania" i UI ma to pokazywać jawnie, a nie jako puste pole.

### 2.6 Karmienie loga — zero nowej instrumentacji

Nowy `ProgressSink` (`flow_engine/dispatchers/progress.rs`) obok brokera. **Nie dokładamy liczników
do adapterów** — to jest zasada wzięta z DeepSeeka i powód, dla którego ich czasy nie kłamią.

Istniejące zdarzenia wystarczają na wszystko **poza TTFT**. Brakuje jednego:

```rust
ProgressEvent::FirstToken { node_id: String }
```

emitowane w ścieżce strumieniowej przy **pierwszym niepustym delcie** (nie przy pierwszym chunku —
puste delty się zdarzają). To jedyny nowy punkt emisji w całym zadaniu.

`ToolCallStarted`/`ToolCallFinished` mają już `call_id` (dodane, bo po zrównolegleniu parowanie po
nazwie łączyło start jednego wywołania z końcem drugiego).

### 2.7 Metryki jako zapytania

```sql
-- czas narzędzia (parowanie po call_id, niezamknięte odrzucane przy turn_end)
SELECT c.call_id, c.payload_json ->> '$.name' AS tool,
       r.at_ms - c.at_ms AS ms
FROM run_events c JOIN run_events r
  ON r.run_id = c.run_id AND r.call_id = c.call_id AND r.kind = 'tool_result'
WHERE c.kind = 'tool_call';

-- TTFT: request_started -> first_token w tym samym kroku
-- dekodowanie: first_token -> assistant_message
```

**Niezamknięte wywołania odrzucamy przy zamknięciu tury** — inaczej pierwsza awaria zatruwa
statystyki na stałe (to konkretna lekcja z DeepSeeka).

### 2.8 Redakcja i audyt

- **Redakcja PRZED zapisem**, przez `code_studio/redact.rs`. Bez tego log staje się tym wyciekiem,
  który ma wykrywać. To warunek etapu, nie dodatek.
- **Zdarzenie security-relevant + jego kopia audytowa w jednej transakcji**, potem osobny krok
  przenosi kopię do `audit_log` w bazie głównej — wzorzec `code_studio/audit_outbox.rs`,
  **at-least-once**.
- **Asymetria jest celowa:** audyt nie może stracić wpisu, oś czasu może stracić ogon. Audyt jest
  zobowiązaniem, oś czasu narzędziem.
- `compliance_ai_events` **zostaje bez treści promptów** i ma własną retencję ≥183 dni. Nowy log ma
  własną, krótszą.

### 2.9 Retencja, objętość, sync

- **Retencja musi przyjść RAZEM z tabelą**, nie później. Domyślnie 30 dni, konfigurowalnie.
- **Każdy wiersz kasowany na terminie SWOJEJ organizacji** — dlatego `run_events` ma `org_id`.
  Termin rozstrzyga się per organizacja (`compliance_retention_policies`, zakres `events`), a
  polityka jednego najemcy nie może rządzić danymi drugiego; bez przypisania wiersza do najemcy w
  chwili czyszczenia jedynym bezpiecznym odczytem byłby najkrótszy termin na węźle, czyli kasowanie
  cudzych danych przed czasem. Wiersze, których żadna organizacja na węźle nie obejmuje (`org_id
  IS NULL` albo organizacja bez polityki), idą właśnie na najkrótszym obecnym terminie: zgadywanie
  właściciela byłoby fabrykacją (inwariant 6), a brak reguły — nieśmiertelnym ogonem.
- **`run_events` NIE wchodzi do Sync Ledger** (jak `flow_executions` i `audit_log`). Inaczej każdy
  węzeł replikuje oś czasu każdego innego.
- Przy dużej objętości: rotacja miesięczna jako plan B.

### 2.10 UI

Mockupy: `mockups/zdarzenia-20260819/` (Z01 jest **działającym prototypem**, nie makietą).

**Trzy wejścia:**
1. Nawigacja → Zarządzanie → **Zdarzenia**, obok „Dziennika audytu". Zakres wg uprawnień.
2. Zakładka **„Oś czasu"** przy sesji Code Studio (obok Konsoli/Plików/Zmian/Gita), analogicznie
   przy przebiegu agenta i czacie projektu.
3. Odsyłacz z `audit_log` przez `correlation_id` → oś czasu w tym miejscu. **Wymaga stemplowania
   `correlation_id` po obu stronach** — warunek etapu 1, nie dodatek.

**Wymagania interakcji, rozstrzygnięte w prototypie:**
- trzy tory: model / wiadomości / narzędzia; granice tur jako pionowe linie,
- pasmo modelu **rozcięte na TTFT i dekodowanie** — „model myślał 8 s" nie mówi, czy czekał, czy pisał,
- kółko = zoom **w punkcie kursora**, przeciągnięcie = zaznacz zakres, prawy przycisk = pan,
  dwuklik = całość,
- **minimapa** pod osią: całość + okno widoku,
- **sprzężenie w obie strony**: wiersz ↔ pasmo, z przewinięciem do widoku,
- **dwie skale**: „czas rzeczywisty" i „równe odstępy". Przy realnym czasie wywołanie 22 ms obok
  builda 4 min jest niewidoczne. Druga skala robi oś **nieliniową**, więc to **jawny przełącznik,
  nigdy automat**,
- **bez fabrykowania czasu**: rekord w locie dostaje znacznik startu, nie zmyślony pasek,
- powiązanie klucza API z użytkownikiem widoczne **w liście aktorów i w inspektorze**.

**Czego prototyp NIE dowodzi:** zachowania przy tysiącach rekordów. Wirtualizacja listy i agregacja
pasm przy dużym oddaleniu — do zmierzenia na realnych danych, nie do zaprojektowania z góry. Brak
też obsługi klawiatury i widoku mobilnego.

### 2.11 Etapy z kryteriami odbioru

| # | Zakres | Odbiór |
|---|---|---|
| 1 | `origin` + `actor*` w `FlowRequestMeta`, stempel w każdym punkcie wejścia, `correlation_id` po obu stronach | `flow_executions` odpowiada „skąd i kto" bez nowej tabeli; test per punkt wejścia |
| 2 | `events.db`: schemat, pula, writer, redakcja, outbox audytowy, retencja | dwa równoległe zapisy → głośny błąd, nie przeplot; powtórka = `duplicate`; brak nieredagowanych danych na dysku |
| 3 | `ProgressSink` + `FirstToken` | z logu da się policzyć TTFT, dekodowanie i czas per narzędzie; zero liczników w adapterach |
| 4 | Metryki jako zapytania | te same liczby co ręcznie zmierzone na znanym przebiegu |
| 5 | Przeglądarka (oś + rejestr + inspektor) | interakcje z §2.10; klucz API pokazuje powiązanie |
| 6 | Osadzenie w Code Studio + odsyłacz z audytu | z wpisu audytu jedno kliknięcie do miejsca w osi |
| R1 | §1.1 trwała kolejka (`jobs.db`) | zadanie Projektów przeżywa restart; addon i Projekty w jednej kolejce |
| R2 | §1.2 G1 — `graph_home` + węzeł `graph_extract` | projekt ma niepuste `kg_active` u siebie; wyłączony graf = zero dodatkowych wywołań LLM |
| R3 | §1.3 jedna powłoka | to samo pytanie z addona i z czatu przechodzi tym samym flow; czat nadal streamuje |
| R4 | §1.2 G2 — warstwa utrzymania w core | identyczny graf, konflikty i scalenia po obu stronach; cofnięcie merge'u działa |

---

## 3. Inwarianty — złamanie któregokolwiek to błąd, nie kompromis

1. **`origin`, `actor*`, `vector_home`, `graph_home` i scope NIGDY nie pochodzą z treści modelu.** Zawsze
   mintowane przez serwer po autoryzacji.
2. **`seq` alokowany w transakcji insertu**, z ograniczeniem unikalności.
3. **Redakcja przed zapisem.**
4. **`run_events` poza Sync Ledger.**
5. **Zero nowej instrumentacji czasu w adapterach** — czasy to różnice zdarzeń.
6. **Nie fabrykujemy danych, których nie mamy** — brak wyniku to luka w logu, nie zmyślony wynik.

---

## 4. Pułapki z tej pracy — żeby nie wdepnąć drugi raz

- **rustfmt przeformatuje cały plik**, w tym cudzy kod. Po `rustfmt` sprawdź `git diff --numstat`;
  jeśli zmian jest wielokrotnie więcej niż wprowadziłeś — odsiej hunki albo cofnij i nałóż zmianę
  ponownie.
- **`cargo fix` posprząta ostrzeżenia w kilkudziesięciu niezwiązanych plikach.** Nie commituj tego
  razem ze swoją zmianą.
- **Filtrowanie hunków po markerach gubi linie bez markera** (np. dodane `None,`). Po odsianiu
  ZAWSZE sprawdź, czy zastage'owana wersja sama się kompiluje (`git stash push --keep-index`).
- **Czerwony test nie zawsze jest nieaktualny.** `rejects_multi_input_edge` łapał realny błąd —
  zbyt szerokie zwolnienie `llm` z R4. Sprawdź, zanim „naprawisz" asercję.
- **Sprawdzaj kontrakt wejścia, nie komentarz w teście** (patrz pomyłka z rerankerem, §1.3).
- **Weryfikuj, co naprawdę wylądowało na zdalnej gałęzi.** PR potrafi złapać gałąź o dwa commity
  za wcześnie.

---

## 5. Otwarte pytania do człowieka

1. **Czy `origin` jest pierwszym filtrem, czy aktor?** Prototyp zakłada `origin`. Jeśli częściej
   szukasz po użytkowniku, hierarchia w pasku się zamienia.
2. **Retencja loga zdarzeń** — ROZSTRZYGNIĘTE: 30 dni (konfigurowalnie, per organizacja). Krócej =
   mniej materiału na diagnozę, dłużej = objętość. Termin świadomie stoi daleko poniżej ≥183 dni,
   którymi związany jest audyt AI: oś czasu jest narzędziem diagnostycznym, a ślad audytowy —
   zobowiązaniem.
3. **Czy oś czasu jest widoczna dla zwykłego użytkownika** (swoje przebiegi), czy tylko dla admina?
   Prototyp zakłada pierwsze.
4. **Domyślne włączenie grafu w Projektach.** G1 daje graf, ale ekstrakcja to dodatkowe wywołania
   LLM na każdy chunk. Proponuję domyślnie WYŁĄCZONY, włączany per projekt — inaczej pierwszy duży
   ingest zaskoczy rachunkiem.
5. **Czy G2 wchodzi teraz, czy po G1.** G1 daje działający graf bez ontologii i bez rozstrzygania
   konfliktów — fakty sprzeczne stoją obok siebie. Użyteczne, ale trzeba wiedzieć, czego się nie ma.
