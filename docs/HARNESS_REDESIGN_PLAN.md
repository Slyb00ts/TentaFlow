# Plan przebudowy harnessu: jeden flow + natywna pętla inline

> Cel: złożyć agentowy harness w **jeden flow** (zamiast seedowanych 3: `…011`/`…012`/`…013`),
> z **natywną pętlą inline** (region, nie subflow), zachowując i ujmując w blokach: pytania do
> użytkownika, pracę w tle, spawnowanie subagentów w osobnych wątkach, okresowe sprawdzanie progresu,
> trigger po zakończeniu subagenta. Plus: wszystkie prompty edytowalne w blokach.
>
> Oparty na realnym audycie kodu (`docs/HARNESS_AUDIT.md`, `docs/CODEX_HARNESS_INTERNALS.md`).

---

## CZĘŚĆ 0 — Co już działa (i musi przetrwać przebudowę)

Z audytu `agents/run_manager.rs`, `agents/interaction.rs`, `tool_exec.rs`:

| Zdolność | Jak działa dziś | Sterowane przez |
|---|---|---|
| **Subagent w osobnym wątku** | `tokio::spawn(run_task)` per child, semafor `max_concurrent_runs=8`, FIFO; caps `max_spawn_depth`/`max_subagents` | tool `core.agent_spawn` → `AgentRunManager::spawn` (run_manager.rs:396) |
| **Praca w tle** | child run jest detached; rodzic dostaje `run_ids` natychmiast i leci dalej; wynik wraca przez **mailbox** (tabela `agent_mailbox`) lub `on_child_complete="continue"` (auto-respawn rodzica) | `deliver_child_result` (run_manager.rs:1035) |
| **Okresowy progres** | `ProgressEvent::ChildSpawned/ChildFinished` strumieniem do UI; `core.agent_list` zwraca statusy dzieci; heartbeat 30 s (liveness) | `progress.publish` + tool `core.agent_list` |
| **Trigger po zakończeniu** | po `Completed`: mailbox enqueue + `ChildFinished` + (opcjonalnie) auto-continuation; rodzic wciąga wynik w `agent_context::drain_mailbox` przy następnym prime | run_manager.rs:982-993, agent_context.rs:114-149 |
| **Pytanie do użytkownika** | `ask_user` blok lub `core.ask_user` tool → `InteractionRegistry`, zwolnienie permitu, `extend_deadline`, wznowienie po odpowiedzi/timeout | interaction.rs:344-429, ask_user.rs |
| **Zgody (permission)** | `tool_exec` na `NotConfigured` woła `run_permission_request`; decyzje `Deny/AllowOnce/AllowForRun/Always`; cache per-run | interaction.rs:437-503 |

**Wniosek:** logika tła/subagentów/pytań **nie jest** problemem — problemem jest, że pętla, która to
spina, jest subflowem rozbitym na 3 grafy. Przebudowa dotyczy **executora + struktury flow**, a te
zdolności podpinamy z powrotem jako bloki/narzędzia w jednym flow.

---

## CZĘŚĆ 1 — Model docelowy: region pętli inline

### 1.1 Zasada

Region pętli to **oznaczony podgraf w TYM SAMYM flow_json**, z jedną krawędzią wsteczną, wykonywany
**inline nad jednym envelope** (append-only `context.messages`, jak historia `Session` w codeksie).
Brak `SubflowRunner`, brak rekompilacji per iteracja, brak osobnej definicji w DB.

```
codex run_turn:  loop { stream model → exec tools → append history → if tool_calls continue else break }
TentaFlow:       loop_region { llm(tools) → tool_exec → [back-edge] }  stop: ostatni assistant bez tool_calls
```

### 1.2 Kodowanie w grafie (decyzja: region jako opaque super-node)

Z badania executora (`cache.rs` Kahn toposort odrzuca cykle; `validation.rs` R4 = dokładnie 1 wejście):
najmniej inwazyjny model to **region jako pojedynczy węzeł w zewnętrznym DAG-u**:

- Węzły ciała pętli mają w flow_json znacznik `region: "<loop_id>"`.
- Jedna krawędź jest oznaczona `kind: "loop_back"` (z węzła-wyjścia regionu do węzła-wejścia regionu).
- **Compile** (`cache.rs`): krawędzie `loop_back` są **wyłączone z liczenia in-degree** w Kahnie →
  toposort nie widzi cyklu. Region kompiluje się raz, jako wewnętrzny sub-execution-order zapięty
  do węzła regionu. Zewnętrzny graf pozostaje acykliczny — szybki scheduler bez zmian.
- **Executor**: nowy `LoopRegionExecutor` uruchamia wewnętrzny sub-DAG **inline** (ta sama pętla
  schedulera co `execute_blocking`, ale nad sklonowanym-raz envelope), po każdej iteracji sprawdza
  warunek stopu i resetuje `pending_deps`/`live_inputs` regionu (mechanika z badania executora,
  sekcja „What Must Change").

### 1.3 Warunek stopu — strukturalny, nie magiczny string

Dziś: `tool_exec` ustawia `meta.harness_done` (tool_exec.rs:481), pętla czyta CEL `until`
(loop_block.rs:84). Docelowo region czyta **strukturalnie**: „ostatnia wiadomość assistant nie ma
`tool_calls`" — to jest dokładnie `needs_follow_up == false` codeksa, liczone tam gdzie wykonujemy
turę. Znika `harness_done`, `until`, i **całe heurystyki nudge** (loop_block.rs:200-302) — pętla widzi
intencję modelu strukturalnie, więc nie zgaduje z tekstu `"i'll "`.

Konfiguracja regionu (zostaje, ale typowana):
- `max_iterations` (domyślnie 25, cap 100) — bezpiecznik.
- `final_pass` (bool) — grace-summary po wyczerpaniu budżetu (zachowane).
- `stop`: domyślnie `no_tool_calls` (strukturalny); opcjonalny CEL override dla zaawansowanych.

### 1.4 Co zachowujemy z dzisiejszej pętli

- Light-mode audyt (brak wiersza `flow_executions` per iteracja).
- Guardy: `max_iterations` cap, cancel/deadline przed każdą iteracją, `effective_deadline`
  (rozszerzenie o human-wait — KLUCZOWE dla ask_user wewnątrz regionu).
- Progres: `IterationStarted/Finished` (loop_block.rs:392) + `NodeStarted/Finished` węzłów regionu.
- Streaming: finalna iteracja streamuje (ostatni `llm` jest producentem); iteracje pośrednie blokujące.

---

## CZĘŚĆ 2 — Zmiany w rdzeniu (executor / compile / walidacja)

### 2.1 `cache.rs` (compile + toposort)
- Rozpoznaj krawędzie `loop_back`; wyłącz je z `in_degree` w `topological_sort` (cache.rs:218).
- Zbuduj `regions: Vec<LoopRegion>` z `region_id`, lista węzłów ciała, węzeł-wejście, węzeł-wyjście,
  krawędź wsteczna, config (`max_iterations`/`final_pass`/`stop`).
- Region reprezentuj w `execution_order` jako jeden „super-pos"; wewnętrzny porządek osobno.

### 2.2 `executor.rs`
- Nowy `run_loop_region(region, envelope, ctx)`: pętla `loop { exec internal sub-DAG inline →
  sprawdź stop → reset pending_deps/live_inputs regionu → iteracje++ }` nad jednym mutowalnym envelope.
- `execute_blocking`/`execute_streaming` traktują super-pos regionu jak węzeł (spawn = wejście do
  `run_loop_region`); reszta schedulera bez zmian.
- Streaming: gdy region jest producentem (`stream` port), iteracje pośrednie blokujące, finalna przez
  `produce_stream` wewnętrznego `llm` (mechanika z dzisiejszego loop_block.rs:594, ale bez SubflowRunner).

### 2.3 `validation.rs` (R1–R10)
- **R4**: dodaj węzeł-wejście regionu do wyjątków (jak `combine`/`output`) — ma wejście „z zewnątrz"
  + krawędź `loop_back`. Albo: krawędź `loop_back` nie liczy się do in-degree w R4.
- **Nowa R11 (region integrity)**: krawędź `loop_back` musi prowadzić wyłącznie między węzłami tego
  samego `region_id`; region ma dokładnie jedno wejście i jedno wyjście; brak krawędzi przecinających
  granicę regionu poza wejściem/wyjściem; `max_iterations <= 100`.
- **R7 (streaming)**: dopuść region jako `StreamProducerAdapter`, gdy jego węzeł-wyjście streamuje.
- **R6/R8** bez zmian.

### 2.4 `ExecutionContext`
- Bez nowych pól krytycznych — `effective_deadline`/`deadline_extension_ms` (Arc atomic) już wspierają
  human-wait wewnątrz regionu. Opcjonalnie `loop_region_depth` dla zagnieżdżonych regionów (analog
  `subflow_depth`).

### 2.5 Migracja danych (`db/seed.rs`, `migrations.rs`)
- Zastąp seedy `…011`/`…012`/`…013` **jednym** flow „Agent Run" z regionem pętli inline.
- Migracja: istniejące flow agentów z `loop(body=…013)` przepisz na region inline (ten sam graf,
  inny zapis); stare 3 flow zostają jako read-only legacy do czasu potwierdzenia.
- `agent.flow_id` wskazuje teraz jeden flow.

---

## CZĘŚĆ 3 — Mapowanie zdolności na bloki (istniejące + DO DOPISANIA)

Legenda: ✅ istnieje · 🔧 zmiana · 🆕 do dopisania

### 3.1 Rdzeń pętli
- 🔧 **`loop` → region semantics** — przestaje być subflow-runnerem; staje się znacznikiem regionu
  (Część 1–2). Stop strukturalny. Pola promptów nudge **usunięte** (zbędne) lub przeniesione jako
  opcjonalne (patrz Część 5).
- ✅ **`llm`** — węzeł tury modelu; `system_prompt` już edytowalny (llm.rs:86). 🔧 czyta `tools` z
  `harness_tools` w meta; `pick_tools` zostaje (final_pass drop tools).
- ✅ **`tool_exec`** — wykonuje `tool_calls`: `core.*` (spawn/wait/list/cancel/ask_user/skill_view) +
  addon tools. 🔧 stop liczony strukturalnie zamiast `meta.harness_done`.
- ✅ **`agent_context`** — prime: system prompt + skills + **mailbox drain** (completion-pull) +
  `harness_tools` + budżet. Zostaje, wchodzi do jednego flow przed regionem.

### 3.2 Pytania do użytkownika
- ✅ **`ask_user`** (ask_user.rs) — pauza/wznowienie, `extend_deadline`. Działa wewnątrz regionu
  (deadline-extension to Arc). Bez zmian poza testem „ask_user w regionie".
- ✅ tool **`core.ask_user`** — ścieżka model-driven (model sam pyta w trakcie tury).

### 3.3 Praca w tle + subagenty w osobnych wątkach
- ✅ tool **`core.agent_spawn`** — model deleguje w tle (detached tokio task). Ścieżka domyślna.
- 🆕 **`spawn` blok** — *deterministyczny* (graf, nie model) launch sub-agenta/flow w tle. Wrapper na
  `AgentRunManager::spawn`. Config: `agent_id`/`target_flow_id`, `input_mapping`, `detach: true`,
  `on_complete: notify|continue`. Zwraca `run_ids` do `variables`. Dla flow, które chcą rozgałęzić
  pracę bez udziału modelu.

### 3.4 Sprawdzanie progresu co jakiś czas
- ✅ tool **`core.agent_list`** — snapshot statusów dzieci (model-driven poll).
- 🆕 **`subagent_status` blok** — deterministyczny snapshot (wrapper na `agent_list` /
  `list_agent_runs_by_parent`): zwraca tablicę `{run_id,status,progress}` do `variables`. Wstawiany w
  pętli z bramką czasową, by „sprawdzać co jakiś czas".
- 🆕 **`interval` / `delay` blok** — bramka czasowa (np. co 10 s) dla regionu „polluj aż dzieci
  skończą". Bez busy-loop: `tokio::time::sleep` z honorem cancel/deadline. Umożliwia wzorzec
  „spawn → [region: status → interval] until all done → combine".

### 3.5 Trigger po zakończeniu subagenta
- ✅ **mailbox pull** — `agent_context::drain_mailbox` wciąga wynik przy następnym prime (pasywne).
- ✅ **`on_child_complete="continue"`** — auto-respawn rodzica z wynikiem (aktywne).
- 🆕 **`on_subagent_complete` trigger** — *drugi typ triggera* (event trigger): flow startuje/wznawia
  się, gdy `ProgressEvent::ChildFinished` dla danego scope. Pozwala zbudować *reaktywny* flow
  („gdy subagent skończy → przetwórz wynik → powiadom"). Implementacja: subskrypcja brokera progresu
  + wejście do flow z `run_id`/`payload` z mailbox. To czyni „trigger po zakończeniu" pierwszorzędnym
  obywatelem grafu, nie tylko mechanizmem wewnętrznym.
- 🆕 **`await_subagents` / `join` blok** — deterministyczny odpowiednik `core.agent_wait`: blokuje
  (z timeout) aż nazwane runy się ustabilizują; zwalnia permit jak `enter_waiting_user`. Config:
  `run_ids` (z variables), `timeout_secs`, `mode: all|any`.

### 3.6 Pomocnicze (zostają)
- ✅ `conversation_history`, `compact_context`, `memory`, `session_context`, `speaker_context`,
  `pii_filter`, `combine`, `condition`, `output`, `agent_router`, `subflow`, `map`.
- `subflow`/`map` zostają osobne (kompozycja flow / fan-out są zasadne) — to **tylko pętla agentowa**
  przestaje być subflowem.

### 3.7 Podsumowanie: bloki DO DOPISANIA
1. 🆕 `spawn` — deterministyczne tło.
2. 🆕 `await_subagents` (`join`) — deterministyczny wait.
3. 🆕 `subagent_status` — snapshot progresu.
4. 🆕 `interval`/`delay` — bramka czasowa do okresowego pollingu.
5. 🆕 `on_subagent_complete` — event trigger (drugi typ triggera).
+ 🔧 przebudowa `loop` na region inline (rdzeń).

---

## CZĘŚĆ 4 — Prompty jako pola edytowalne w blokach

Z inwentaryzacji (`agent_context.rs`, `compact_context.rs`, `loop_block.rs`, `agent_router.rs`,
`builtins.rs`). Dziś większość to **hardcoded `const`**. Cel: każdy prompt = pole config bloku
(z domyślną wartością = dzisiejszy const, by zero regresji).

| Prompt | Plik / linia | Dziś | Docelowy blok → pole |
|---|---|---|---|
| Agent system prompt | agent_context.rs:218 (DB `agents.system_prompt`) | DB | ✅ już edytowalne (agent) |
| Skills index template `<available_skills>` | agent_context.rs:87-104 | const | 🔧 `agent_context.skills_template` |
| Anti-injection note | agent_context.rs:25-27 | const | 🔧 `agent_context.anti_injection_note` |
| Delegated-results template `<delegated_results>` | agent_context.rs:136-149 | const | 🔧 `agent_context.delegated_results_template` |
| Compaction system prompt | compact_context.rs:76-84 | const | 🔧 `compact_context.summary_system_prompt` |
| Compaction update prompt | compact_context.rs:86-93 | const | 🔧 `compact_context.update_system_prompt` |
| Compaction prefix/suffix | compact_context.rs:99-105 | const | 🔧 `compact_context.summary_prefix/suffix` |
| Nudge: empty-after-tools | loop_block.rs:57-59 | const | 🔧 opcjonalne `loop.nudge_*` (domyślnie OFF — strukturalny stop je zastępuje) |
| Nudge: intermediate-ack | loop_block.rs:62-64 | const | 🔧 jw. |
| LLM inline system prompt | llm.rs:86-92 | config | ✅ już edytowalne (`llm.system_prompt`) |
| Router system prompt | agent_router.rs:40-44 | const | 🔧 `agent_router.system_prompt` |
| General agent system prompt | seed.rs:927 (DB) | DB | ✅ edytowalne |
| Core tool descriptions (spawn/wait/list/cancel/ask_user/skill_view) | builtins.rs:127-265 | const | 🔧 opcjonalny override per-flow w `tool_exec.tool_overrides` (model widzi te opisy → to też prompt) |
| Transcription summarization (PL/EN/DE/ES/FR) | seed.rs:628-721 (tabela `prompts`) | DB | ✅ edytowalne (prompt registry) |

Mechanika: każde 🔧 pole czyta `node.config.<field>`; brak/empty → dzisiejszy const jako default.
Sanityzacja anty-injection (ZWJ-defuse delimiterów) zostaje **niezależnie** od treści użytkownika.

---

## CZĘŚĆ 5 — Docelowy flow na blokach (jeden flow)

Patrz diagram `docs/target-agent-flow.svg` / `.mmd`. Skrót:

**Główny „Agent Run" (jeden flow, zastępuje …011/…012/…013):**
```
trigger
 → conversation_history        (wczytaj wcześniejsze tury)
 → agent_context               (system prompt + skills + mailbox-drain + harness_tools + budżet)
 → ┌─ LOOP REGION "agent turn" ──────────────────────────────────────────┐
   │  compact_context          (gdy ctx > próg)                            │
   │   → llm(tools)            (system_prompt edytowalny; pick_tools)       │
   │   → tool_exec             (core.* + addon; ask_user/spawn/wait/list)   │
   │   → (loop_back do compact_context)   stop: ostatni assistant bez tool_calls │
   └───────────────────────────────────────────────────────────────────────┘
 → output                      (stream finalnej iteracji)
```

**Reaktywny wariant tła (pokazany obok):**
```
on_subagent_complete (🆕 event trigger)
 → (payload = wynik z mailbox)
 → llm/tool_exec lub output    (przetwórz wynik / powiadom)
```

**Deterministyczne tło (alternatywa do tool-driven):**
```
... → spawn(🆕, detach)        (launch N subagentów w tle, run_ids→vars)
   → ┌─ LOOP REGION "watch" ──────────────┐
     │  subagent_status(🆕) → interval(🆕) │  stop: wszystkie dzieci terminal
     └─────────────────────────────────────┘
   → await_subagents(🆕, mode=all) → combine → output
```

---

## CZĘŚĆ 6 — UI Flow Buildera (canvas)

- 🔧 **Rysowanie regionu**: wizualny kontener (box) grupujący węzły ciała; przeciągnięcie węzłów do
  boxa nadaje `region_id`. (`www/js/modules/flows-builder/canvas.js`).
- 🔧 **Krawędź wsteczna**: pozwól narysować edge z wyjścia regionu do wejścia regionu; oznacz wizualnie
  (przerywana, „loop") i serializuj jako `kind: "loop_back"`.
- 🔧 **Walidacja klienta**: dziś canvas odrzuca cykle — wyjątek dla krawędzi `loop_back` w obrębie
  regionu (lustro R11).
- 🔧 **Palette**: nowe szablony `spawn`, `await_subagents`, `subagent_status`, `interval`,
  `on_subagent_complete` (drugi trigger) — z `flowNodeTemplatesListRequest`.
- 🔧 **Config panel**: pola promptów z Części 4 jako `textarea` (format jak `llm.system_prompt`).
- 🔧 **Progres**: render `IterationStarted/Finished`, `ChildSpawned/Finished`, `UserQuestion`,
  `PermissionRequest` w obrębie regionu/węzła (już są w `ProgressEvent`).

---

## CZĘŚĆ 7 — Kolejność implementacji (fazy)

1. **Compile + executor: region inline** (rdzeń, bez nowych bloków) — `cache.rs` regiony +
   `executor.rs` `run_loop_region` + R4/R11 + testy jednostkowe (counter-body jak loop_block.rs:761).
2. **Migracja seed → jeden flow** (`seed.rs`/`migrations.rs`) + przepięcie `agent.flow_id`; strukturalny
   stop zamiast `harness_done`; usuń heurystyki nudge.
3. **Prompty do configu bloków** (Część 4) — zero regresji (defaulty = consty).
4. **Bloki tła/subagentów**: `spawn`, `await_subagents`, `subagent_status`, `interval`,
   `on_subagent_complete` (wrappery na istniejący `run_manager`/`interaction`/broker progresu).
5. **UI canvas**: region box + loop_back edge + palette + config promptów + progres.
6. **E2E**: jeden flow z pętlą inline + ask_user w regionie + spawn w tle + completion trigger.

Każda faza: `cargo check`/`clippy` bez warningów, realne testy (CLAUDE.md), zero TODO/stubów.

---

## CZĘŚĆ 8 — Trwałość produkcyjna (NIE MVP) — każdy krok musi realnie persystować

Audyt warstwy storage ujawnił **realną dziurę**: historia konwersacji żyje TYLKO w pamięci i jest
niekompletna. To musi zostać przepisane, zanim harness będzie produkcyjny. Stan faktyczny:

| Dane | Gdzie dziś | Trwałe? | Problem |
|---|---|---|---|
| Historia konwersacji | `ConversationCache` = `RwLock<HashMap>` (`services/runtime/quic_handle.rs:65`), owinięte przez `conversation_impl.rs` | ❌ tylko RAM | ginie po restarcie; tylko `role+content` (gubi `tool_calls`, multimodal); komentarz w kodzie: „awaiting storage rewrite" |
| Zapis tury | `conversation_history` node zapisuje **tylko user msg** (conversation_history.rs:95-101) | ❌ | assistant + tool results NIGDY nie wracają do store'u |
| flow_executions (trace) | SQLite (`executor.rs:292` `persist_execution`) | ✅ | OK, ale to trace nie wiadomości |
| audit_log (hash-chain) | SQLite (`audit_impl.rs`) | ✅ | OK |
| agent_runs + run_log | SQLite (`append_agent_run_log`) | ✅ | strukturalne eventy, nie pełne wiadomości do replay |
| bloby (audio/obraz) | `FileBlobStore` (SHA256, dysk) | ✅ | OK |
| memory (wektory) | usługa QUIC `memory-engine` | ✅ | OK |

### 8.1 Nowa trwała tabela konwersacji (wymóg)

`migrations.rs` — nowa tabela, źródło prawdy dla historii (cache staje się read-through, nie storage):

```sql
CREATE TABLE conversation_messages (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id    TEXT NOT NULL,
    seq           INTEGER NOT NULL,         -- monotoniczny porządek w sesji
    role          TEXT NOT NULL CHECK(role IN ('system','user','assistant','tool')),
    content       TEXT,                     -- treść tekstowa (lub NULL dla czysto multimodal)
    tool_calls    TEXT,                     -- JSON Vec<LlmToolCall> dla assistant; NULL inaczej
    tool_call_id  TEXT,                     -- dla role=tool: powiązanie z wywołaniem
    name          TEXT,                     -- nazwa narzędzia (role=tool)
    payload_ref   TEXT,                     -- blob_ref dla multimodal (audio/obraz), NULL inaczej
    payload_kind  TEXT,                     -- 'text'|'audio'|'image'|... gdy payload_ref
    node_id       TEXT,                     -- proweniencja: który węzeł wyprodukował
    created_at    TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(session_id, seq)
);
CREATE INDEX idx_conv_msgs_session ON conversation_messages(session_id, seq);
```

**Kluczowe:** `tool_calls`/`tool_call_id`/`name` muszą round-trippować (dziś cache je gubi), inaczej
łańcuch assistant→tool jest po stronie modelu niespójny w kolejnej turze.

### 8.2 `ConversationHistoryStore` → trwały (SQLite source-of-truth + cache read-through)

- `recent(session_id, limit)`: czyta z `conversation_messages` (ostatnie N wg `seq`), z cache jako
  warstwą szybkiego odczytu (write-through). Mapuje pełny `ChatMessage` (z `tool_calls` itd.), nie
  okrojony `role+content`.
- `append(session_id, message)`: transakcyjny INSERT z `seq = MAX(seq)+1` per sesja + aktualizacja cache.
- Nowość: `append_batch(session_id, &[ChatMessage])` — atomowy zapis delty całej tury.

### 8.3 Gdzie persystuje pojedyncza tura (w modelu jeden-flow)

Wzorzec jak w codeksie (embedder persystuje historię Session), ale w grafie jawnie:

1. **Wejście tury**: `conversation_history` (read-mode) ładuje trwałą historię → `envelope.context.messages`
   i zapamiętuje **indeks granicy** (`meta.history_base_len`) — ile wiadomości było „z bazy".
2. **Region pętli**: akumuluje nowe wiadomości (user→assistant+tool_calls→tool results) w TYM SAMYM
   envelope — już dziś poprawne (`current = next`), zostaje inline.
3. **Wyjście tury — NOWY krok `persist_turn`** (lub `conversation_history` write-mode): zapisuje
   **deltę** `messages[history_base_len..]` przez `append_batch` durably, z multimodalem do `BlobStore`
   i `payload_ref`. To jest moment, którego dziś **brakuje** (zapisywany jest tylko user msg).
4. **Streaming**: persist musi nastąpić w `finalize_streaming_flow` (`executor.rs`) PO domknięciu
   streamu (asystent doklejany w finalizerze, nie w `produce_stream`), żeby delta była kompletna zanim
   przyjdzie następny request. Koordynacja z `resume_token` dla in-flight.

### 8.4 Granice flow a historia (już dziś poprawne — zachować)

- `agent_block.rs:188` celowo **NIE** wynosi konwersacji dziecka do rodzica (wzorzec Codex-review):
  subagent zwraca tylko podsumowanie. To zostaje — subagent persystuje SWOJĄ historię pod swoim
  `agent_run_id`/sesją, a do rodzica wraca payload + `agent_run_id` (rodzic może podlinkować).
- `agent_context` klonuje envelope i tylko **dodaje** system prompts/meta (nie resetuje messages) —
  zostaje.

### 8.5 Spójność i odzysk
- **Restart**: trwała tabela + `agent_runs.run_log` pozwalają odtworzyć stan; `reap_interrupted_on_startup`
  (run_manager.rs:235) domyka osierocone runy; mailbox przeżywa (idempotentny pull).
- **Idempotencja**: `persist_turn` zapisuje deltę raz (granica `history_base_len` + `UNIQUE(session_id,seq)`),
  retry nie duplikuje.
- **Sync**: konwersacja to dane runtime (jak `flow_executions`) — per `sync/core_registry.rs` NIE jest
  syncowana między węzłami; zostaje lokalnie trwała. (Zgodne z CLAUDE.md: runtime tables not synced.)

---

## CZĘŚĆ 9 — Produkcyjna obsługa flow i ich budowy

Nie tylko wykonanie — sama **budowa/zapis/wersjonowanie** flow musi być produkcyjna.

- **Walidacja na zapisie**: `dispatch/handlers.rs` `flowUpdateRequest` uruchamia R1–R11 (z nową R11
  integralności regionu) PRZED zapisem; błąd = odrzucenie z czytelnym komunikatem, nie zepsuty flow w DB.
- **Atomowość zapisu**: zapis `flow_json` + walidacja w jednej transakcji; aktywacja flow tylko gdy
  kompiluje się (`CompiledFlow::from_json`) — nieaktywne/niekompilowalne nie trafiają na ścieżkę resolvera.
- **Wersjonowanie**: zachować historię definicji (kolumna/tabela wersji flow) — git-like, żeby zmiana
  harnessu agenta nie kasowała działającej wersji w locie; rollback możliwy.
- **Migracja 3→1**: idempotentna migracja seedów `…011/…012/…013` → jeden flow; stare zostają jako
  legacy/read-only do potwierdzenia; `agent.flow_id` przepięte transakcyjnie.
- **Kompatybilność resolvera**: `FlowDispatcher` resolwuje jeden flow per `{model}:{service_type}:{modality}`;
  synthetic fallback (`synthetic.rs`) bez zmian dla nie-agentowych ścieżek.
- **Autosave + recompile**: autosave (10 s, flows-builder.js:272) musi walidować klient-side (lustro
  R11, w tym `loop_back`) i nie zapisywać grafu, który serwer odrzuci.
- **Obserwowalność**: każdy run pisze `flow_executions` (trace), `audit_log` (hash-chain), a agentowy
  dodatkowo `agent_runs`/`run_log` + trwałą konwersację — pełny ślad do debugowania produkcyjnego.
