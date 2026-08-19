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

## 1. RAG — co zostało

Trzy pozycje. Każda była **świadomie odłożona**, nie zapomniana, i każda ma zapisany warunek, który
musiałby się zmienić.

### 1.1 Wspólna trwała kolejka ingestu

**Stan:** addon ma durable `ingest_jobs` w swoim SQLite, drenowane przez Scheduler
(`ingest_drain`). Projekty mają zadania w procesie (`start_job` → `tokio::spawn`), ginące przy
restarcie. Wspólne są już: limit współbieżności i rejestr anulowania.

**Dlaczego odłożone:** to nie jest jedna zduplikowana mechanika, tylko **dwa różne modele
trwałości**. Sprowadzenie do jednego wymaga migracji danych addona i zmiany jego modelu
synchronizacji.

**Co trzeba zrobić, gdyby wchodziło:**
1. Usługa core: kolejka + postęp + cancel + limit, z trwałością w **osobnym pliku** (patrz §2.2 —
   ten sam argument o pojedynczym pisarzu głównej bazy).
2. `ingest_drain` addona staje się cienkim wywołaniem host-fn zamiast własnego SQL.
3. Migracja `ingest_jobs` z bazy instancji: albo przeniesienie wierszy, albo odczekanie na
   opróżnienie kolejki i skasowanie tabeli. **Bundle hash addona się zmieni** — przebudowany addon
   zostaje wyłączony do zatwierdzenia nowego hasha.

**Warunek wejścia:** ktoś zgłasza, że zadanie ingestu ginie przy restarcie w Projektach. Dopóki nie
ginie w praktyce, koszt migracji przewyższa zysk.

### 1.2 Warstwa grafowa dla Projektów

**Stan:** Projekty nie mają grafu. Ekstrakcja (`extract_chunk_graph`, konflikty A_det/A_res,
entity merge) żyje **w wasm addona**, nie w nodach flow — flow ingestu nie ma żadnego węzła
grafowego, graf powstaje w kodzie addona PO flow, czytając teksty chunków z `passages`.

**Strona retrievalowa JEST w core** (`rag_graphrag.rs`, `graph_search.rs`) i `retrieval-round` ma ją
wpiętą — dla scope projektowego degraduje się do pass-through, bo nie ma kolekcji `kg_active`.
`ps-chat` ustawia `graph_enabled=false` jawnie, żeby oszczędzić tej pracy.

**Dlaczego odłożone:** przeniesienie MemGraphRAG do core to projekt wagi całego RAG_ETAP3, a graf
**nie jest zduplikowany** — Projekty go po prostu nie mają. To luka, nie dług.

**Znane duplikacje w tej warstwie** (do posprzątania niezależnie): `normalize_entity_name` istnieje
w `addons/rag/src/lib.rs` i lustrzanie w `node_adapters/rag_graphrag.rs`; stała `kg_active`
powtórzona w trzech miejscach z komentarzem „MUSI byc identyczna".

### 1.3 Zewnętrzna powłoka retrievalu

**Stan:** ciało (`retrieval-round`) jest wspólne. Powłoki są dwie: platformowy flow `query` (używa
go addon) i `ps-chat` (Projekty).

**Czego brakuje do pełnej jedności** — trzy konkretne różnice w `query`:
1. węzeł `output` ma `emit_citations`, **nie streamuje**; `ps-chat` jest streamingowy,
2. węzeł `answer` ma **zaszyte `model: rag-llm`**; `ps-chat` bierze model z `envelope.meta`,
3. `query` jest wołany jako model po nazwie, `ps-chat` po stałym `flow_id`.

**Ostrzeżenie z doświadczenia:** wcześniej zapisałem tu czwarty warunek — „reranker wywala flow przy
niezwiązanym aliasie". **To była pomyłka.** `retrieval-round` karmi reranker trafieniami ze scorami,
dla których istnieje degradacja do kolejności wektorowej. Błąd dotyczy wyłącznie generycznego
kontraktu `{query, candidates}`, którego ten flow nie używa. Sprawdzaj kontrakt wejścia, nie
komentarz w teście.

**Warunek wejścia:** chęć, żeby addon i Projekty miały identyczną powłokę. Dziś różnią się
uzasadnienie: jedna streamuje do przeglądarki, druga zwraca blok z cytatami.

---

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

- **Retencja musi przyjść RAZEM z tabelą**, nie później. Domyślnie 14 dni, konfigurowalnie.
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

---

## 3. Inwarianty — złamanie któregokolwiek to błąd, nie kompromis

1. **`origin`, `actor*`, `vector_home` i scope NIGDY nie pochodzą z treści modelu.** Zawsze
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
2. **Retencja loga zdarzeń** — proponowane 14 dni. Krócej = mniej materiału na diagnozę, dłużej =
   objętość.
3. **Czy oś czasu jest widoczna dla zwykłego użytkownika** (swoje przebiegi), czy tylko dla admina?
   Prototyp zakłada pierwsze.
4. **Czy wchodzimy w §1.1 lub §1.3**, czy zostają odłożone do zgłoszenia z produkcji.
