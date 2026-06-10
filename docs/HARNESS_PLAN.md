# TentaFlow Harness — projekt

Status: PROJEKT DO REVIEW (nic z tego dokumentu nie jest jeszcze zaimplementowane).
Źródła analizy: `thirparty/codex` (OpenAI Codex CLI, Rust) i `hermes-agent` (Nous Research, Python)
— oba przeczytane na poziomie kodu; odwołania do plików w sekcji 1.

---

## 0. Cel

Zbudować w TentaFlow harness agentowy (pętla model → narzędzia → model, długie zadania,
agenci w tle, skille) **wyłącznie z bloków Flow Buildera** — bez osobnego "trybu agenta"
poza silnikiem flow. Do tego dwie nowe sekcje dashboardu: **Agenci** i **Skills**.

Decyzje użytkownika (wiążące):
1. Skille = wyłącznie instrukcje (markdown + referencje). Żadnych wykonywalnych skryptów.
   Addony są nośnikiem "wykonywalnej" części — addon może mieć własny skill
   (już przygotowane: kolumna `addons.skill_md`, `addon/lifecycle.rs:423`, bundlowane
   `addons/memory/SKILL.md`, `addons/embeddings-chunker/SKILL.md` — dziś nieskonsumowane).
2. Tool-calling musi być uniwersalny: toole pochodzą z addonów (deklaracje `[[tool]]`
   z opisami), docelowo własny model trenowany pod nasz format, ale każdy obcy model
   też musi umieć wywoływać narzędzia.
3. Flow "TentaFlow Harness" powstaje jako zwykły flow, **nigdzie nie podpięty**. Później
   zostanie wpięty do "Default Chat" przez nowy blok **Sub Flow** (blok ogólnego
   przeznaczenia — komponowanie flow z flow).
4. **Harness ma być modyfikowalny z poziomu FlowBuildera, nie zaszyty w kodzie**:
   pętla jako blok `loop`, a logika każdej iteracji (LLM, wykonanie tooli,
   kompakcja) jako osobne bloki w edytowalnym flow-ciele pętli. Żadnego
   monolitycznego "bloku agenta" z harnessem w Rust.
5. **Jeden spójny plan, całość ma być zrobiona** (etapami, ale bez "później
   się zobaczy"): w zakres wchodzą też porządne zmiany w silniku (bramkowanie
   gałęzi `condition`, uogólnienie producenta strumienia, zdarzenia postępu —
   §3.11), blok `map` (dynamiczna równoległość), batch-spawn, mailbox
   i auto-kontynuacja przebiegów w tle (§3.6).
6. **Czat pokazuje, co dzieje się w tle** (chat.js / chat-audio.js): prosty,
   estetyczny, kompaktowy widżet aktywności z możliwością drill-in (§3.9).
7. **Zmienne flow + język wyrażeń** (Camunda-style io-mapping, semantyka
   "zmienne płyną z envelope + merge na combine" — §3.12). Plan ma zawierać
   wszystko, co potrzebne, by w przyszłości **importować procesy BPMN** do
   naszych flow (mapa drogowa: §6); nie budujemy drugiego silnika.
8. **Pytania do użytkownika w trakcie wykonania** (§3.13): doprecyzowania
   (clarify), brakujące dane oraz zgody na dodatkowe uprawnienia — agent
   pyta, czeka na odpowiedź, kontynuuje.
9. Tryb pracy: projekt → review → implementacja.

---

## 1. Wnioski z analizy Codex i Hermes (co kopiujemy, co odrzucamy)

### 1.1 Pętla agenta i wykrywanie końca zadania

**Codex** (`core/src/session/turn.rs`, `core/src/tasks/regular.rs`): hierarchia
Session → Task → Turn → Sampling Request. Wewnętrzna pętla per turn: wyślij całą historię →
odbierz strumień → każdy tool call ustawia `needs_follow_up=true`, wynik trafia do historii →
kolejny sampling request. **Koniec zadania = odpowiedź asystenta bez tool calli**
(plus opcjonalny sygnał serwera `end_turn`). Brak "tokenu zakończenia".

**Hermes** (`agent/conversation_loop.py:461`): `while api_call_count < max_iterations and
budget.remaining > 0`. Identyczna zasada: tool calls → wykonaj → kontynuuj; brak tool calli →
finalna odpowiedź → koniec. Dodatkowo drabinka zabezpieczeń przed fałszywym końcem:
nudge po pustej odpowiedzi po toolach, auto-kontynuacja po "I'll do X..." bez akcji
(regex), kontynuacja po `finish_reason=length`, retry pustych odpowiedzi.

**Budżet iteracji** (Hermes): domyślnie 90 iteracji na turę (subagent: 50, niezależny).
Po wyczerpaniu — **jedno dodatkowe wywołanie bez narzędzi** z prośbą o podsumowanie
("grace summary", `agent/chat_completion_helpers.py:1297`). Codex nie limituje iteracji
twardo — limituje kontekst (auto-kompakcja).

**Przejmujemy**: koniec = brak tool calli; budżet iteracji per agent z grace summary;
`_turn_exit_reason` (każde wyjście z pętli ma nazwany powód — diagnostyka).
**Odrzucamy (v1)**: drabinkę nudge'ów Hermesa (dodamy w hardening, gdy zobaczymy realne
zachowania modeli), hooki stop/continuation Codexa.

### 1.2 Długie zadania: kompakcja kontekstu

**Codex** (`core/src/compact.rs`): trzy punkty wyzwolenia (pre-turn, mid-turn, zmiana
modelu); próg z `auto_compact_token_limit` modelu. Kompakcja = osobne wywołanie LLM
z promptem "CONTEXT CHECKPOINT COMPACTION → handoff summary dla innego LLM, co zrobiono,
co zostało"; historia **zastępowana** podsumowaniem + ostatnie wiadomości użytkownika
(do 20k tokenów).

**Hermes** (`agent/context_compressor.py`): próg 50% okna kontekstu; najpierw faza bez LLM
(stare wyniki tooli → jednolinijkowe streszczenia, dedup), potem LLM streszcza środek do
ustrukturyzowanego szablonu (`## Active Task`, `## Completed Actions`, `## Remaining Work`,
`## Key Decisions`...). Kluczowe triki: ostatnia wiadomość użytkownika ZAWSZE zostaje
w żywym ogonie; "temporal anchoring" (zrobione rzeczy zapisane w czasie przeszłym, żeby
model ich nie powtarzał); iteracyjna aktualizacja poprzedniego podsumowania zamiast
streszczania od zera; anty-thrashing (2 kompresje <10% oszczędności → wyłącz auto).

**Przejmujemy**: dwufazową kompakcję Hermesa (tania faza bez LLM + szablonowe streszczenie
przez LLM), próg % okna kontekstu, kotwiczenie ostatniej wiadomości użytkownika.
Wywołanie streszczające idzie przez `AiGateway` (audyt).

### 1.3 Wybór narzędzi

**Codex** (`core/src/tools/spec_plan.rs`): lista narzędzi budowana **per sampling request**
z `TurnContext` — rejestr trait-objectów keyed by name, spec i handler to ten sam obiekt.
Duże zestawy MCP (≥100) przechodzą w tryb **deferred** + narzędzie `tool_search` (BM25)
— model doszukuje narzędzia w trakcie. Wyniki tooli przycinane polityką
bytes/tokens (middle-out truncation) zanim wrócą do modelu.

**Hermes** (`toolsets.py`, `model_tools.py`, `tools/tool_search.py`): statyczne toolsety
per platforma + dynamiczne MCP; gdy odroczone schematy przekroczą 10% okna kontekstu
(lub 20k tokenów), długi ogon znika za trzema bridge-toolami `tool_search` /
`tool_describe` / `tool_call` (BM25 po nazwach+opisach). Higiena: schemat narzędzia
przebudowywany tak, by nie wspominał o niedostępnych narzędziach; sanityzacja schematów
pod llama.cpp GBNF; koercja argumentów (string→int/bool, JSON-string→array) dla modeli
open-weight; `[TOOL_ERROR]`-prefiksowane, przycięte komunikaty błędów.

**Przejmujemy**: rejestr = istniejący `ToolDispatcher` (`addon/tool_dispatch.rs` — gotowy,
nieużywany); allowlistę narzędzi per agent (to nasz główny mechanizm kontroli powierzchni
— twórca agenta wybiera narzędzia, więc problem "200 tooli" nie występuje w v1);
truncation wyników; koercję argumentów; sanityzację błędów.
**Odkładamy**: `tool_search` (sensowny dopiero przy agentach "z dostępem do wszystkiego").

### 1.4 Agenci w tle i "wracanie do nich"

**Codex** (`core/src/agent/control/spawn.rs`, `tools/handlers/multi_agents.rs`): child
agent = pełny wątek Codexa (własna sesja, własny kontekst, własny rollout). Narzędzia
modelu: `spawn_agent`, `send_input`, `wait_agent`, `close_agent`, `list_agents`. Statusy
dzieci przez `watch::Receiver<AgentStatus>`; `wait_agent` czeka na kanale (bez pollingu);
wynik = finalna wiadomość dziecka w `AgentStatus::Completed`. Wariant V2: mailbox —
dziecko po skończeniu wrzuca wiadomość do skrzynki rodzica; jeśli rodzic bezczynny,
mail **sam wyzwala nową turę** rodzica. Limity: głębokość spawn, cap równoległości.
Background terminale: procesy w PTY przeżywają turę, model wraca do nich
`write_stdin(session_id, "")` = poll.

**Hermes** (`tools/delegate_tool.py`): `delegate_task` jest **synchroniczne** — rodzic
blokuje do końca dzieci (batch przez ThreadPool, poll co 0.5 s na wypadek interruptu).
Trwałe tło robią: **cron** (`cron/jobs.py` — joby w JSON, at-most-once przez bump
`next_run_at` przed startem, dostarczanie wyników na platformy czatowe) i **kanban**
(SQLite: claim_lock, heartbeat, TTL, reclaim po crashu PID — przeżywa restarty).
Dzieci dziedziczą narzędzia rodzica **przeciętego** z żądanym zestawem (nigdy więcej
niż rodzic); zakazane dla dzieci: delegate, clarify, memory.

**Przejmujemy**: model Codexa V1 (spawn/wait/list przez watch-channel) jako narzędzia
agenta + rejestr przebiegów w SQLite (wzorzec kanbana: status, wynik, heartbeat) — UI
"Agenci" pokazuje przebiegi. Intersekcję narzędzi rodzic∩dziecko i limit głębokości
z Hermesa. Mailbox V2 (skrzynka wyników budząca rodzica) wchodzi w fazie 7 (§3.6).
**Odkładamy**: wznawianie przebiegów po restarcie procesu (po restarcie biegi
`running` oznaczamy `interrupted`; wznawianie = wzorzec rollout-reconstruction
Codexa, poza zakresem planu).

### 1.5 Skille

**Codex** (`core-skills/`): `SKILL.md` z frontmatter (name ≤64, description ≤1024);
ekspozycja = lista name+description+ścieżka w wiadomości developer (budżet 2% okna
kontekstu, degradacja: przycinanie opisów → pomijanie wg rangi); model sam doczytuje
treść (progressive disclosure); jawne wywołanie `$skill-name` wkleja całą treść do tury.

**Hermes** (`tools/skills_tool.py`, `tools/skills_hub.py`, `agent/curator.py`): trzy
poziomy — indeks w system prompcie (`<available_skills>`, nakaz "MUSISZ załadować
pasujący skill przez `skill_view`"), `skill_view(name)` ładuje pełny SKILL.md,
`skill_view(name, file_path)` doczytuje referencje. Kategorie = struktura katalogów
+ `DESCRIPTION.md`. Tagi w `metadata.hermes.tags`. **Skills Hub**: adaptery źródeł
(GitHub taps: openai/skills, anthropics/skills...; skills.sh; URL; well-known;
centralny indeks JSON budowany w CI), kwarantanna → skan bezpieczeństwa
(macierz trust×verdict) → potwierdzenie → instalacja z provenance w `lock.json`
+ audit log. **Curator** (to jest mechanizm grupowania, o który pytał użytkownik —
"doctor" w Hermes to diagnostyka środowiska, nie skille): cykliczny przegląd
(domyślnie co 7 dni, na idle) — LLM buduje "umbrelle": klastruje podobne skille,
scala je w skill-parasol, wchłonięte degraduje do `references/`, nieużywane
archiwizuje (nigdy nie kasuje); snapshot+rollback przed każdym przebiegiem;
raport dla użytkownika.

**Przejmujemy**: format SKILL.md z frontmatter (name/description/tags), trzy poziomy
ekspozycji (indeks → `skill_view` → referencje), hub z importem z GitHub/URL +
kwarantanną (uproszczoną: import ląduje jako wyłączony do zatwierdzenia przez admina,
skan wzorców injection), kuratora jako tryb **raport + zatwierdzenie** (nie autonomiczna
mutacja). **Odrzucamy**: scripts/ (decyzja użytkownika), automatyczne self-tworzenie
skilli przez agenta w tle (Hermes background_review) — do rozważenia później.

### 1.6 Sterowanie w trakcie i przerwania

Codex: steering — nowy input użytkownika dokleja się do **biegnącej** tury (kolejka
pending input drenowana na początku każdego sampling requestu). Hermes: `/steer` doklejany
do ostatniego wyniku toola w specjalnym markerze (jedyny zaufany kanał mid-turn —
anty-injection). Obaj: twarde przerwanie = cancellation token + synteza wyników
"cancelled" dla wiszących tool calli, żeby para call/result została poprawna.

**Przejmujemy w v1 tylko twarde anulowanie** (mamy `CancellationToken` w
`ExecutionContext` — pętla agenta sprawdza go przed każdą iteracją i każdym toolem).
Steering mid-turn wymaga kanału klient→biegnący flow, którego nie mamy — odłożone.

### 1.7 Pytania do użytkownika i zgody

**Hermes** (`tools/clarify_tool.py`, `tools/clarify_gateway.py`): narzędzie
`clarify` — pytanie + ≤4 opcje wyboru (UI dokleja piątą "Other: wpisz własną"),
albo pytanie otwarte. Agent działa na wątku roboczym, więc clarify to blokująca
kolejka eventowa: `wait_for_response` czeka w 1-sekundowych plasterkach,
**dotykając przy każdym heartbeat aktywności** (watchdog bezczynności nie ubija
agenta czekającego na człowieka). Timeout domyślnie 600 s → narzędzie zwraca
sentinel `"[user did not respond within Nm]"` — model dostaje to jako wynik
i ADAPTUJE SIĘ zamiast wisieć. Odpowiedź wraca jako JSON
`{question, choices_offered, user_response}`. Wytyczna w schemacie: clarify
służy do niejednoznaczności/trade-offów, NIE do potwierdzania niebezpiecznych
komend (od tego osobny approval-callback). Subagenci mają clarify ZABLOKOWANE
(`clarify_callback=None`) — pytać może tylko góra drzewa.

**Codex**: dwa rozłączne mechanizmy. (a) **Approvals** — `ToolOrchestrator`
(approval → sandbox → eskalacja): `EventMsg::ExecApprovalRequest` + oneshot
`ReviewDecision`; decyzja `ApprovedForSession` jest cache'owana per klucz
zgody (`ApprovalStore`), więc "zezwól na zawsze w tej sesji" nie pyta drugi
raz. Approvals **dzieci bąbelkują do rodzica**: pompa `forward_events`
delegata przechwytuje żądania zgód sub-agenta i odpowiada przez sesję
rodzica — dziecko nigdy nie pokazuje własnego UI. (b) **Pytania** — narzędzia
`request_user_input` (flaga eksperymentalna) i `request_permissions`
(Feature-gate) jako zwykłe toole modelu.

**Przejmujemy**: narzędzie pytające z opcjami + timeout-sentinel + heartbeat
podczas czekania (Hermes), flow zgód z decyzją raz/na-zawsze i cache (Codex),
bąbelkowanie pytań i zgód dzieci do tego samego użytkownika (Codex), rozdział
"pytanie o doprecyzowanie" od "zgoda na uprawnienie" (oba). Projekt: §3.13.

---

## 2. Stan TentaFlow — fakty wiążące projekt

1. **Silnik flow jest ściśle acykliczny** (Kahn w `flow_engine/cache.rs:210`; cykl =
   `CompileError::Cycle`). Każdy node wykonuje się dokładnie raz. **Jedyna forma pętli
   zgodna z silnikiem: pętla wewnątrz `execute` adaptera.** Pętle krawędziami w grafie
   wymagałyby przepisania `executor.rs` + `cache.rs` + `validation.rs` (re-armowanie
   zależności, sloty wyjść per iteracja, trace per iteracja) — odrzucone. Zamiast tego:
   generyczny blok `loop`, którego **ciałem jest inny flow** — mechanika iteracji
   żyje w adapterze, ale treść każdej iteracji jest w 100% edytowalna
   w FlowBuilderze (§3.4).
2. **Nie istnieje żadna pętla tool-callingowa.** `ToolDispatcher`
   (`addon/tool_dispatch.rs`) jest kompletny (mapowanie `addon_id.tool` → `call_tool`,
   filtrowanie po uprawnieniach, format wiadomości `role=tool`) i ma **zero wywołań**.
3. `LlmRequest` (`flow_engine/dispatchers/llm.rs`) nie ma pola `tools`;
   `build_chat_request` (`dispatchers_impl/llm_impl.rs:255`) wpisuje `tools: None`;
   `GenerateParams` silników natywnych (llama.cpp/MLX) przyjmuje płaski prompt.
   Backendy OpenAI-compatible serializują **cały** `ChatCompletionRequest`
   (`services/backend/client.rs:297`) — przepuszczą `tools` od ręki.
4. `ChatMessage` w envelope ma już `ChatRole::Tool` i `tool_call_id`; `LlmStreamChunk`
   ma `tool_calls: Vec<ToolCallDelta>`; `FinishReason::ToolCalls` round-tripuje;
   `compliance_ai_tool_calls` + zapis w `AiGateway` istnieją. Brakuje: `tool_calls`
   na wiadomości asystenta, `tools` w żądaniu, pętli.
5. **Timeout flow**: `run_blocking` ma twarde `FLOW_TIMEOUT_SECS = 120`
   (`dispatcher.rs:48`) — pętla agenta tego nie przeżyje; potrzebny per-flow override.
6. **Streaming**: producentem strumienia może być wyłącznie node ze slotu
   `registry.llm()` (`executor.rs:357`). Blok `agent` w v1 jest blocking
   (`wrap_blocking_as_stream` opakuje go w 1 chunk); uogólnienie producenta strumienia
   to osobna faza.
7. **Nowy blok core = 4 kroki** (adapter w `node_adapters/` + rejestracja
   w `build_registry()` (`dispatcher.rs:719`) + seed `flow_node_templates`
   (`db/seed.rs:202`; paleta backend-owned, prune na starcie!) + i18n
   `flows.node_names.*` w 5 plikach) — ALE `build_registry()` jest celowo
   bezargumentowe i woła się zanim istnieje `AddonManager`. Adaptery z zależnościami
   mają w kodzie dwa wzorce: late-bound resolver (`set_addon_resolver`,
   `dispatcher.rs:239`) i slot (`ModelRuntimeSlot` = `RwLock<Option<Arc<...>>>`,
   `llm_impl.rs:32`). Nowe bloki §3.5 używają wzorca slotu (§3.5.0).
   Paleta i porty idą istniejącym handlerem `flow_node_templates_list` —
   zero zmian w protokole.
8. **Nowa domena protokołu = 1 wariant `MessageBody`** opakowujący enum payloadu
   (limit 256 wariantów CBOR; wzorzec `SchedulerBody(SchedulerPayload)`):
   message_body.rs → `variant_name_of` → handlery `#[handler]`/`#[policy]` →
   enkodery/dekoder w `tentaflow-protocol-wasm` → helpery w `codec.js` → moduł UI.
9. **Menu**: `ADMIN_NAV`/`USER_NAV` w `www/js/app.js` + `Router.register` + i18n.
   Gating ról wyłącznie po stronie serwera (`#[policy]`).
10. **Addony już deklarują skille** (`addons.skill_md`, czytane przy instalacji,
    synchronizowane przez ledger) i **toole z opisami** (`[[tool]]` w manifest.toml →
    `ToolDefinition` z `parameters_schema`, `keywords`). Uwaga: host fn
    `flow_invoke_v1` (`addon/host_functions/flow.rs:373`) odpala DAG-i operatorowe
    z manifestu addonu przez **osobny** podsystem `flow_runtime` — to NIE jest
    precedens dla Sub Flow (flow Flow Buildera wykonuje `flow_engine`); blok
    `subflow` buduje własną ścieżkę.
11. **AiGateway żyje wyłącznie na warstwie routingu** (`routing/chat.rs:113`,
    `routing/streaming.rs`, host fn `llm_generate`) — `ctx.llm` w silniku flow
    (`LlmDispatcherImpl`) woła `ModelRuntimeExecutor` bezpośrednio, bez compliance.
    Jedno żądanie czatu = jeden event compliance obejmujący cały flow. Pętla agenta
    wołająca `ctx.llm` N razy NIE wygeneruje eventów per wywołanie — rozwiązanie:
    gateway-aware `LlmDispatcherImpl` (§3.4), audyt per wywołanie dla każdego
    node'a `llm` w każdym flow.
12. **`ToolDispatcher`/`call_tool` są synchroniczne** (wywołanie wasmtime) i wymagają
    konkretnego `user_id` (`tool_dispatch.rs:78`, gate `permission_checker.check(...,
    user_id, "llm", ...)`; admin ma bypass, brak konfiguracji = deny). Wzorzec
    wywołania z async: `tokio::task::spawn_blocking` (tak robi `AddonNodeAdapter`,
    `node_adapters/addon.rs:147`).
13. Bundlowane SKILL.md używają nigdzie niezaimplementowanej notacji "TOON"
    (`@memory.memory_store|layer=user|fact=...`) — do aktualizacji na format z §3.1.
14. **Blok `condition` NIE bramkuje wykonania**: zapisuje tylko
    `meta.condition_result` (`node_adapters/condition.rs:75`), a executor odpala
    WSZYSTKICH następników bezwarunkowo (`spawn_node`/`build_inputs` nie czytają
    portów `true`/`false`; nagłówek pliku twierdzi co innego niż robi kod).
    `FlowEdge.condition` jest deserializowane i nigdy nie czytane. Naprawa
    (skip-semantyka) w §3.11.

---

## 3. Projekt

### 3.1 Fundament: uniwersalny tool-calling

Jeden kanoniczny opis narzędzia (już istnieje): `ToolDefinition`
{`addon_id`, `tool_name`, `description`, `parameters_schema`, `keywords`} +
narzędzia wbudowane core (§3.5). Nazwa publiczna: `"{addon_id}.{tool_name}"`
(np. `memory.memory_store`) — dokładnie jak parsuje to `ToolDispatcher`.

**Dwa tryby dostarczenia narzędzi do modelu** — `native` | `prompt`:

- `native` — `tools`/`tool_choice` w `ChatCompletionRequest` (przechodzi przez
  `services/backend/client.rs` bez zmian), odpowiedź: `message.tool_calls`.
- `prompt` — core renderuje sekcję narzędzi do system promptu (zwarta lista:
  nazwa, opis, parametry z typami) i wymaga odpowiedzi w formacie:

  ```
  <tool_call>{"name":"memory.memory_store","arguments":{"fact":"...","layer":"user"}}</tool_call>
  ```

  Parser w core (tolerancyjny: wiele `<tool_call>` w jednej odpowiedzi, fenced
  ```json, koercja typów argumentów wg schematu — wzorzec Hermes
  `coerce_tool_args`). Działa z **każdym** backendem, w tym llama.cpp/MLX —
  i to jest format, pod który użytkownik wytrenuje własny model. Format ten jest
  zgodny z konwencją Hermes/Qwen, więc istniejące open-weight modele znają go
  z pudełka.

**Gdzie żyje rozgałęzienie trybu**: NIE w `dispatchers_impl/llm_impl.rs` —
realny backend obsługujący żądanie jest wybierany dopiero w
`ModelRuntimeExecutor::execute_chat` (`services/runtime/executor.rs:196` —
ranking kandydatów z fallbackiem między rodzajami backendów), więc decyzja
native/prompt zapada **per kandydat** wewnątrz runtime executora: kandydat
OpenAI-compatible → `tools` natywnie; kandydat natywny (llama.cpp/MLX) →
render `prompt` + parse odpowiedzi. Jawne nadpisanie `tool_call_mode` w configu
deploymentu modelu czyta runtime executor (ma dostęp do configów deploymentów).
`LlmDispatcherImpl` tylko przekazuje `tools` w dół — bez decyzji.

Zmiany w typach (wszystkie w miejscu, bez wariantów `_v2`):
- `LlmRequest` + `tools: Vec<ToolDefinitionWire>`, `tool_choice: Option<String>`;
  `LlmResponse` + `tool_calls: Vec<LlmToolCall>`.
- `ChatMessage` (envelope) + `tool_calls: Option<Vec<LlmToolCall>>` (asystent);
  `ChatRole::Tool` + `tool_call_id` już są.
- `build_chat_request` przekazuje `tools` zamiast `None`; mapowanie odpowiedzi
  wyciąga `message.tool_calls` zamiast je gubić (`dispatchers/llm.rs:56`,
  `llm_impl.rs:77-111`); mapper streamingowy `chat_chunk_to_llm_chunk`
  (`llm_impl.rs:354`) przestaje hardkodować `tool_calls: Vec::new()`.
- `AiGateway`: `compliance_ai_tool_calls` dostaje wpisy z **realnym wynikiem
  wykonania** (status success/failed, output hash) — dziś zapisuje tylko żądania
  modelu; `AiGatewayContext` + pola `agent_id`, `agent_run_id` (spinanie wielu
  `compliance_ai_events` jednego przebiegu agenta).

### 3.2 Rejestr Skills

**Tabela `skills`** (migracja + sync przez core_registry, polityka
`replicated_by_permission` jak flows):

```sql
CREATE TABLE skills (
    id TEXT PRIMARY KEY,                 -- UUID
    name TEXT NOT NULL,                  -- kebab-case, <=64; BEZ UNIQUE (jak flows.name —
                                         -- UNIQUE psułby apply synca przy zbieżnych nazwach
                                         -- z różnych node'ów); unikalność miękko w UI/handlerze
    display_name TEXT,
    description TEXT NOT NULL,           -- <=1024, widoczny dla LLM w indeksie
    content TEXT NOT NULL,               -- treść SKILL.md (markdown, bez frontmatter)
    tags_json TEXT NOT NULL DEFAULT '[]',
    category TEXT,
    source TEXT NOT NULL CHECK(source IN ('user','addon','hub')),
    source_ref TEXT,                     -- addon_id | URL/repo+ścieżka
    status TEXT NOT NULL DEFAULT 'active'
        CHECK(status IN ('active','disabled','quarantine','archived')),
    use_count INTEGER NOT NULL DEFAULT 0,
    last_used_at TEXT,
    created_by TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE skill_files (               -- referencje (tylko markdown/tekst)
    skill_id TEXT NOT NULL REFERENCES skills(id) ON DELETE CASCADE,
    path TEXT NOT NULL,                  -- np. "references/api.md"
    content TEXT NOT NULL,
    PRIMARY KEY (skill_id, path)
);
```

Limity jak w Hermes: name ≤64, description ≤1024, content ≤100k znaków,
plik ≤1 MiB, `path` tylko pod `references/` i `templates/` (zakaz `scripts/` —
decyzja użytkownika; import zawierający skrypty: skrypty odrzucane z ostrzeżeniem).

**Źródła skilli:**
1. `user` — tworzone/edytowane w UI (modal z polami frontmatter + edytor markdown).
2. `addon` — przy instalacji/aktualizacji addonu `addons.skill_md` jest
   materializowany do `skills` z **deterministycznym ID** (UUIDv5 z addon_id),
   żeby każdy node floty wygenerował identyczny wiersz i apply synca był
   idempotentny (tabela `addons` replikuje się fleet-wide — losowe UUID per node
   dałyby trwałe duplikaty). Treść skilla addonowego jest **read-only** w UI
   (aktualizacja addonu nadpisuje, odinstalowanie usuwa); edycja = przycisk
   "Forkuj jako własny skill" (kopia `source='user'`, niezależna). Kurator nie
   modyfikuje skilli addonowych (może najwyżej zaproponować wyłączenie).
   Frontmatter w SKILL.md addonu opcjonalny — fallback: name z addon_id,
   description z manifestu.
3. `hub` — import w locie (wzorzec Hermes Skills Hub, uproszczony):
   - źródła: repo GitHub (lista "tapów" w ustawieniach; domyślnie
     `anthropics/skills`, `openai/skills`), bezpośredni URL do SKILL.md;
   - pobieranie przez istniejący guard publicznych URL-i (`web_research/reader.rs`
     ma DNS-pinning/limit body — reużywamy ten sam mechanizm SSRF);
   - import ląduje ze statusem `quarantine` + skan wzorców injection (port listy
     z Hermes `_INJECTION_PATTERNS`); admin zatwierdza → `active`;
   - provenance w `source_ref` + wpis w `audit_log`.

**Tagi**: z frontmattera `metadata.tags` + edytowalne w UI; filtrowanie listy po tagach
i źródle. Kategoria = pole tekstowe (dla huba: segment ścieżki repo).

**Kurator** (mechanizm grupowania; w Hermes to `agent/curator.py` — "doctor" w Hermes
to co innego: diagnostyka instalacji): przycisk "Przegląd kuratora" + opcjonalny
harmonogram (interwał w ustawieniach). Przebieg: LLM (przez `AiGateway`, model
pomocniczy konfigurowalny) dostaje listę skilli (nazwa, opis, tagi, statystyki użycia)
i zwraca **propozycje**: klastry podobnych skilli → scalenie w skill-parasol,
duplikaty → merge, nieużywane >N dni → archiwizacja. Wynik = raport w UI z akcjami
do zatwierdzenia jeden klik (apply wykonuje mutacje + wpis audytowy). Żadnych
autonomicznych zmian, żadnego kasowania (najniżej `archived`).

**Protokół**: `MessageBody::SkillsBody(SkillsPayload)` —
`ListRequest/Response`, `DetailRequest/Response`, `UpsertRequest/Response`,
`DeleteRequest/Response`, `HubSearchRequest/Response`, `HubImportRequest/Response`,
`CuratorRunRequest/Response`, `CuratorApplyRequest/Response`.
Handlery `#[policy(Admin)]` (lista/detal może być `UserSession` — skille czyta też
Flow Builder).

### 3.3 Rejestr Agentów

**Tabela `agents`** (sync jak `skills`):

```sql
CREATE TABLE agents (
    id TEXT PRIMARY KEY,                 -- UUID (seedowane: stałe UUID, jak flows)
    name TEXT NOT NULL,                  -- kebab-case; BEZ UNIQUE (sync — jak flows.name)
    display_name TEXT,
    description TEXT NOT NULL,           -- kluczowe: na tej podstawie router LLM wybiera agenta
    system_prompt TEXT,
    model TEXT,                          -- NULL = model z requestu/envelope.meta
    tools_json TEXT NOT NULL DEFAULT '[]',   -- allowlista: "addon_id.tool" | "addon_id.*" | "core.skill_view" | "core.agent_spawn" ...
    skills_json TEXT NOT NULL DEFAULT '{}',  -- {"names":[...], "tags":[...]}
    params_json TEXT NOT NULL DEFAULT '{}',  -- temperature, top_p, max_tokens per wywołanie
    max_iterations INTEGER NOT NULL DEFAULT 25,
    timeout_secs INTEGER NOT NULL DEFAULT 600,
    max_subagents INTEGER NOT NULL DEFAULT 0,   -- 0 = nie może spawnować
    max_spawn_depth INTEGER NOT NULL DEFAULT 1,
    flow_id TEXT,                        -- flow harnessa agenta; NULL = seedowany "Agent Run"
                                         -- (różni agenci mogą mieć różne pętle — §3.4)
    routable INTEGER NOT NULL DEFAULT 1, -- 0 = wykluczony z auto-routingu (agent_router);
                                         -- nadal użyteczny jako jawny blok / cel agent_spawn
    is_enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
```

**Tabela `agent_runs`** (runtime, NIE synchronizowana — jak `flow_executions`):

```sql
CREATE TABLE agent_runs (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    parent_run_id TEXT,                  -- spawnowane przez innego agenta (łańcuch = głębokość spawnu)
    flow_execution_id INTEGER,           -- jeśli odpalony z flow
    user_id TEXT,                        -- pryncypał przebiegu (dziedziczony przez dzieci)
    org_id TEXT,                         -- atrybucja compliance (multi-tenant)
    status TEXT NOT NULL CHECK(status IN ('queued','running','waiting','waiting_user','completed','failed','cancelled','interrupted')),
    prompt TEXT NOT NULL,
    result TEXT,
    exit_reason TEXT,                    -- final_response | budget_exhausted | timeout | cancelled | error:<...>
    iterations INTEGER NOT NULL DEFAULT 0,
    total_tokens INTEGER NOT NULL DEFAULT 0,
    run_log TEXT,                        -- JSON: kroki pętli (model call / tool call / wynik skrócony)
    last_heartbeat_at TEXT,
    started_at TEXT, finished_at TEXT, created_at TEXT NOT NULL
);
```

**Model tożsamości (pryncypał przebiegu)** — każdy przebieg MA pryncypała:
- z flow: `ExecutionContext.user_id` (sesja czatu); dzieci (`agent_spawn`)
  **dziedziczą** `user_id`/`org_id` rodzica i zapisują je w swoim wierszu;
- `user_id = NULL` (np. wywołanie zewnętrzne bez sesji): narzędzia addonów są
  ODMAWIANE (istniejący `permission_checker`: brak konfiguracji = deny; to jest
  zachowanie domyślne, nie nowy mechanizm) — działają tylko narzędzia `core.*`;
- uprawnienia per tool call: `permission_checker.check(addon_id, user_id, "llm")`
  jak dziś w `call_tool` (uwaga: admin ma bypass — allowlista agenta pozostaje
  wtedy jedynym ogranicznikiem i dlatego jest obowiązkowa, nie opcjonalna);
- ACL widoków: admin widzi/anuluje wszystkie przebiegi; zwykły użytkownik tylko
  własne (`user_id` match) — egzekwowane w handlerach (`#[policy(UserSession)]`
  + filtr), nie w UI.

**Retencja `run_log`**: wyniki tooli bywają danymi osobowymi (CRM `contacts`,
`memory`), a skrócenie ≠ anonimizacja. `agent_runs` dostaje politykę retencji
w `compliance_retention_policies` (nowa kategoria `agent_runs`, domyślnie 30 dni,
purge w istniejącym cyklu retencji: po terminie `run_log`/`prompt`/`result`
są czyszczone, wiersz statystyczny zostaje). Bez tego powstałby niegovernowany
magazyn PII obok w pełni governowanego compliance.

**Protokół**: `MessageBody::AgentsBody(AgentsPayload)` — CRUD agentów +
`RunsListRequest/Response`, `RunDetailRequest/Response`, `RunCancelRequest/Response`,
`ToolsCatalogRequest/Response` (katalog dostępnych narzędzi do pickera: z
`AddonManager::list_tools()` + wbudowane core).

**UI „Agenci"**: lista agentów (nazwa, opis, model, liczba narzędzi/skilli, włączony);
edytor: opis (z podpowiedzią, że to baza decyzji routera), system prompt, model
(dynamic_enum `models`), picker narzędzi pogrupowany po addonach (checkboxy,
wildcard per addon), picker skilli (po nazwach + po tagach), limity; zakładka
„Przebiegi": aktywne i historyczne `agent_runs` z podglądem `run_log` i przyciskiem
anulowania.

### 3.4 Harness jako flow — pętla złożona z bloków

**Decyzja użytkownika (§0 pkt 4)**: harness NIE jest monolitycznym blokiem
z pętlą w Rust. Pętla agentowa z §1.1 jest rozłożona na bloki, a jej treść —
to, co dzieje się w każdej iteracji — jest **flow w FlowBuilderze**, więc da się
ją modyfikować (wstawić filtr, podmienić kompakcję, dołożyć blok addonu) bez
zmian w kodzie. Mechanika "powtarzaj aż" żyje w generycznym bloku `loop`,
którego ciałem jest wskazany flow (zgodnie z filozofią "osobne flow składane
w większe" — ta sama mechanika co `subflow`).

Rozkład harnessa na seedowane flow:

```
TentaFlow Harness (...0011):
  trigger → conversation_history → agent_router → subflow(Agent Run) → output

Agent Run (...0012):
  trigger → agent_context → loop(body: "Agent Iteration",
                                 until: vars.harness_done,
                                 max_iterations z vars) → output

Agent Iteration (...0013):
  trigger → compact_context → llm (tryb tools) → tool_exec → output
```

Role bloków (specyfikacje w §3.5):
- **`agent_context`** — ładuje definicję agenta (z configu bloku albo
  z `vars.agent_id` ustawionego przez router): `system_prompt` →
  `context.system_prompts`, indeks skilli (`<available_skills>`: name+description,
  nakaz `skill_view` — wzorzec Hermes), `vars.harness_tools` (allowlista),
  `vars.loop_max_iterations` (z `agents.max_iterations`), nota anty-injection
  (§3.10); tworzy rekord `agent_runs` (jeśli `vars.agent_run_id` brak)
  z pryncypałem `ctx.user_id`.
- **`llm` w trybie tools** — istniejący node `llm` ROZSZERZONY (reguła jakości 4:
  nie forkujemy w `llm_call`): gdy `vars.harness_tools` obecne, żądanie idzie
  z `tools` (§3.1), a odpowiedź z `tool_calls` ląduje jako wiadomość asystenta
  z `tool_calls` w `context.messages`. Iteracja finałowa
  (`vars.loop_final_pass=true`): bez tools + nota "podsumuj co osiągnięto"
  (grace summary Hermesa, §1.1).
- **`tool_exec`** — wykonuje `tool_calls` z ostatniej wiadomości asystenta:
  nazwa z prefiksem `core.` → wbudowany handler (dispatch PRZED `ToolDispatcher`;
  addon_id `core` zarezerwowany — walidacja instalacji addonu odrzuca tę nazwę);
  pozostałe → `spawn_blocking { ToolDispatcher.dispatch_tool_call(name, args,
  user_id) }` (wasmtime synchroniczny — §2.12). Wyniki → wiadomości `role=tool`
  z truncation (per-tool `max_result_chars`, domyślnie 16k, middle-out) +
  koercja/sanityzacja błędów `[TOOL_ERROR]` (Hermes). Brak tool_calli →
  `vars.harness_done=true`, `vars.harness_exit_reason="final_response"`
  (wykrywanie końca jak Codex/Hermes). Każde wykonanie → wpis
  `compliance_ai_tool_calls` z realnym statusem + `run_log`.
- **`compact_context`** — kompakcja jako blok (a nie zaszyta w pętli): gdy
  szacowany kontekst > próg% okna modelu, dwufazowa kompresja z §1.2 (tania faza
  bez LLM + szablonowe streszczenie przez LLM). Wyjęcie/podmiana polityki
  kompakcji = edycja flow "Agent Iteration".
- **`loop`** — generyczny iterator (§3.5): wykonuje flow-ciało na bieżącym
  envelope, wynik iteracji = wejście następnej, aż `until` albo `max_iterations`.

**Audyt per wywołanie LLM**: zamiast dedykowanego executora — `LlmDispatcherImpl`
staje się gateway-aware: `FlowDispatcher::new` ma `DbPool`, więc dispatcher LLM
otwiera/zamyka event `compliance_ai_events` wokół każdego `execute_chat`,
z `AiGatewayContext{flow_id, flow_node_id}` (oba pola JUŻ istnieją) +
`agent_id`/`agent_run_id` ze zmiennych flow. Efekt: **każdy node `llm` w każdym flow** jest
audytowany per wywołanie (spójne z rolą AiGateway jako centralnego wejścia LLM),
nie tylko harness; zewnętrzny event routingu (czat) zostaje jako event sesyjny,
korelacja po `request_id`/`agent_run_id`. To zamyka lukę z §2.11 raz, dla
wszystkich obecnych i przyszłych bloków.

**`AgentService`** (moduł `tentaflow-core/src/agents/`) jest po tej zmianie
chudy: rejestr agentów + katalog narzędzi + `AgentRunManager` (§3.6) +
wbudowane handlery `core.*`. Pętli NIE wykonuje — pętla to flow. Konstruowany
w `main.rs` (`DbPool`, `Arc<AddonManager>` → `ToolDispatcher`, zaplecze
AiGateway), trzymany w `AppState`, wpinany do adapterów przez slot (§3.5.0).

Pozostałe zasady bez zmian względem pierwotnego projektu:
- **Narzędzia wbudowane core**: `core.skill_view(name, file_path?)` (bumpuje
  `use_count`), `core.agent_spawn/agent_wait/agent_list/agent_cancel` (§3.6;
  widoczne tylko gdy `max_subagents > 0`), `core.ask_user` (§3.13 — pytania
  do użytkownika, w allowliście domyślnie tylko dla agentów najwyższego
  poziomu; dzieci bąbelkują przez rodzica). Wykonywane w core (nie WASM),
  audytowane jak narzędzia addonów.
- **Allowlista + pryncypał**: narzędzia agenta ∩ uprawnienia addonów dla
  `user_id` przebiegu (§3.3); subagent dostaje `tools(child) ∩ tools(parent)`.
- **Trace**: iteracje → `run_log` (retencja §3.3); blok `loop` zostawia
  w `envelope.trace` jeden `TraceStep` z licznikami (iteracje, tool calle,
  tokeny). Pełny przebieg w UI Agenci → Przebiegi.

### 3.5 Nowe bloki Flow Buildera

**3.5.0 Wiring zależności** (rozstrzygnięcie — `build_registry()` jest
bezargumentowe, §2.7): nowe adaptery rejestrują się w `build_registry()` ze
**slotami** (wzorzec `ModelRuntimeSlot`, `RwLock<Option<Arc<...>>>`):
- `agent_context`, `tool_exec`, `agent_router`, `agent` → `AgentServiceSlot`,
  wypełniany z `main.rs` przez `FlowDispatcher::set_agent_service(Arc<AgentService>)`
  (analogicznie do `set_addon_resolver`) po zbudowaniu `AddonManager`/`AppState`;
- `subflow`, `loop`, `map`, `agent` → `SubflowRunnerSlot`; `SubflowRunner { db,
  registry: Weak<AdapterRegistry>, compile: CompiledFlow::from_json }` wypełniany
  już w `FlowDispatcher::new` (dispatcher ma DbPool i registry; `Weak` przerywa
  cykl registry→adapter→registry).
Wywołanie `execute` przy niewypełnionym slocie = błąd node'a (sloty wypełniane
na starcie procesu, przed obsługą ruchu — stan nieosiągalny w praktyce).

Wszystkie nowe bloki to adaptery core 1-in/1-out (zgodne z R4), kategoria wg
CHECK-a `flow_node_templates`. Rozszerzenie `llm` o tryb tools opisane w §3.4.

**1. `loop`** (kategoria `logic`) — generyczny iterator, nie tylko dla harnessa
- Config: `body_flow_id` (dynamic_enum `flows`), `until` (wyrażenie CEL —
  §3.12; domyślnie `vars.harness_done == true`; w zakresie wyrażenia także
  `iteration`),
  `max_iterations` (domyślnie 25, twardy cap 100; nadpisywalne przez
  `vars.loop_max_iterations`, które ustawia `agent_context` z definicji agenta),
  `final_pass` (bool: po wyczerpaniu budżetu jedna dodatkowa iteracja
  z `vars.loop_final_pass=true` — grace summary, §1.1).
- `execute`: wykonuje flow-ciało przez `SubflowRunner` na bieżącym envelope;
  envelope wyjściowy iteracji N = wejściowy iteracji N+1. Przed każdą iteracją:
  cancel/deadline-check. Po wyjściu: `vars.loop_iterations`,
  `vars.loop_exit_reason` (`until` / `max_iterations` / `cancelled` / `error`).
- Iteracje ciała wykonują się w trybie lekkim (jak flow syntetyczne:
  `execution_id=0`, BEZ rekordu `flow_executions` per iteracja — 25 iteracji
  nie może spamować tabeli); przebieg iteracji idzie do `run_log` przebiegu
  agenta (gdy `vars.agent_run_id` obecne) i do jednego `TraceStep` bloku.
- Guard rekurencji: ciało dziedziczy `subflow_depth+1` i `subflow_visited`
  (ścieżka stosu — sekwencyjne powtórzenia tego samego ciała są legalne,
  zagnieżdżenie tego samego flow w głąb nie).

**2. `map`** (kategoria `logic`) — dynamiczna równoległość ("50 zadań naraz")
- Config: `body_flow_id` (dynamic_enum `flows`), `concurrency` (domyślnie 4,
  cap 16), `items` (wyrażenie CEL wskazujące tablicę — §3.12; domyślnie
  `payload` jako `FlowValue::Json` z tablicą), `error_policy`
  (`fail_fast` | `collect` — błędne elementy jako `{"error": ...}` w wyniku);
  w ciele dostępne `item` i `index` (zakres CEL §3.12), zmienne wyjściowe
  elementów scala polityka jak w `combine`.
- `execute`: per element tablicy wykonuje flow-ciało przez `SubflowRunner`
  (element → `payload` ciała; meta/context dziedziczone), z capem współbieżności
  (`JoinSet` + semafor); wyniki składane w `FlowValue::Json` (tablica, kolejność
  wejściowa). Cancel/deadline przerywa wszystkie elementy w locie.
- Iteracje w trybie lekkim jak `loop` (bez rekordu `flow_executions` per element);
  postęp per element → zdarzenia postępu (§3.11). Ten sam guard rekurencji.
- Statyczny DAG nie umie wyrazić "N gałęzi znane w runtime" — `map` to
  odpowiednik dynamic task mapping z Airflow/Temporal; razem z `loop` tworzy
  rodzinę bloków sterujących z flow-ciałami.

**3. `agent_context`** (kategoria `service`)
- Config: `agent_id` (dynamic_enum `agents`) **albo** `from_vars=true`
  (bierze `vars.agent_id` ustawione przez `agent_router`); opcjonalne
  nadpisania: `model`, `max_iterations`.
- `execute`: ładuje definicję agenta i przygotowuje envelope pod pętlę —
  szczegóły w §3.4 (system prompt, indeks skilli, `vars.harness_tools`,
  `vars.loop_max_iterations`, nota anty-injection, rekord `agent_runs`
  z pryncypałem `ctx.user_id`).

**4. `tool_exec`** (kategoria `service`)
- Config: `max_result_chars` (domyślnie 16000), `max_tool_calls_per_iteration`
  (domyślnie 16).
- `execute`: wykonuje `tool_calls` ostatniej wiadomości asystenta — dispatch
  `core.*` → wbudowane, reszta → `ToolDispatcher` w `spawn_blocking`; wyniki
  jako `role=tool`; brak tool_calli → `vars.harness_done=true`. Szczegóły §3.4.

**5. `compact_context`** (kategoria `transform`)
- Config: `threshold_percent` (domyślnie 50), `protect_last_messages`
  (domyślnie 4), `summary_model` (dynamic_enum `models`, puste = model
  z `meta`).
- `execute`: nic nie robi poniżej progu; powyżej — dwufazowa kompresja §1.2
  (ostatnia wiadomość użytkownika zawsze w żywym ogonie). Wywołanie
  streszczające = zwykłe `execute_chat` (audytowane jak każde, §3.4).

**6. `agent`** (kategoria `service`) — agent bezpośrednio jako blok (wymóg §0)
- Cienki: NIE zawiera pętli. Config: `agent_id` (dynamic_enum `agents`).
- `execute`: ustawia `vars.agent_id` i wykonuje przez `SubflowRunner` flow
  agenta (`agents.flow_id`, domyślnie seedowany "Agent Run") — czyli dokładnie
  to, co zrobiłby `subflow` z prefillem. Wynik → `payload=Text(final)`,
  `vars.agent_run_id`, `vars.agent_exit_reason`; wewnętrzna konwersacja pętli
  nie wraca do envelope rodzica (wzorzec Codex review: wraca podsumowanie).
- Streaming finalnej odpowiedzi przez forward strumienia ciała (§3.11 B —
  od fazy 5).
- **Paleta**: obok generycznego wpisu `agent`, handler `flow_node_templates_list`
  dokleja po jednym wpisie na włączonego agenta (wzorzec bloków addonów,
  `handlers.rs:1037`) z prefillem `agent_id` w `default_config`. Jeden
  `node_type`, zero nowych adapterów, a UX = "agent jest blokiem".

**7. `agent_router`** (kategoria `logic`) — wybiera, NIE wykonuje
- Config: `agent_ids` (multi-select; puste = wszyscy włączeni agenci
  z `routable=1` — §3.3), `router_model` (dynamic_enum `models`; mały/szybki
  model), `fallback_agent_id` (gdy router nie wybierze jednoznacznie).
- `execute`: jedno tanie wywołanie LLM — prompt z listą {name, description} +
  zadaniem użytkownika w wydzielonym, oznakowanym bloku (tekst zadania to dane,
  nie instrukcje dla routera); wymuszona odpowiedź
  `{"agent":"<name>","reason":"<1 zdanie>"}`. Wynik: `vars.agent_id`,
  `vars.agent_routing = {candidates, selected, reason, fallback_used}` —
  zapisywane też do `run_log`, więc "dlaczego ten agent" jest audytowalne w UI.
  Wykonanie wybranego agenta to następny blok w grafie (`subflow(Agent Run)`
  albo `agent_context` + `loop`) — router nie musi nic uruchamiać, a harness
  pozostaje w 100% widoczny w FlowBuilderze.
- **Eskalacja uprawnień**: wybór agenta sterowany niezaufanym tekstem to
  confused-deputy — dlatego (a) kandydaci tylko `routable=1` (admin świadomie
  oznacza agentów dopuszczonych do auto-routingu; agentów z szerokimi
  uprawnieniami, np. `addon.*` + `agent_spawn`, oznacza `routable=0`),
  (b) zalecaną praktyką jest jawna lista `agent_ids` w configu bloku.

**8. `subflow`** (kategoria `logic`) — blok ogólnego przeznaczenia
- Config: `flow_id` (dynamic_enum, nowe źródło `flows` — tylko `status='active'`;
  loader dostaje id edytowanego flow, by wykluczyć self-reference z listy —
  wymaga przekazania parametru do `loadDynamicEnumOptions`), `timeout_ms`
  (clamp do deadline'u rodzica).
- `execute`: pobiera flow po id, kompiluje `CompiledFlow::from_json` per
  wywołanie (tak działa dzisiejsze `dispatch_by_flow_id`; `FlowCache` jest
  kluczowany `{model}:{service_type}:{modality}` i NIE nadaje się tu bez zmian —
  cache per flow_id to ewentualna późniejsza optymalizacja), wykonuje
  `execute_blocking` z **bieżącym envelope jako initial** (trigger sub-flow go
  skonsumuje) na **klonie** `ExecutionContext` (derive `Clone` — same Arc):
  świeży `execution_id` (nowy rekord `flow_executions` z nową kolumną
  `parent_execution_id`), świeży `Arc<UsageSink>` (współdzielony sink byłby
  drenowany przez zagnieżdżone wykonanie i kradłby atrybucję tokenów
  równoległym gałęziom rodzica — `executor.rs:150`), `subflow_depth + 1`.
  Zagregowane zużycie dziecka wraca do `TraceStep` bloku.
- **Zabezpieczenia w `ExecutionContext`, NIE w `envelope.meta`**: nowe pola
  `subflow_depth: u8` i `subflow_visited: Arc<[String]>` (flow_id'y na ścieżce).
  Meta jest zapisywalne przez każdy node — w tym blok addonu WASM, który
  deserializuje CAŁY envelope z odpowiedzi gościa (`addon.rs:173`) — więc guard
  w meta dałby się wyzerować i umożliwić nieograniczoną rekurencję. Limit
  głębokości 4 + odwiedzony flow_id = błąd bloku. Sub-flow streamingowy
  wykonywany w trybie blocking (wynik = finalny envelope).
- Artefakty sub-flow wracają do rodzica z prefiksem `subflow.{node_id}.`
  (artefakty są add-only — bez prefiksu kolizja kluczy).

Dla każdego bloku: seed `flow_node_templates` (UWAGA: prune na starcie — wpisy
muszą być w tablicy seedów), i18n `flows.node_names/node_descriptions` ×5 języków,
ikony/`--node-*` w CSS (opcjonalne), nowe źródła `dynamic_enum` (`agents`, `flows`)
w `flows-builder/config.js::loadDynamicEnumOptions`.

**Walidacja**: nowe bloki same w sobie nie wymagają zmian w R1–R8 (1-in/1-out,
porty typowane `any`→`text`). Zmiany w executorze/walidacji są wyłącznie tymi
zaplanowanymi w §3.11 (faza 4: skip-semantyka, slot producenta strumienia w R7,
`ProgressSink`) + dwa nowe pola `ExecutionContext` dla guardu subflow.

### 3.6 Agenci w tle (`AgentRunManager`)

Nowy serwis w `AppState`: rejestr aktywnych przebiegów
(`HashMap<RunId, RunHandle{ join: AbortOnDropHandle, status: watch::Sender }>`)
+ persystencja w `agent_runs`.

- `spawn(agent_id, prompt, parent_run_id, inherited_tools) -> run_id` — tworzy rekord
  `queued`, odpala task tokio, który **wykonuje flow agenta** (`agents.flow_id`,
  domyślnie "Agent Run" — ta sama ścieżka co blok `agent`, §3.4/§3.5; w meta:
  `agent_id`, `agent_run_id`, intersekcja narzędzi) z deadline'em
  z `agent.timeout_secs`, heartbeat do `last_heartbeat_at` co 30 s, status przez
  `watch::Sender`. Przebieg w tle to więc zwykłe wykonanie flow — bez drugiego
  silnika pętli.
- Narzędzia agenta (wzorzec Codex V1):
  - `core.agent_spawn {agent_name, task, context?}` **albo batch
    `{tasks: [{agent_name, task, context?}, ...]}`** → `{run_ids}` — wraca
    natychmiast (wzorzec `spawn_agents_on_csv` Codexa: 50 zadań jednym
    wywołaniem → cap równoległości, reszta `queued` w kolejce FIFO); dzieci
    liczą się do `max_subagents` rodzica i globalnego capa
    (`agents.max_concurrent_runs`, ustawienie, domyślnie 8); głębokość
    z `max_spawn_depth` (liczona po łańcuchu `parent_run_id` w DB — nie do
    podrobienia przez model); dziecko dziedziczy pryncypała rodzica (§3.3)
    i `tools(child) ∩ tools(parent)`.
  - `core.agent_wait {run_ids, timeout_secs}` → `{run_id: {status, result?}}` —
    czeka na watch-channelach (bez pollingu), respektuje cancel/deadline rodzica.
    **Anty-livelock**: rodzic w `agent_wait` przechodzi w status `waiting`
    i ZWALNIA swój slot globalnego capa (permit semafora oddawany na czas
    oczekiwania, odbierany po przebudzeniu) — inaczej ≥cap równoległych rodziców
    czekających na zakolejkowane dzieci zaklinowałoby pulę aż do timeoutów.
    Ta sama reguła dla `waiting_user` (przebieg czekający na odpowiedź
    użytkownika, §3.13).
  - `core.agent_list` → aktywne dzieci rodzica; `core.agent_cancel {run_id}`.
- Rodzic "wraca" do dziecka przez `agent_wait` w dowolnej późniejszej iteracji —
  dokładnie tak robi Codex; wynik dziecka = jego finalna odpowiedź.

**Powrót wyników, gdy tura rodzica skończyła się przed dzieckiem** — trzy
poziomy, wszystkie w zakresie planu:
1. **Powiadomienie** (faza 6): zakończenie przebiegu w tle emituje zdarzenie
   (§3.11) → widżet aktywności w czacie (badge + toast "Agent X ukończył —
   zobacz wynik", §3.9) i wpis w Agenci → Przebiegi.
2. **Mailbox** (faza 7, wzorzec Codex V2): tabela `agent_mailbox`
   (`run_id`, `target_session_id`, `target_agent_id`, `payload` = finalna
   odpowiedź dziecka, `delivered_at NULL`). Przy następnej interakcji w tej
   sesji (lub z tym agentem) `agent_context` wstrzykuje niedostarczone wyniki
   do kontekstu ("delegowane zadanie X zakończyło się wynikiem: ...")
   i oznacza `delivered_at`. Retencja jak `agent_runs`.
3. **Auto-kontynuacja** (faza 7, opt-in per agent: `on_child_complete =
   'notify' | 'continue'`): zakończenie dziecka samo odpala nowy przebieg
   rodzica z wynikiem jako wejściem (Ralph-style). Wyłącznie świadoma decyzja
   admina — to autonomiczne zużycie tokenów; liczy się do capów i głębokości
   jak każdy przebieg, więc pętla wzajemnych kontynuacji ubija się o limity.

- Po restarcie procesu: biegi `running` → `interrupted` przy starcie (uczciwa
  semantyka v1; wznawianie z `run_log` = przyszłość, wzorzec rollout-reconstruction
  Codexa). Niedostarczone wpisy `agent_mailbox` przeżywają restart (SQLite).
- Blok `agent`/`agent_router` używa tych samych przebiegów (rekord `agent_runs`
  z `flow_execution_id`), więc UI pokazuje wszystko w jednym miejscu.

### 3.7 Timeout długich zadań

- `run_blocking`: zamiast stałej `FLOW_TIMEOUT_SECS` czytaj `timeout_ms` z configu
  **trigger** node'a (tam już mieszka `continue_on_error`), clamp do ustawienia
  globalnego `flow_max_timeout_ms` (seedowane, domyślnie 3 600 000). Brak configu =
  dzisiejsze 120 s (zero regresji).
- Blok `agent` dodatkowo clampuje swój deadline do `min(agent.timeout_secs,
  pozostały deadline flow)` — jak `addon` node dzisiaj.
- Przebiegi w tle nie podlegają timeoutowi flow (własny deadline z definicji agenta).

### 3.8 Seedowane flow harnessa

Trzy flow ze stałymi UUID (losowe per node rozjechałyby sync — jak Default Chat),
`is_default=0`, **bez** `flow_model_bindings`, **bez** `published_model_name`,
`service_type=NULL` — celowo nieosiągalne przez resolver; do użycia jako Sub Flow
/ ciało pętli / jawny `flow_invoke` (decyzja użytkownika):

| Flow | UUID | Graf |
|---|---|---|
| TentaFlow Harness | `...0011` | `trigger → conversation_history → agent_router → subflow(Agent Run) → output` |
| Agent Run | `...0012` | `trigger → agent_context(from_vars) → loop(body: Agent Iteration) → output` |
| Agent Iteration | `...0013` | `trigger → compact_context → llm(tools) → tool_exec → output` |

Wszystkie trzy są zwykłymi flow — edytowalne w FlowBuilderze; harness
customizuje się przez edycję tych flow albo podpięcie agentowi własnego
`flow_id` (§3.3).

Seed dodaje też jednego agenta systemowego `general` ze **stałym UUID**
(`...0014`) (opis: "Agent ogólnego przeznaczenia...", narzędzia:
`core.skill_view` + memory.* jeśli addon obecny, `routable=1`) — żeby harness
działał out-of-the-box. Test seedów asystujący przy "exactly one default flow"
(`seed.rs:765`) wymaga aktualizacji.

Uwaga na później (samo wpięcie poza zakresem tego planu): wpięcie harnessa do
Default Chat przez `subflow` oznacza, że producentem odpowiedzi przestaje być
node `llm` — wymaga uogólnienia producenta strumienia (§3.11 B, faza 4)
i forwardu w `loop`/`subflow` (faza 5). Wpięcie planować PO fazie 5 — wtedy
czat zachowuje streaming token-po-tokenie.

### 3.9 Menu i ekrany

- `ADMIN_NAV`: nowa sekcja `nav.section_ai_agents` z pozycjami
  `{id:'agents', labelKey:'nav.agents'}` i `{id:'skills', labelKey:'nav.skills'}`
  (+ `Router.register`, moduły `www/js/modules/agents.js`, `skills.js`,
  i18n ×5, symbole SVG w sprite).
- Wyłącznie komponenty `tf-*` (tabele: `tf-table`, edytory: `tf-modal`/`tf-window`,
  tagi: `tf-chip`, pickery: `tf-combobox`/`tf-checkbox`).

**Widżet aktywności agentów w czacie** (`chat.js` + `chat-audio.js`) — wymóg §0
pkt 6: widać, co dzieje się w tle, kompaktowo, z drill-in. Nowy komponent
**`tf-agent-activity`** (wzorzec powtórzony w 2 modułach ⇒ nowy komponent
`tf-*`, reguła jakości 7), zasilany zdarzeniami postępu (§3.11):

- **Stan zwinięty** (domyślny): pojedynczy cienki pasek nad polem wpisywania —
  pulsująca kropka + jedna linia bieżącego kroku
  ("Researcher · iteracja 3/25 · memory.memory_search…") + badge liczby
  przebiegów w tle ("2 w tle"). Auto-ukrywa się, gdy nic nie działa —
  zero zajętego miejsca w zwykłej rozmowie. W `chat-audio.js` wariant jeszcze
  węższy (sama kropka + badge).
- **Drill-in poziom 1** (klik w pasek): rozwijany panel — drzewko przebiegów
  (rodzic → dzieci ze spawnu/map), per przebieg: status, agent, czas, tokeny,
  przycisk anuluj. Żywe aktualizacje ze zdarzeń, bez pollingu.
- **Drill-in poziom 2** (klik w przebieg): oś czasu kroków — iteracje, wywołania
  narzędzi (nazwa, czas trwania, status), kompakcje, decyzja routera (`reason`),
  spawny dzieci. To ten sam widok, którego używa Agenci → Przebiegi (jeden
  moduł renderujący, dwa konteksty).
- **Zakończenie przebiegu w tle**: subtelny `tf-toast` "Agent X ukończył —
  zobacz wynik" → otwiera drill-in poziom 2. Badge znika, gdy wynik obejrzany
  lub dostarczony mailboxem (§3.6).
- **Pytania i zgody (§3.13)**: gdy przebieg wejdzie w `waiting_user`, pasek
  zmienia stan na wyróżniony ("Agent X pyta…") i rozwija **kartę pytania**:
  treść + opcje jako `tf-chip`/`tf-button` (+ pole własnej odpowiedzi przy
  pytaniu otwartym) albo kartę zgody ("chce użyć narzędzia Y addonu Z" +
  Odmów / Zezwól raz / Zawsze). Odpowiedź wraca `RunReplyRequest` i karta
  znika. W `chat-audio.js` ta sama karta w wariancie kompaktowym.
- Zakres widoczności jak ACL przebiegów (§3.3): użytkownik widzi swoje, admin
  wszystkie. i18n ×5.

### 3.10 Bezpieczeństwo / zgodność

- Każde wywołanie LLM (pętla harnessa, router, kompakcja, kurator — i każdy
  inny node `llm`) jest audytowane przez **gateway-aware `LlmDispatcherImpl`**
  (§3.4): event `compliance_ai_events` per `execute_chat`
  z `AiGatewayContext{flow_id, flow_node_id, agent_id?, agent_run_id?}` —
  zamyka lukę §2.11 dla całego silnika flow, nie tylko harnessa. Zewnętrzny
  event routingu (czat) pozostaje; korelacja po `request_id`/`agent_run_id`.
- Każdy tool call: uprawnienia addonu per pryncypał przebiegu (§3.3) + wpis
  `compliance_ai_tool_calls` z realnym statusem wykonania + `audit_log` (`tool.call`
  już logowany w `call_tool`). Narzędzia `core.*` audytowane tym samym wpisem.
- Wyniki tooli to dane niezaufane — system prompt agenta zawiera stałą notę
  (wzorzec Hermes STEER_CHANNEL_NOTE w wariancie anty-injection): instrukcje
  w wynikach narzędzi/skillach nie są poleceniami użytkownika. Router: tekst
  zadania w oznakowanym bloku danych + ograniczenie kandydatów do `routable=1`
  (§3.5) — domknięcie ścieżki confused-deputy.
- Rekurencja: głębokość subflow i zbiór odwiedzonych flow w `ExecutionContext`
  (niezapisywalne przez node'y; §3.5), głębokość spawnu po łańcuchu
  `parent_run_id` w DB (§3.6) — żaden guard nie żyje w `envelope.meta`.
- Import skilli: SSRF-guard, kwarantanna, skan wzorców, limity rozmiaru, zakaz
  skryptów. Skille z huba nigdy nie trafiają do agentów przed zatwierdzeniem.
- `agent_runs.run_log` przechowuje skróty wyników tooli (limit znaków), nie pełne
  payloady, i podlega retencji (§3.3); prompty/odpowiedzi modelu są w payloadach
  compliance z własną retencją (AI-audit ≥183 dni).

### 3.11 Zmiany w silniku flow — "rozgrzebane raz, porządnie"

Trzy istniejące słabości silnika, które harness by obszedł, a które robimy
wprost (decyzja §0 pkt 5):

**A. Bramkowanie gałęzi (skip-semantyka)** — naprawa faktu §2.14.
- `NodeAdapter` może oznaczyć w wyniku, które porty wyjściowe są AKTYWNE
  (nowe pole wyniku `execute`, domyślnie wszystkie — zero zmian w istniejących
  adapterach); `condition` aktywuje `true` ALBO `false`.
- Executor: następnik osiągalny wyłącznie nieaktywnymi krawędziami dostaje
  status `Skipped` (nowy `TraceStatus`) i propaguje skip w dół; `combine`/`output`
  ignorują wejścia ze `Skipped` (barierę spełniają pozostałe). Model wykonania
  "node odpala się raz" zostaje nietknięty — to bramkowanie, nie cykle.
- `FlowEdge.condition` (deserializowane, martwe) zostaje usunięte albo wpięte
  w ten sam mechanizm — bez trzeciego, równoległego sposobu wyrażania warunków.
- Zyskują wszystkie flow, nie tylko harness (np. routing po `condition_result`
  przestaje wykonywać obu gałęzi "na wszelki wypadek").

**B. Uogólnienie producenta strumienia** — dziś strumień może produkować tylko
node ze slotu `registry.llm()` (`executor.rs:357`); bez tej zmiany harness
wpięty do czatu odpowiada jednym chunkiem.
- Nowy trait `StreamProducerAdapter` (obok `StreamingNodeAdapter` dla
  pośredników): `produce_stream(node, inputs, ctx) -> BoxStream<EnvelopeDelta>`;
  rejestracja w slocie registry per node_type.
- `cache.rs::streaming_llm_run_idx` → generalizacja: producent = node, którego
  krawędź wychodzi z portu `stream` i który ma zarejestrowanego producenta
  (LLM pozostaje jednym z producentów — `LlmAdapter` implementuje nowy trait).
- `loop`/`subflow`/`agent` forwardują strumień: ostatnia iteracja ciała /
  ciało wykonane w trybie streamingowym przepuszcza deltę LLM na zewnątrz —
  finalna odpowiedź harnessa streamuje do czatu token-po-tokenie.
- R7 (walidacja end-shape) czyta nowy slot zamiast zakładać LLM.

**C. Zdarzenia postępu wykonania** — Codex i Hermes żyją strumieniem eventów;
u nas długi przebieg byłby czarną skrzynką.
- Nowy trait `ProgressSink` w `flow_engine/dispatchers/` (wzorzec `MetricsSink`),
  pole w `ExecutionContext`; produkcyjna implementacja publikuje do brokera
  w `AppState` (`broadcast` per zakres). Emitowane zdarzenia: node start/finish
  (z typem), iteracja `loop`/element `map` (n/max), tool call start/finish
  (nazwa, czas, status), kompakcja, spawn/zakończenie dziecka, decyzja routera.
- Protokół: `AgentsPayload::RunEventsSubscribeRequest {scope: session | run_id}`
  + push `AgentsPayload::RunEvent` po istniejącym kanale WS/WT (dashboard już
  odbiera push przy streamingu czatu — ten sam mechanizm ramek). ACL jak
  przebiegi; admin może subskrybować wszystko.
- Konsumenci: `tf-agent-activity` w czacie (§3.9), Agenci → Przebiegi (żywy
  podgląd), przyszłe metryki. Zdarzenia są ulotne (nie persystowane — trwały
  zapis to `run_log`); UI po reconnect odtwarza stan z `RunDetail` + dosłuchuje.

### 3.12 Zmienne flow + język wyrażeń (CEL)

Formalizacja czwartego kanału danych (decyzja §0 pkt 7) — zamiast rozrostu
nieudokumentowanych kluczy `meta`:

**Model danych**
- `FlowEnvelope.variables: BTreeMap<String, FlowValue>` (pole opcjonalne,
  serde-default — `schema_version` zostaje 1). `meta` od tej pory służy
  WYŁĄCZNIE wewnętrznej hydraulice silnika; dane użytkowe = zmienne.
  Sygnały harnessa (`harness_done`, `agent_id`, `loop_*`) projektowane od
  razu jako zmienne.
- Deklaracje w `flow_json`: sekcja `variables: [{name, type, default?,
  description?}]` — FlowBuilder pokazuje je w panelu flow; zapis do
  niezadeklarowanej zmiennej = błąd walidacji na save (nowa reguła **R10**).
- Guardy silnika (subflow_depth, visited) ZOSTAJĄ w `ExecutionContext`
  (§3.10) — zmienne, jak meta, są zapisywalne przez node'y.

**Semantyka współbieżności — "płyną z envelope" (decyzja użytkownika)**
- Zmienne podróżują z envelope: każda gałąź fan-out dostaje własną kopię
  (copy-on-write); ŻADNEJ globalnej mutowalnej mapy per wykonanie (wyścigi —
  znany footgun Camundy przy równoległości).
- `combine` scala: domyślnie konfliktowy zapis tego samego klucza różnymi
  wartościami = błąd node'a; per klucz polityka w configu `combine`:
  `last_wins` | `prefer_port(<port>)` | `collect` (tablica). Deterministyczne
  i debugowalne; w liniowym flow nieodróżnialne od "zmiennych globalnych".
- `subflow`/`loop`/`map` przekazują zmienne do ciała i z powrotem (map:
  zmienne wyjściowe elementów scala polityka jak w combine).

**Język wyrażeń: CEL** (crate `cel` 0.13 — następca `cel-interpreter`; sandbox,
brak I/O, deterministyczny; FEEL z Camundy nie ma dojrzałej implementacji
w Rust, JS-wyrażeń n8n nie odpalimy w core). ZAIMPLEMENTOWANE na branchu
`phase4-cel` (`flow_engine/expr.rs`, 39 testów). Limity zmierzone empirycznie,
inne niż pierwotnie zakładane: `MAX_EXPR_CHARS=512` (nie 4096 — dłuższe
łańcuchy operatorów przepełniają stos parsera ANTLR), cap zagnieżdżenia 32
liczący też `?` (beznawiasowa rekursja ternary trafiała w panikę parsera
cel 0.13), budżet czasu ewaluacji (domyślnie 1 s, `recv_timeout` + odłączany
worker — wielomianowe komprehensje nie zawieszą wątku tokio), bindowanie
wyłącznie zmiennych z `Program::references()`. Granica paniki przez `join`
działa tylko w dev (release ma `panic="abort"`) — realną obroną jest walidacja
wejścia przypięta baterią hostile-testów. Zakres zmiennych w wyrażeniu: `vars`,
`payload`, `artifacts`, `meta` (read-only) + lokalne `item`/`index` w `map`
i `iteration` w `loop`. Jeden silnik wyrażeń dla WSZYSTKICH miejsc — od
pierwszego dnia używają go bloki z faz 4–5:
- `condition.expression` (CEL → aktywacja portu `true`/`false`, §3.11 A),
- `loop.until` (CEL zamiast ad-hoc konwencji po kluczach),
- `map.items` (CEL wskazujący tablicę).

**Io-mapping per node (Camunda-style), generycznie w executorze**
- `node.config.input_mapping: {<klucz_configu>: "<CEL>"}` — wyliczane PRZED
  `execute`, wyniki nakładane na config node'a. Efekt: KAŻDY istniejący blok
  (łącznie z addon.*) może mieć config liczony ze zmiennych
  (np. `llm.model = vars.chosen_model`) bez zmiany jednego adaptera.
- `node.config.output_mapping: {<zmienna>: "<CEL>"}` — wyliczane PO `execute`
  na wyniku (`payload`, `artifacts`, `meta` node'a), zapis do zmiennych.
- Implementacja w jednym miejscu (`executor.rs` wokół wywołania adaptera +
  ścieżka streamingowa pre-LLM); błąd wyrażenia = błąd node'a z czytelnym
  komunikatem (nazwa node'a, wyrażenie, przyczyna).
- UI: prosty edytor deklaracji zmiennych w fazie 5; pełny edytor io-mapping
  per node (zakładka "Zmienne" w configu bloku) w fazie 7.

### 3.13 Pytania do użytkownika i zgody w trakcie wykonania

Dwa rozłączne mechanizmy (wzorce §1.7), wspólna infrastruktura dostarczania
(zdarzenia §3.11 C + widżet §3.9 + Przebiegi):

**A. `core.ask_user` — narzędzie agenta** (doprecyzowania, brakujące dane)
- Wejście: `{question, choices?: [≤4], timeout_secs?}` (UI dokleja "Inna
  odpowiedź…" przy opcjach — wzorzec Hermes). Wynik: JSON
  `{question, choices_offered, user_response}`.
- Mechanika: przebieg → status `waiting_user` (slot semafora ZWOLNIONY,
  §3.6); pytanie idzie jako `RunEvent::UserQuestion` do sesji (czat: karta
  pytania w `tf-agent-activity`) i do Agenci → Przebiegi (przebiegi w tle:
  toast + badge). **Zegar deadline'u przebiegu jest pauzowany na czas
  oczekiwania** (czas czekania na człowieka nie konsumuje
  `agent.timeout_secs`; heartbeat dalej bije — odpowiednik dotykania
  heartbeatu w Hermes).
- Odpowiedź: `AgentsPayload::RunReplyRequest {run_id, question_id, answer}`
  (ACL: pryncypał przebiegu albo admin). Treść odpowiedzi wchodzi do wyniku
  narzędzia w oznakowanym markerze zaufanego kanału użytkownika (analog
  STEER-markera Hermesa, §3.10) — odpowiedź użytkownika to polecenia,
  wyniki tooli nie.
- Timeout (domyślnie 600 s) → sentinel `"[użytkownik nie odpowiedział
  w ciągu N min]"` jako wynik narzędzia — model adaptuje się zamiast wisieć.
  Anulowanie przebiegu zamyka wiszące pytania.
- Dzieci: `core.ask_user` domyślnie NIE w allowliście subagentów (Hermes
  blokuje clarify dzieciom); pytanie dziecka, któremu jednak je nadano,
  bąbelkuje do tego samego pryncypała z widocznym łańcuchem
  `parent_run_id` (Codex: approvals dzieci przez sesję rodzica).

**B. Zgody na uprawnienia** (brakujące uprawnienie do narzędzia addonu)
- `tool_exec` przy `NotConfigured` (deny z `permission_checker`) NIE kończy
  się cichym błędem: emituje `RunEvent::PermissionRequest {addon_id,
  tool_name, permission}` i wchodzi w `waiting_user` (karta zgody w widżecie:
  **Odmów / Zezwól raz / Zezwól na ten przebieg / Zawsze**).
- `Zawsze` persystuje grant przez istniejący permission engine addonów
  (zakres = pryncypał przebiegu; grant globalny może nadać tylko admin);
  `na ten przebieg` = cache w przebiegu (odpowiednik `ApprovedForSession`
  Codexa); decyzja idzie `AgentsPayload::PermissionReplyRequest`.
- Timeout → deny → `[TOOL_ERROR] permission denied` jako wynik narzędzia
  (model może zmienić podejście). Każda decyzja → `audit_log`.

**C. `ask_user` jako BLOK FlowBuildera** (kategoria `service`) — pytanie bez
agenta, wprost w flow (to jest nasz odpowiednik **BPMN User Task**, §6):
- Config: `question` (CEL-interpolowalne), `choices?`, `timeout_secs`,
  `output_variable` (zapis odpowiedzi przez output_mapping §3.12).
- Ta sama mechanika dostarczania co A; w v1 ograniczony deadline'em flow
  (minuty, nie dni) — wielodniowe human tasks wymagają trwałych instancji
  (jedyna duża brakująca inwestycja pod BPMN, §6).

---

## 4. Fazy implementacji

| Faza | Zakres | Wynik weryfikowalny |
|---|---|---|
| 1 | Tool-calling: `LlmRequest.tools`, `tool_calls` round-trip (blocking + mapper streamingowy `llm_impl.rs:354`), tryb `native`/`prompt` per kandydat w `ModelRuntimeExecutor` (render+parser+koercja), rewitalizacja `ToolDispatcher`, AiGateway z wynikami wykonania | test integracyjny: model (vLLM i llama.cpp) wywołuje `memory.memory_store` i dostaje wynik |
| 2 | Skills: migracja, protokół `SkillsBody`, ekran Skills, materializacja skilli addonów (deterministyczne ID, read-only + fork), CRUD + tagi | skille addonów `memory`/`embeddings-chunker` widoczne w UI; fork i edycja kopii działa |
| 3 | Agents: migracja (`agents` z `flow_id`/`routable`, `agent_runs` z pryncypałem i retencją), protokół `AgentsBody`, ekran Agenci, `AgentService` (rejestr + katalog tooli + handlery `core.*`), gateway-aware `LlmDispatcherImpl` (audyt per wywołanie), bloki `agent_context` + `tool_exec` + rozszerzenie `llm` o tryb tools, wiring slotów (§3.5.0), timeout flow z triggera | flow `trigger→agent_context→llm→tool_exec→output` wykonuje jedną iterację z narzędziem addonu; wywołanie widoczne w compliance |
| 4 | Silnik (§3.11 + §3.12): bramkowanie gałęzi `condition` (skip-semantyka + `TraceStatus::Skipped`), uogólnienie producenta strumienia (`StreamProducerAdapter` + generalizacja `streaming_llm_run_idx` + R7), `ProgressSink` + broker zdarzeń + `RunEventsSubscribe`/`RunEvent` w protokole; **fundament zmiennych**: `envelope.variables`, merge w `combine`, CEL (`cel-interpreter`), generyczny io-mapping w executorze, R10, `condition.expression` na CEL | condition wykonuje jedną gałąź wybraną wyrażeniem CEL po zmiennej ustawionej `output_mapping`; nie-LLM node streamuje do czatu; zdarzenia wykonania widoczne w kliencie testowym |
| 5 | Bloki kompozycji: `loop` (until=CEL, forward strumienia), `map` (items=CEL, równoległość z capem, merge zmiennych), `subflow` (klon ctx + `parent_execution_id` + guard głębokości/cykli w ctx), `agent_router` (routable, reason, fallback), `agent` (cienki), `compact_context` (minimalna); seedy "TentaFlow Harness"/"Agent Run"/"Agent Iteration" + agent `general`; wpisy per-agent w palecie; prosty edytor deklaracji zmiennych w FlowBuilderze | pełny wielokrokowy harness działa jako flow, jest edytowalny w FlowBuilderze i streamuje finalną odpowiedź; `map` przetwarza 50 elementów z capem 4 |
| 6 | Tło + interakcja + UI: `AgentRunManager` (semafor z oddawaniem slotu w `waiting`/`waiting_user`; przebieg = wykonanie flow agenta), `agent_spawn` (single+batch)/`wait`/`list`/`cancel`, **`core.ask_user` + blok `ask_user` + zgody na uprawnienia (§3.13)** z pauzą deadline'u, zakładka Przebiegi (ACL per użytkownik, żywy podgląd ze zdarzeń), **widżet `tf-agent-activity` w chat.js/chat-audio.js** (pasek + drill-in ×2 + karta pytania/zgody + toast zakończenia), purge retencyjny `agent_runs` | agent deleguje 50 zadań batchem, robi co innego, wraca po `agent_wait`; agent pyta o doprecyzowanie, użytkownik klika opcję w czacie, agent kontynuuje; brakujące uprawnienie → karta zgody → "Zawsze" persystuje grant |
| 7 | Powroty i dojrzałość: mailbox (`agent_mailbox` + wstrzykiwanie w `agent_context`), auto-kontynuacja (`on_child_complete`, opt-in), hub skilli (import GitHub/URL + kwarantanna), kurator (raport+apply), pełna kompakcja dwufazowa w `compact_context`, hardening pętli (nudge'y Hermesa), pełny edytor io-mapping per node (zakładka "Zmienne") | wynik nocnego przebiegu czeka w mailboxie przy następnej rozmowie; import skilla z anthropics/skills; raport kuratora; config bloku liczony ze zmiennych w UI |

Każda faza = osobny, kompletny przyrost (bez stubów). Krytyczna ścieżka:
1 → 3 → 4 → 5; faza 2 może iść równolegle z 3; 6 wymaga 4 (zdarzenia) i 5
(bloki); 7 domyka całość. **Cały zakres §3 jest objęty fazami — nic nie jest
"do rozważenia później".**

## 5. Otwarte kwestie (do decyzji przy review)

1. Równoległe wykonywanie tool calli jednej iteracji (Codex: FuturesOrdered;
   v1 proponuję sekwencyjnie — prościej audytować, addony WASM i tak mają pulę).
2. Czy `agent_router` ma honorować `keywords` z `ToolDefinition`/tagi skilli jako
   sygnał routingu (tanio: dokleić do opisu agenta listę nazw jego narzędzi).
3. Polityka modelu kompakcji/kuratora: dedykowane ustawienie `auxiliary_model`
   czy model agenta (v1: model agenta; ustawienie w fazie 7).
4. Czy skille `user` mają być per-użytkownik czy globalne (v1: globalne, admin
   zarządza; per-user wymaga zmian w syncu i ACL).
5. "Agent jako blok" — przyjęta interpretacja: jeden `node_type` `agent` +
   wpisy per-agent w palecie z prefillem konfiguracji (§3.5). Alternatywa
   (odrębne `node_type` `agent.{id}` przez dynamic resolver, jak `addon.*`)
   jest możliwa, ale zmiana zdania PO wdrożeniu zmienia zapisane `flow_json` —
   do potwierdzenia przy review.
6. Pryncypał przebiegów uruchamianych bez sesji użytkownika (np. przyszły
   trigger z harmonogramu): v1 = brak dostępu do narzędzi addonów (deny);
   docelowo ewentualny "service principal" z własnymi uprawnieniami.

---

## 6. BPMN — mapa drogowa (kierunkowa, poza fazami 1–7)

Decyzja (§0 pkt 7): docelowo **import podzbioru BPMN kompilowany do naszego
grafu bloków** — NIE drugi, natywny silnik BPMN obok `flow_engine` (token-based
state machine z kompensacjami i korelacją wiadomości to inny model wykonania;
utrzymywanie dwóch silników to podwójny koszt każdej przyszłej zmiany).

### 6.1 Mapowanie prymitywów (stan po fazach 1–7)

| BPMN | TentaFlow | Status |
|---|---|---|
| Service Task | dowolny node / `tool_exec` / blok addonu | jest |
| Exclusive Gateway | `condition` (CEL) + bramkowanie skip (§3.11 A, §3.12) | faza 4 |
| Parallel Gateway (fork/join) | fan-out + `combine` (merge zmiennych §3.12) | jest + faza 4 |
| Call Activity | `subflow` | faza 5 |
| Multi-Instance Activity | `map` | faza 5 |
| Loop Activity | `loop` | faza 5 |
| User Task | blok `ask_user` (§3.13 C) | faza 6 |
| Script Task | wyrażenia CEL w io-mapping (§3.12) | faza 4/7 |
| Process Variables / ioMapping | zmienne envelope + input/output_mapping (§3.12) | faza 4 |
| Timer Start Event | Admin Scheduler | jest |
| Message End/Throw (powiadomienia) | zdarzenia postępu + mailbox | fazy 4/7 |

### 6.2 Czego import BPMN będzie jeszcze wymagał (świadomie POZA planem 1–7)

1. **Trwałe instancje wykonania (durable execution)** — jedyna duża nowa
   inwestycja: persystencja stanu wykonania (envelope + zmienne + pozycje
   node'ów) na wait-state'ach, wznawianie po restarcie, wielodniowe czekanie.
   Bez tego User Task/Receive Task działają tylko w granicach deadline'u flow.
   Envelope i zmienne są serializowalne (CBOR) od początku — checkpoint na
   barierach nie wymaga zmiany modelu bloków, "tylko" warstwy persystencji
   i wznawiania.
2. **Generyczny blok "czekaj na zdarzenie/wiadomość"** (Receive Task /
   Intermediate Message Catch) + korelacja wiadomości (correlation keys po
   zmiennych) — naturalne rozszerzenie mailboxa z §3.6.
3. **Boundary events / per-activity timery** (timeout/eskalacja przypięta do
   pojedynczego node'a, nie całego flow).
4. **Parser/kompilator BPMN XML → flow_json** + translacja wyrażeń FEEL → CEL
   + import layoutu (BPMN DI → pozycje node'ów w FlowBuilderze).

### 6.3 Co plan 1–7 załatwia już teraz

Wszystkie decyzje modelowe, których nie dałoby się później tanio odkręcić:
typowane zmienne procesowe z io-mappingiem, jeden język wyrażeń (CEL),
bramkowanie gałęzi, rodzina bloków sterujących z flow-ciałami
(call/multi-instance/loop), human task z dostarczaniem do UI, zdarzenia
wykonania. Przyszły import BPMN konsumuje te prymitywy — nie wymaga ich
przeprojektowania.
