# Etap 1 — Flow Engine Clean Rewrite (plan v4.2)

**Status:** v4.1 + sekcja "Adapter semantics decisions (1b)" — 4 decyzje semantyczne wykryte podczas implementacji 1b. Wszystko inne z v4.1 nadal aktualne.

## Zmiany v4.1 → v4.2 (decyzje 1b)

W trakcie implementacji stage 1b (adapter layer) wyszły 4 luki semantyczne których v4.1 nie precyzował. Każda wymaga wpisu do planu.

### D1. `condition` — źródło pól
Stary `condition` (`adapters/condition.rs`) używa `ctx.input`, `ctx.model`, `ctx.variables`, `node_id.path` (cross-node lookup po `execution_log`). Hard rule 1 (single input edge) zabija cross-node lookup — adapter widzi tylko `inputs[0]`.

**Decyzja:**
- `field == "input"` → `inputs[0].envelope.payload` jako Text (jeśli payload to nie-Text → traktować jak null, condition zwraca false-equivalent)
- `field == "model"` → `inputs[0].envelope.meta["model"]`
- `field` jako klucz → `inputs[0].envelope.artifacts[field]` (Json-like path: `field.sub.path` zachodzi w obrębie wartości artefaktu)
- `ctx.variables` zachowane jako `inputs[0].envelope.meta` (uniwersalny key-value)
- Cross-node lookup **wycięty** (legacy flows które używają `node_id.field` są niekompatybilne; flowy w seed użyte dziś używają tylko `input` lub stałych)

### D2. `trigger` — source initial envelope
Trigger nie ma edge wejściowego (źródło flow). Adapter musi mieć skądś messages/payload.

**Decyzja:** dodać do `ExecutionContext` pole:
```rust
pub initial_envelope: Arc<FlowEnvelope>,
```

Trigger.execute() zwraca klon initial. Routing buduje initial przed `execute_blocking/execute_streaming`. Pozostałe adaptery ignorują pole — używają tylko `inputs[0]`.

**Doprecyzowanie (codex round D2):** `initial_envelope` jest seedem TYLKO dla trigger node'a. Streaming LLM w `execute_streaming` NIE czyta `initial_envelope` — czyta `inputs[0].envelope`, który jest outputem ostatniego pre-LLM node'a (po toposorcie pre-LLM nodes). Jeśli flow to `trigger → conversation_history → session_context → llm(stream)`, LLM dostaje envelope z dopisanymi `messages`/`system_prompts` z pre-LLM nodów, nie surowy initial. `LlmAdapter::prepare_llm_request(node, inputs, ctx)` bierze tylko inputs/ctx — nigdy `ctx.initial_envelope`.

### D3. `pii_filter` / `tts_clean` — nowe dispatchery dla reguł DB
Plan v4.1 nie wymienia tych traitów. Rules siedzą w DB (`pii_rules`, `tts_cleaning_rules`).

**Decyzja:** dodać do dispatchers/:

```rust
// dispatchers/pii_rules.rs
pub struct PiiRule {
    pub id: i64,
    pub name: String,
    pub category: String,
    pub pattern: String,
    pub replacement: String,
}

#[async_trait]
pub trait PiiRulesStore: Send + Sync {
    async fn active_rules(&self) -> Result<Vec<PiiRule>>;
}

// dispatchers/tts_cleaning.rs — opakowuje istniejący `tts::clean_cache::clean`,
// żeby adapter nie miał DbPool ani wiedzy o cache
#[async_trait]
pub trait TtsCleaningStore: Send + Sync {
    async fn clean(&self, text: &str) -> Result<String>;
}
```

Dodać oba do `ExecutionContext`. Cache (regex compile, TTL) żyje w impl, nie w adapterze.

### D4. Dispatcher impl wrappers — bootstrap z `ServiceManager`
Adaptery widzą `Arc<dyn LlmDispatcher>` itd., ale musimy te traity gdzieś **zaimplementować**. Implementacje opakowują istniejący `services/runtime/executor.rs::execute_chat/stream_chat/embed/...` plus QUIC mesh path.

**Decyzja:** nowy katalog `flow_engine/dispatchers_impl/`. **Każdy wrapper bierze najwęższe dependency** — NIE `Arc<ServiceManager>`, tylko konkretny runtime/cache:

- `llm_impl.rs` — `LlmDispatcherImpl { runtime: Arc<ModelRuntimeExecutor> }` → owija `executor.rs::execute_chat:172` + `stream_chat:253`
- `embeddings_impl.rs` — `EmbeddingsDispatcherImpl { runtime: Arc<ModelRuntimeExecutor> }` → owija `execute_embeddings:995`
- `tts_impl.rs` — `TtsDispatcherImpl { runtime: Arc<ModelRuntimeExecutor>, blobs: Arc<dyn BlobStore> }` → owija `execute_tts:1289`, audio bytes idą do `blobs.put`
- `stt_impl.rs` — `SttDispatcherImpl { runtime: Arc<SttRuntime>, blobs: Arc<dyn BlobStore> }` → owija `services/stt/runtime.rs::transcribe` (bezpośrednio, omijając `executor.rs::execute_stt:1545`)
- `prompts_impl.rs` — `PromptsImpl { registry: SharedPromptRegistry }` → owija `prompt_registry::SharedPromptRegistry::get_prompt`
- `memory_impl.rs` — `MemoryStoreImpl { quic_finder: Arc<QuicClientFinder>, settings: Arc<SettingsCache> }` (lub równoważne minimalne deps) — owija `find_quic_client_for_model("memory")`
- `conversation_impl.rs` — `ConversationHistoryImpl { cache: Arc<ConversationCache>, db: DbPool }` — owija dotychczasowy `conversation_cache`
- `audit_impl.rs` — `AuditSinkImpl { db: DbPool }` → owija `repository::log_audit`
- `pii_rules_impl.rs` — `PiiRulesStoreImpl { db: DbPool }` → owija `repository::list_pii_rules_active`
- `tts_cleaning_impl.rs` — `TtsCleaningStoreImpl { db: DbPool }` → owija `tts::clean_cache::clean`

**Twardy invariant:** żaden `*_impl.rs` nie trzyma `Arc<ServiceManager>`. Jeśli jakiś wrapper potrzebuje >2 zależności do zrekonstruowania routing logic, to znak że robimy god-objectu pod inną nazwą — najpierw refaktor istniejącego runtime, dopiero potem dispatcher.

Bootstrap żyje w `Router::new` (lub równoważnym): tworzymy każdy `Arc<dyn ...>` raz z minimalnym fragmentem `ServiceManager` (np. `service_manager.runtime()`, `service_manager.stt_runtime()`), wkładamy do `AdapterRegistry`/`ExecutionContext` factory.

### Skutek D1-D4 dla planu v4.1
- Sekcja "Capability dispatchers" rozszerza się o `PiiRulesStore`, `TtsCleaningStore` (D3).
- Sekcja "NodeAdapter / ExecutionContext" dodaje pole `initial_envelope: Arc<FlowEnvelope>` (D2).
- Nowa sekcja w call site refactor map: `flow_engine/dispatchers_impl/*.rs` (NEW, łącznie ~600-800 LOC) (D4).
- LOC estymata: +600-800 do "Razem".



## Zmiany v4.0 → v4.1 (codex round 5 fixy)

1. **`flow_executions` lifecycle dla streamingu** — `execute_blocking`/`execute_streaming` na starcie wołają `create_flow_execution(flow_id) → execution_id`. `ExecutionContext` dostaje `execution_id: i64`. Finalizer task dostaje `execution_id`, woła `update_flow_execution(execution_id, ...)`. Persist po `execution_id`, NIE po `flow_id`.
2. **Backpressure-resilient `outbound_tx.send`** — `select! { _ = outbound_tx.send(...) => {}, _ = cancel.cancelled() => break }`. Bounded channel + zatkany consumer NIE blokuje cancel.
3. **Disconnect detection bez `Sse::on_close`** — `server.rs:303` używa ręcznego `StreamBody`. Mechanizm: wrapper `CancelOnDropStream { inner, cancel_token }` z `Drop` impl wywołującym `cancel_token.cancel()`. Owijamy outbound stream przed zwrotem do hyper. Gdy klient disconnectuje, hyper droppuje body, `Drop` propaguje cancel do finalizera.
4. **Typed accessor zamiast downcast** — `AdapterRegistry { adapters: HashMap<String, Arc<dyn NodeAdapter>>, llm: Arc<LlmNodeAdapter> }` — typed pole obok mapy. Executor woła `registry.llm.prepare_request(...)` bezpośrednio.

Plus drobne:
- `continue_on_error` flag żyje w `trigger_node.config["continue_on_error"]` (JSON). Executor odczytuje w `execute_blocking`/`execute_streaming`: `let coe = trigger.config.get("continue_on_error").and_then(|v| v.as_bool()).unwrap_or(false);`.
- Seed `Standardowy pipeline TTS` SEED OUT w Etapie 1 — wraca razem z TTS-as-flow surface w Etapie 2. Czystsze niż "placeholder".
- `teams-flow` wycięty z grupy TTS-blocked w Otwarte ryzyka — to `trigger → llm → pii_filter → output`, nie używa TTS (codex zweryfikował `seed.rs:567,757`).
**Data:** 2026-05-06
**Codex session ID (review codu):** `019dfca1-fef1-7ca1-b154-b73a796670a8` (w `tentaflow-core/.context/codex-session-id`)

Plan v4.0 zastępuje całkowicie v3.x. Jest jeden zestaw typów, jeden kontrakt streaming, jedna sygnatura executora. Wszystkie sprzeczności z poprzednich rund usunięte.

---

## Kontekst

Aktualny `flow_engine/` ma scalar/single-pass executor:
- pojedynczy `FlowContext`, pojedynczy `final_output`, jeden producer streamu
- aktywacja node'a przez "co najmniej jedno aktywne wejście" (`executor_async.rs:233`)
- adaptery przekazują `serde_json::Value` jako wszystko (text/audio/image/json — typeless)
- node'y używają `ServiceManager` jako god-object zamiast narrow capability traits
- mutowalny `FlowContext.messages` jest niejawnym kanałem komunikacji adapterów z LLM

Etap 1 buduje fundament typed envelope + BlobRef + capability traits + immutable graph snapshot. Bez typed portów (zostają loose `full`/`in`), bez multi-modality LLM, bez Many/loop. To czysty refactor execution modelu.

---

## Hard rules

1. **Strict 1-input-edge dla każdego node'a.** Walidacja egzekwowana w `validation.rs` (dla save) i `CompiledFlow::compile` (dla load z DB). Multi-input wraca w Etapie 3 razem z merge node'ami i jawnymi semantykami.
2. **Brak `ServiceManager` ani `LiveHandlesCache` w adapterach.** Adaptery widzą tylko narrow capability traits.
3. **`NodeInput { envelope: Arc<FlowEnvelope> }`** — fan-out bez kopiowania artifacts/meta/trace.
4. **`schema_version: u16 = 1`** placeholder w FlowEnvelope.
5. **Brak `FlowFrame`/`FlowValue::List`/`FrameRef`.** Cardinality 1:1 zawsze.
6. **Brak `activate-on-any`.** Każdy node jest oczekiwany przez topological order, brak speculative activation.
7. **Replay determinism świadomie odrzucony.** Adaptery dalej czytają live DB/settings.
8. **Streaming jest cechą flow, nie adaptera.** `NodeAdapter` ma JEDNĄ metodę `execute(...) -> FlowEnvelope` (blocking). Streaming branch w executorze pomija adaptera LLM i woła `LlmDispatcher::stream_chat` bezpośrednio, używając `LlmAdapter::prepare_request` do zbudowania `LlmRequest` z envelope.
9. **Routing-side finalizer jest background-only.** Routing handler nie czeka na `outcome_receiver`. Slow persist nie blokuje klienta. Trailery / response headers po outcome dochodzą dopiero w Etapie 2 jeśli będą potrzebne.

---

## Co usuwamy

- `FlowContext` (pola idą do `FlowEnvelope.meta` lub `ExecutionContext`)
- `FlowStepLog` (zastępuje `TraceStep`)
- `FlowExecutionResult` (zastępuje `FlowExecutionOutcome`)
- `ParsedFlow` (zastępuje `CompiledFlow`)
- `executor_async.rs::run_streaming_flow` w obecnej formie pasywnego forwardera (zastąpiony aktywnym finalizer task'iem)

## Co NIE robimy w Etapie 1

- typed porty (`FlowEdge.data_type`)
- ArtifactKey registry
- multi-modality LLM nodes
- CEL conditions
- frame-aware streaming (zostaje single-stream-producer pattern, jak dziś)
- compiled flow persistence (replay z DB)
- blob GC scheduling (interfejs ma stub)
- Many/loop/batch/merge
- TTS-as-flow (dziś `executor.rs:1535` zwraca `"TTS via flow not supported yet"` — plan v4.0 ten stan utrzymuje; TTS-as-flow zostaje na Etap 2+)
- Trailery/headers po `outcome_receiver` w streaming response

---

## Typy

### `FlowEnvelope` i payload

```rust
pub struct FlowEnvelope {
    pub schema_version: u16,                        // = 1
    pub payload: FlowValue,                         // typed, główny przepływ
    pub artifacts: HashMap<String, FlowValue>,      // add-only bag
    pub provenance: HashMap<String, ArtifactProvenance>,
    pub context: ConversationContext,               // appendable conversation state
    pub meta: BTreeMap<String, serde_json::Value>,  // request_id, locale, timestamps
    pub trace: Vec<TraceStep>,
}

pub enum FlowValue {
    Empty,
    Text(String),
    Json(serde_json::Value),
    Audio { blob_ref: BlobRef, mime: String, sample_rate: Option<u32> },
    Image { blob_ref: BlobRef, mime: String, dims: Option<(u32, u32)> },
    Video { blob_ref: BlobRef, mime: String, duration_ms: Option<u64> },
    Embedding(Vec<f32>),
}

pub struct BlobRef {
    pub id: String,         // uuid
    pub size_bytes: u64,
    pub mime: String,
    pub sha256: String,
}

pub struct ArtifactProvenance {
    pub producer_node_id: String,
    pub producer_node_type: String,
    pub timestamp_ms: u64,
}
```

### `ConversationContext` (mutable conversation state)

```rust
pub struct ConversationContext {
    pub messages: Vec<ChatMessage>,         // pełna historia konwersacji
    pub system_prompts: Vec<String>,        // każdy = osobny System message przy build
}

pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
}

pub enum ChatRole { System, User, Assistant, Tool }
```

**Reguły context:**
- Trigger adapter seeduje `envelope.context.messages` z `request.messages` (dziś `routing/mod.rs:46` w `build_flow_context_inner`).
- Adaptery memory/conversation_history/session_context/speaker_context dopisują system_prompts/messages (append-only z perspektywy adaptera).
- LLM adapter buduje request: `final_messages = system_prompts.iter().map(System) ++ messages`. Każdy system_prompt = osobny System message w ustalonej kolejności (nie sklejać).
- Flatten dopiero w `dispatchers/llm.rs` jeśli backend wymaga (np. niektóre OpenAI-compat proxy odrzucają multiple System).
- `payload` (Text z trigger lub po STT) NIE jest auto-appendowany. LLM adapter dopisuje `User(payload.text)` tylko jeśli ostatnia message nie jest tym samym tekstem.

### `TraceStep` i `TokenUsage`

```rust
pub struct TraceStep {
    pub node_id: String,
    pub node_type: String,
    pub started_at_ms: u64,
    pub duration_ms: u64,
    pub status: TraceStatus,
    pub usage: Option<TokenUsage>,          // tylko dla llm/embeddings
}

pub enum TraceStatus {
    Ok,
    Skipped,
    Error(String),
}

pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}
```

### `FlowExecutionOutcome`

```rust
pub struct FlowExecutionOutcome {
    pub final_envelope: FlowEnvelope,
    pub trace: Vec<TraceStep>,
    pub usage: TokenUsage,                  // aggregate po wszystkich llm/embeddings nodes
    pub finish_reason: FinishReason,        // mapowanie z ostatniego LLM albo z statusu flow
    pub total_latency_ms: i64,
    pub error: Option<String>,
}

pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Cancelled,
    Error,
}
```

`finish_reason` agregowany: ostatni LLM chunk dostarcza wartość → executor zapisuje. Brak LLM w flow → `Stop` jeśli `error.is_none()`, `Error` jeśli error. Cancel via `cancel_token` → `Cancelled`.

### Definition / Compiled

```rust
pub struct FlowDefinition {
    pub nodes: Vec<FlowNode>,
    pub edges: Vec<FlowEdge>,
}

pub struct FlowNode {
    pub id: String,
    pub node_type: String,
    pub config: serde_json::Value,
    pub position: Option<(f64, f64)>,
    pub label: Option<String>,
}

pub struct FlowEdge {
    pub id: Option<String>,
    pub from_node: String,
    pub to_node: String,
    pub from_port: String,
    pub to_port: String,
    pub condition: Option<String>,
    pub label: Option<String>,
}

pub struct CompiledFlow {
    pub flow_id: i64,                       // przynależność do DB row, NIE w FlowDefinition
    pub definition: FlowDefinition,
    pub execution_order: Vec<usize>,        // toposort
    pub incoming_edges_per_pos: Vec<Vec<usize>>,
    pub node_pos_by_id: HashMap<String, usize>,
    pub is_streaming: bool,                 // detekcja w compile, nie runtime scan
}

pub struct NodeInput {
    pub from_node_id: String,
    pub from_port: String,
    pub envelope: Arc<FlowEnvelope>,
}
```

### Streaming delta

```rust
pub struct LlmStreamChunk {
    pub text_delta: String,
    pub reasoning_delta: Option<String>,    // dla modeli z reasoning_content (Qwen, DeepSeek-R1)
    pub usage: Option<TokenUsage>,          // tylko ostatni chunk
    pub finish_reason: Option<FinishReason>,
}

pub enum EnvelopeDelta {
    Llm(LlmStreamChunk),                    // single variant — przygotowane pod future rozbudowę
}
```

`EnvelopeDelta` nigdy nie niesie `Final(FlowEnvelope)`. Terminalny stan dochodzi przez `outcome_receiver.await` po EOF streamu.

---

## BlobStore

```rust
#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn put(&self, bytes: Bytes, mime: &str) -> Result<BlobRef>;
    async fn get(&self, blob_ref: &BlobRef) -> Result<Bytes>;
    async fn delete(&self, blob_ref: &BlobRef) -> Result<()>;
    async fn gc(&self, retention: Duration) -> Result<u64>;     // stub w Etapie 1
}

pub struct FileBlobStore { root: PathBuf }
pub struct InMemoryBlobStore { /* HashMap<id, Bytes> */ }
```

`FileBlobStore` path: `<tentaflow_home>/blobs/<sha2[0:2]>/<sha2[2:4]>/<full_sha2>.bin`.

Powody filesystem zamiast SQLite BLOB: audio/video to GB; SQLite BLOB ma write perf issues > 1MB; filesystem page cache za darmo; GC = `rm -rf orphans`; backup = `rsync`.

---

## Capability dispatchers

Cienkie wrappery nad istniejącymi typami. DTO flow-engine, nie surowe `ChatCompletionRequest/Response`.

### `LlmDispatcher`

```rust
#[async_trait]
pub trait LlmDispatcher: Send + Sync {
    async fn execute_chat(&self, req: LlmRequest) -> Result<LlmResponse>;
    async fn stream_chat(&self, req: LlmRequest) -> Result<BoxStream<'static, Result<LlmStreamChunk>>>;
}

pub struct LlmRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,         // już zbudowane przez adapter z envelope.context
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub stop: Vec<String>,
    pub deadline: Option<Instant>,
    pub cancel_token: CancellationToken,
}

pub struct LlmResponse {
    pub content: String,
    pub usage: TokenUsage,
    pub finish_reason: FinishReason,
}
```

Wrapper w `dispatchers/llm.rs` mapuje DTO ↔ `services/runtime/executor.rs::execute_chat` (`:172`) / `stream_chat` (`:253`). Mapowanie `StreamChunkType → LlmStreamChunk`: `ContentDelta → text_delta`, `ReasoningDelta → reasoning_delta`, `Done → usage + finish_reason`.

### Pozostałe dispatchery

| Trait | Wraps |
|-------|-------|
| `EmbeddingsDispatcher` | `executor.rs::execute_embeddings:995` |
| `TtsDispatcher` | `executor.rs::execute_tts:1289` |
| `SttDispatcher` | `services/stt/runtime.rs:34` (bezpośrednio, omijając `executor.rs::execute_stt:1545`) |
| `PromptStore` | `prompt_registry/mod.rs::SharedPromptRegistry:312` |
| `Clock` | `chrono::Utc::now()` |
| `AuditSink` | `repository::log_audit` |
| `MetricsSink` | placeholder no-op |
| `MemoryStore` | nowy trait — wrapuje `find_quic_client_for_model("memory")` z dziś używanego w `adapters/memory.rs:34` |
| `ConversationHistoryStore` | nowy trait — wrapuje `conversation_cache` z `adapters/conversation_history.rs:43` |

`SessionContextStore` i `SpeakerStore` NIE są potrzebne — adaptery `session_context` i `speaker_context` używają wyłącznie `prompt_registry` (`session_context.rs:87`, `speaker_context.rs:31`), więc `PromptStore` wystarcza.

---

## NodeAdapter

```rust
#[async_trait]
pub trait NodeAdapter: Send + Sync {
    fn node_type(&self) -> &str;
    fn supported_input_ports(&self) -> &[&str];
    fn supported_output_ports(&self) -> &[&str];

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],          // strict 1-input enforced w validation
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope>;
}

pub struct ExecutionContext {
    pub request_id: String,
    pub execution_id: i64,                  // v4.1: utworzony w execute_*, używany przy update_flow_execution
    pub session_id: Option<String>,
    pub user_id: Option<i64>,
    pub user_role: Option<String>,
    pub deadline: Option<Instant>,
    pub cancel_token: CancellationToken,
    pub clock: Arc<dyn Clock>,
    pub blobs: Arc<dyn BlobStore>,
    pub llm: Arc<dyn LlmDispatcher>,
    pub embeddings: Arc<dyn EmbeddingsDispatcher>,
    pub stt: Arc<dyn SttDispatcher>,
    pub tts: Arc<dyn TtsDispatcher>,
    pub prompts: Arc<dyn PromptStore>,
    pub memory: Arc<dyn MemoryStore>,
    pub history: Arc<dyn ConversationHistoryStore>,
    pub audit: Arc<dyn AuditSink>,
    pub metrics: Arc<dyn MetricsSink>,
    pub usage_sink: Arc<UsageSink>,         // wewnętrzny accumulator, executor agreguje
}
```

`NodeAdapter` ma JEDNĄ metodę. Streaming nie jest na poziomie adaptera (codex round 4 — usuwamy podwójne API). Adapter `LlmNodeAdapter` ma dodatkową concrete (nie-trait) metodę:

```rust
impl LlmNodeAdapter {
    pub fn prepare_request(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> LlmRequest;
}
```

**v4.1 — typed accessor zamiast downcast:**

```rust
pub struct AdapterRegistry {
    adapters: HashMap<String, Arc<dyn NodeAdapter>>,
    pub llm: Arc<LlmNodeAdapter>,           // typed pole obok mapy
}

impl AdapterRegistry {
    pub fn get(&self, node_type: &str) -> Option<&Arc<dyn NodeAdapter>>;
    pub fn llm(&self) -> &LlmNodeAdapter { &self.llm }
}
```

Executor w streaming branch:
1. wykonuje wszystkie nody przed LLM przez `registry.get(node_type)?.execute(...)` (jak blocking)
2. dla streaming LLM node: woła `registry.llm().prepare_request(node, inputs, &ctx)` → `LlmRequest`
3. potem `ctx.llm.stream_chat(req).await?` → `BoxStream<LlmStreamChunk>`

Bez downcastu, bez `prepare_llm_request` na traicie.

`UsageSink` to thin `Mutex<Vec<(String, TokenUsage)>>` — adaptery llm/embeddings pushują, executor zbiera na końcu do `FlowExecutionOutcome.usage` + `TraceStep.usage`.

---

## Validation & compile

`validation.rs` jest jedyną prawdą reguł. `CompiledFlow::compile` woła walidację jako pierwszy krok.

```rust
// flow_engine/validation.rs
pub fn validate(def: &FlowDefinition, registry: &AdapterRegistry) -> Result<(), ValidationError>;
```

Walidacja sprawdza:
- każdy node ma adapter (`registry`)
- każdy edge `from_port` ∈ producer's `supported_output_ports`, `to_port` ∈ consumer's `supported_input_ports`
- cycle detection (graph walk)
- strict 1-input-edge (każdy nie-trigger ma ≤1 incoming)
- trigger-uniqueness (dokładnie jeden trigger)
- condition edges: edge z `from_port == "true"|"false"` musi pochodzić z `condition` node
- streaming end-shape: jeśli jakiś node ma `config.stream == true`, musi to być `llm` z edge `stream → output.in` gdzie `output.config.mode == "stream"`, i żaden inny node nie może być po tym LLM (na ścieżce do output)

```rust
// flow_engine/cache.rs (CompiledFlow)
impl CompiledFlow {
    pub fn compile(
        flow_id: i64,
        definition: FlowDefinition,
        registry: &AdapterRegistry,
    ) -> Result<Self, CompileError> {
        validation::validate(&definition, registry)?;       // jedna prawda

        let execution_order = topo_sort(&definition);
        let incoming_edges_per_pos = build_adjacency(&definition);
        let node_pos_by_id = build_index(&definition);
        let is_streaming = detect_streaming(&definition);

        Ok(CompiledFlow {
            flow_id,
            definition,
            execution_order,
            incoming_edges_per_pos,
            node_pos_by_id,
            is_streaming,
        })
    }
}
```

Wywołania `compile()`:
- przy save flow w `dispatch/handlers.rs:118` (defense in depth)
- przy load flow z DB w dispatcherze (`dispatcher.rs:251` — przestaje robić tylko `ParsedFlow::parse`)
- jeden raz, wynik cachowany w `CompiledFlowCache` (już istnieje jako `cache.rs`)

`continue_on_error` na `trigger.config` (`types.rs:141`) honorowany w nowym executorze: jeśli `true`, błąd node'a → `TraceStatus::Error` w trace, executor propaguje envelope sprzed błędu do następnych. Default `false` = abort flow z `outcome.error: Some(...)`.

---

## Executor

Dwie funkcje (codex round 3 — czytelniejsze niż enum z arm streaming):

```rust
pub async fn execute_blocking(
    compiled: Arc<CompiledFlow>,
    initial: FlowEnvelope,
    ctx: ExecutionContext,
    adapters: Arc<AdapterRegistry>,
) -> Result<FlowExecutionOutcome>;

pub async fn execute_streaming(
    compiled: Arc<CompiledFlow>,
    initial: FlowEnvelope,
    ctx: ExecutionContext,
    adapters: Arc<AdapterRegistry>,
) -> Result<StreamingExecution>;

pub struct StreamingExecution {
    pub stream: BoxStream<'static, Result<EnvelopeDelta>>,
    pub outcome: oneshot::Receiver<FlowExecutionOutcome>,
}
```

### `execute_blocking`

1. **`let execution_id = repository::create_flow_execution(&db, compiled.flow_id, ...).await?;`** (v4.1 — przed wykonaniem). Wstawiamy execution_id do `ctx.execution_id`.
2. **continue_on_error:** odczyt z trigger node config:
   ```rust
   let trigger = compiled.find_trigger_node();
   let continue_on_error = trigger.config
       .get("continue_on_error")
       .and_then(|v| v.as_bool())
       .unwrap_or(false);
   ```
3. Topological execution po `compiled.execution_order`. Każdy node:
   - bierze `inputs: &[NodeInput]` (max 1 przez hard rule)
   - wywołuje `registry.get(&node.node_type)?.execute(node, inputs, ctx).await`
   - jeśli error: `if continue_on_error { trace.push(Error); continue }` else `break`
   - writes do `node_results: HashMap<String, Arc<FlowEnvelope>>`
   - pushuje `TraceStep` (status + usage z `ctx.usage_sink`)
4. Po topo loopie:
   - agreguje `usage` ze wszystkich TraceStep
   - wybiera ostatni `FlowEnvelope` (z node oznaczonego jako output, lub ostatni w toposort)
   - buduje `FlowExecutionOutcome { final_envelope, trace, usage, finish_reason, total_latency_ms, error }`
   - **`repository::update_flow_execution(&db, ctx.execution_id, status, &outcome).await?;`** (po `execution_id`, NIE po `flow_id`)

### `execute_streaming`

Egzekwowane tylko gdy `compiled.is_streaming == true`. Logika:

1. **`let execution_id = repository::create_flow_execution(&db, compiled.flow_id, ...).await?;`** (v4.1).
2. Wykonuje wszystkie nody PRZED streaming LLM przez topo (jak `execute_blocking`), kończąc na inputs streaming LLM node.
3. Buduje `LlmRequest` przez `registry.llm().prepare_request(llm_node, inputs, &ctx)` (v4.1 typed accessor).
4. Woła `ctx.llm.stream_chat(req).await?` → `BoxStream<LlmStreamChunk>`.
5. Spawnuje finalizer task, zwraca `StreamingExecution { stream: outbound_rx, outcome: outcome_rx }`.

```rust
// v4.1 finalizer task — disconnect-resilient + backpressure-resilient + execution_id-based persist
async fn finalize_streaming_flow(
    execution_id: i64,                               // v4.1: nie flow_id
    mut adapter_stream: BoxStream<Result<LlmStreamChunk>>,
    outbound_tx: mpsc::Sender<Result<EnvelopeDelta>>,
    outcome_tx: oneshot::Sender<FlowExecutionOutcome>,
    cancel: CancellationToken,
    mut builder: ResponseBuilder,
    db: Arc<Database>,
) {
    let mut error: Option<String> = None;
    let mut cancelled = false;

    'main: loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                cancelled = true;
                break 'main;
            }
            chunk = adapter_stream.next() => match chunk {
                Some(Ok(c)) => {
                    builder.absorb(&c);
                    // v4.1: send + cancel race — backpressure NIE blokuje cancel
                    tokio::select! {
                        biased;
                        _ = cancel.cancelled() => {
                            cancelled = true;
                            break 'main;
                        }
                        send_res = outbound_tx.send(Ok(EnvelopeDelta::Llm(c))) => {
                            // SendError = klient disconnect — ignorujemy, kontynuujemy żeby
                            // adapter_stream się dokończył lub cancel złapał kolejną iterację.
                            // (Drop CancelOnDropStream w routing wywołał cancel — kolejna iter
                            // złapie cancelled().)
                            let _ = send_res;
                        }
                    }
                }
                Some(Err(e)) => {
                    error = Some(format!("{e}"));
                    break 'main;
                }
                None => break 'main,                  // EOF
            }
        }
    }

    // 1) zamknij outbound — routing dostanie EOF
    drop(outbound_tx);

    // 2) zbuduj outcome
    let outcome = builder.build_outcome(
        if cancelled {
            (Some("cancelled".into()), FinishReason::Cancelled)
        } else if let Some(e) = error.clone() {
            (Some(e), FinishReason::Error)
        } else {
            (None, FinishReason::Stop)
        }
    );

    // 3) persist po execution_id — v4.1
    let _ = repository::update_flow_execution(
        &db,
        execution_id,
        if cancelled { "cancelled" } else if outcome.error.is_some() { "error" } else { "completed" },
        &outcome,
    ).await;

    // 4) wyślij outcome
    let _ = outcome_tx.send(outcome);
}
```

**Twardy invariant:** `drop(outbound_tx)` zawsze przed `outcome_tx.send`. Routing widzi EOF zanim outcome dochodzi.

**v4.1 race-free:** wewnętrzny `select!` przy `outbound_tx.send` z `cancel.cancelled()` — bounded channel + zatkany consumer NIE blokuje cancel ani `update_flow_execution`. Cancel propaguje przez `CancelOnDropStream` z routing layer.

---

## Routing

`routing/chat.rs::route_chat_completion`:
- woła `flow_dispatcher.execute_blocking(...).await?`
- konwertuje `FlowExecutionOutcome` → `ChatCompletionResponse` przez `flow_outcome_to_chat_response`
- bare passthrough (gdy brak flow dla modelu) zostaje

`routing/streaming.rs::route_chat_completion_stream` (background-only finalizer model):

```rust
pub async fn route_chat_completion_stream(
    req: ChatCompletionRequest,
    ctx: ExecutionContext,
    flow_dispatcher: Arc<FlowDispatcher>,
) -> Result<BoxStream<'static, Result<ChatCompletionChunk, AppError>>, AppError> {
    let StreamingExecution { stream: envelope_stream, outcome } =
        flow_dispatcher.execute_streaming(req.into_initial_envelope(), ctx.clone()).await?;

    let meta = StreamChunkMeta {
        chat_id: generate_response_id(),
        created: unix_timestamp(),
        model: req.model.clone(),
    };

    // background-only finalizer: log/audit po outcome, NIE blokuje response
    tokio::spawn(async move {
        match outcome.await {
            Ok(o) => tracing::info!(
                latency_ms = o.total_latency_ms,
                prompt_tokens = o.usage.prompt_tokens,
                completion_tokens = o.usage.completion_tokens,
                error = ?o.error,
                "flow streaming completed"
            ),
            Err(_) => tracing::warn!("flow finalizer dropped without outcome"),
        }
    });

    // bridge envelope_stream → chat_chunk_stream
    let chat_stream = envelope_stream.map(move |delta_result| {
        delta_result
            .map(|delta| envelope_delta_to_chat_chunk(delta, &meta))
            .map_err(AppError::from)
    });

    Ok(Box::pin(chat_stream))
}

pub fn envelope_delta_to_chat_chunk(
    delta: EnvelopeDelta,
    meta: &StreamChunkMeta,
) -> ChatCompletionChunk {
    let EnvelopeDelta::Llm(LlmStreamChunk { text_delta, reasoning_delta, finish_reason, usage: _ }) = delta;
    // usage nie leci do klienta w streamie (parytet z dziś `executor.rs:398` Done.final_metrics:_)
    ChatCompletionChunk {
        id: meta.chat_id.clone(),
        object: "chat.completion.chunk".to_string(),
        created: meta.created,
        model: meta.model.clone(),
        choices: vec![ChunkChoice {
            index: 0,
            delta: Delta {
                role: None,
                content: if text_delta.is_empty() { None } else { Some(text_delta) },
                reasoning_content: reasoning_delta,
                tool_calls: None,
            },
            finish_reason: finish_reason.map(|f| f.to_string()),
            logprobs: None,
        }],
        system_fingerprint: None,
        audio: None,
        detected_intent: None,
        detected_tools: None,
        transcribed_text: None,
        speaker_id: None,
        speaker_name: None,
    }
}
```

**Disconnect detection (v4.1 — codex round 5):** `api/openai/server.rs:303` używa ręcznego `StreamBody`, nie `axum::Sse`. `Sse::on_close` nie istnieje. Zamiast tego — wrapper `CancelOnDropStream` z `Drop` impl:

```rust
// flow_engine/cancel_on_drop.rs (nowy, ~30 LOC)
pub struct CancelOnDropStream<S> {
    inner: S,
    cancel: Option<CancellationToken>,      // Option żeby Drop mogło take()
}

impl<S> CancelOnDropStream<S> {
    pub fn new(inner: S, cancel: CancellationToken) -> Self {
        Self { inner, cancel: Some(cancel) }
    }
}

impl<S: Stream + Unpin> Stream for CancelOnDropStream<S> {
    type Item = S::Item;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

impl<S> Drop for CancelOnDropStream<S> {
    fn drop(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.cancel();                     // idempotent
        }
    }
}
```

W `routing/streaming.rs::route_chat_completion_stream`:

```rust
let cancel_for_drop = ctx.cancel_token.clone();
let chat_stream = envelope_stream.map(move |delta_result| { ... });
Ok(Box::pin(CancelOnDropStream::new(chat_stream, cancel_for_drop)))
```

Gdy klient disconnectuje, hyper droppuje response body → `CancelOnDropStream::drop` → `cancel_token.cancel()` → finalizer w executor widzi w pętli `cancel.cancelled()` (biased select). Persist `status='cancelled'` wykonuje się.

`server.rs:303` zostaje bez zmian — dalej zwraca ręczny `StreamBody`, ale nad tym StreamBody jest już `CancelOnDropStream` wrappujący wyjście z routing layer.

**Czyste warstwy:**
- `executor` finalizer: persist `flow_executions`, emit outcome, disconnect-resilient
- `routing/streaming.rs`: bridge EnvelopeDelta → ChatCompletionChunk, spawn detached log/audit task po outcome
- `api/openai/server.rs`: axum SSE wrapper z `on_close → cancel`, dopisuje `[DONE]` po EOF (już to robi)

Klient nie czeka na żaden post-stream task. Routing zwraca stream natychmiast po `execute_streaming`.

---

## Konwertery `FlowExecutionOutcome` → response

### Chat (blocking + non-streaming)

```rust
// flow_engine/converter.rs (full rewrite)
pub fn flow_outcome_to_chat_response(outcome: &FlowExecutionOutcome, model: &str) -> Value {
    let content = match &outcome.final_envelope.payload {
        FlowValue::Text(t) => Cow::Borrowed(t.as_str()),
        FlowValue::Empty => Cow::Borrowed(""),
        other => Cow::Owned(serde_json::to_string(other).unwrap_or_default()),
    };
    serde_json::json!({
        "id": generate_response_id(),
        "object": "chat.completion",
        "created": unix_timestamp(),
        "model": model,
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": content },
            "finish_reason": finish_reason_to_str(&outcome.finish_reason),
        }],
        "usage": {
            "prompt_tokens": outcome.usage.prompt_tokens,
            "completion_tokens": outcome.usage.completion_tokens,
            "total_tokens": outcome.usage.total_tokens,
        }
    })
}

fn finish_reason_to_str(r: &FinishReason) -> Value {
    match r {
        FinishReason::Stop => Value::String("stop".into()),
        FinishReason::Length => Value::String("length".into()),
        FinishReason::ToolCalls => Value::String("tool_calls".into()),
        FinishReason::ContentFilter => Value::String("content_filter".into()),
        FinishReason::Cancelled | FinishReason::Error => Value::Null,
    }
}
```

### Embeddings

Format dziś (`executor.rs:1714`): `data: Vec<{ embedding: [f32], index, object: "embedding" }>` z twardym cardinality check.

```rust
pub fn flow_outcome_to_embedding_response(
    outcome: &FlowExecutionOutcome,
    model: &str,
) -> Result<EmbeddingsResponse, AppError> {
    let data = match &outcome.final_envelope.payload {
        FlowValue::Embedding(v) => vec![EmbeddingObject {
            embedding: v.clone(),
            index: 0,
            object: "embedding".to_string(),
        }],
        FlowValue::Json(v) => parse_embedding_batch(v)?,
        FlowValue::Empty => vec![],
        other => return Err(AppError::Internal(format!(
            "embedding flow returned unexpected payload: {:?}", other
        ))),
    };
    Ok(EmbeddingsResponse {
        object: "list".into(),
        data,
        model: model.into(),
        usage: EmbeddingsUsage {
            prompt_tokens: outcome.usage.prompt_tokens,
            total_tokens: outcome.usage.total_tokens,
        },
    })
}

// Format batch JSON wybrany w v4.0:
//   { "embeddings": [[f32; D], [f32; D], ...] }
// Adapter embeddings dla batch input zwraca FlowValue::Json z dokładnie tym kształtem.
fn parse_embedding_batch(v: &serde_json::Value) -> Result<Vec<EmbeddingObject>, AppError> {
    let arr = v.get("embeddings")
        .and_then(|e| e.as_array())
        .ok_or_else(|| AppError::Internal("embedding batch missing 'embeddings' field".into()))?;
    arr.iter().enumerate().map(|(i, vec_val)| {
        let embedding = vec_val.as_array()
            .ok_or_else(|| AppError::Internal(format!("embedding[{i}] not array")))?
            .iter()
            .map(|x| x.as_f64().map(|f| f as f32))
            .collect::<Option<Vec<f32>>>()
            .ok_or_else(|| AppError::Internal(format!("embedding[{i}] non-numeric")))?;
        Ok(EmbeddingObject { embedding, index: i, object: "embedding".to_string() })
    }).collect()
}
```

### TTS (skip w Etapie 1)

`services/runtime/executor.rs:1535` zwraca dziś `"TTS via flow not supported yet"`. Plan v4.0 ten stan utrzymuje. Konwerter `flow_outcome_to_tts_response` NIE jest dodawany. TTS-as-flow surface dochodzi w Etapie 2+ razem z multimodal triggerami.

---

## Call site refactor map (kompletna)

### Production code

| Plik | Linia | Co siedzi | Zmiana |
|------|-------|-----------|--------|
| `routing/chat.rs` | call site `dispatch_by_flow_id` | przyjmuje `FlowExecutionResult` | → `flow_dispatcher.execute_blocking(...)`, `flow_outcome_to_chat_response` |
| `routing/streaming.rs` | `route_chat_completion_stream:232` | zwraca `BoxStream<ChatCompletionChunk>` | → tak samo, ale bridge przez `EnvelopeDelta::Llm`, detached log task |
| `services/runtime/executor.rs` | 788 | `dispatch_by_flow_id` blocking chat 1 | → `execute_blocking(...)` + nowy konwerter |
| `services/runtime/executor.rs` | 1258 | blocking chat 2 | jak wyżej |
| `services/runtime/executor.rs` | 1720 | embedding response builder | → `flow_outcome_to_embedding_response` |
| `flow_engine/dispatcher.rs` | 160 | `dispatch_by_flow_id` | → `execute_blocking` |
| `flow_engine/dispatcher.rs` | 226 | `dispatch_streaming_by_flow_id` | → `execute_streaming` |
| `flow_engine/dispatcher.rs` | 251-263 | `ParsedFlow::parse` przy load z DB | → `CompiledFlow::compile(flow_id, def, registry)` |
| `flow_engine/dispatcher.rs` | 278-343 | streaming path forwarder | usunięty (logika idzie do executora) |
| `flow_engine/executor_async.rs` | 205 | `flow_executions::create` | → finalizer task |
| `flow_engine/executor_async.rs` | 365 | `flow_executions::update` | → finalizer task |
| `flow_engine/executor_async.rs` | 402-529 | streaming forwarder | usunięty (zastąpiony aktywnym finalizerem) |
| `flow_engine/converter.rs` | cały plik | `flow_result_to_chat_response`, `extract_text_from_output` | → przepisany pod `FlowExecutionOutcome`, `flow_result_to_stream_chunk` (dead_code) wycięty całkowicie |
| `flow_engine/cache.rs` | wszystkie use'a `ParsedFlow` | | → `CompiledFlow` |

### Tests

| Plik | Linia | Co | Akcja |
|------|-------|-----|-------|
| `flow_engine/converter.rs` | 102+ | testy `flow_result_to_chat_response` | przepisać pod `flow_outcome_to_chat_response` z `FlowExecutionOutcome` fixture |
| `flow_engine/executor_async.rs` | 823+ | mock adapters + test executora | przepisać pod nowy `NodeAdapter`, `FlowEnvelope`, fake dispatchery |
| `flow_engine/executor_async.rs` | 1143-1144, 1209-1210 | testy z join 2 branche → output | usunąć (multi-input zakazany hard rule) ALBO przepisać jako condition-branch (false branch propaguje envelope dalej) |
| `services/runtime/executor.rs` | 1957+ | testy embedding response | fixture `FlowExecutionOutcome` z `FlowValue::Embedding` |

### Bare passthrough (zostaje bez zmian)

Wszystkie ścieżki w `routing/chat.rs` / `routing/streaming.rs` które idą bezpośrednio do backend LLM gdy brak flow dla modelu — zostają. To bypass flow_engine, więc refactor `FlowExecutionOutcome` ich nie dotyczy.

---

## Adaptery — kolejność implementacji

13 adapterów do przepisania. Kolejność krytyczna — najpierw "hard" żeby skontraktować input/output shape:

1. **llm** (najtrudniejszy, definiuje main contract + `prepare_request`)
2. **stt**
3. **tts**
4. **embeddings**
5. **condition**
6. **output**
7. **pii_filter**
8. **tts_clean**
9. **memory** (decouple od ServiceManager)
10. **conversation_history** (decouple)
11. **session_context** (używa tylko PromptStore)
12. **speaker_context** (używa tylko PromptStore)
13. **trigger** (seeduje envelope.context.messages z request.messages)

---

## DB

Nic nowego nie dodajemy. `flow_executions` używamy dalej do logu (z `Vec<TraceStep>` serializowanym do JSON w `execution_log`). BlobStore = filesystem, nie DB.

**v4.1 lifecycle `flow_executions` dla streamingu:**
- `repository::create_flow_execution(&db, flow_id, request_id, ...)` na początku `execute_blocking`/`execute_streaming` — zwraca `execution_id: i64` (auto-increment row id z `db/repository.rs:1881`).
- `execution_id` umieszczony w `ExecutionContext.execution_id`.
- Finalizer task dostaje `execution_id` przez parametr (nie przez ctx — finalizer może wisieć po cancel ctx).
- `repository::update_flow_execution(&db, execution_id, status, &outcome)` po zakończeniu (normalnym/cancel/error) — update po `execution_id`, NIE po `flow_id` (`flow_id` jest non-unique).

**Seed cleanup (v4.1 — codex round 5):**
- Seed `Standardowy pipeline TTS` USUNIĘTY z `db/seed.rs` w ramach refactor (dodać komentarz: "TTS-as-flow wraca w Etapie 2, dziś TTS idzie wyłącznie przez `executor.rs::execute_tts` mesh path").

User wykasuje stare bazy ręcznie przed pierwszym startem:
```
rm /home/critix/repos/rust/TentaFlow/tentaflow/target/debug/data/router.db*
```

---

## Test strategy

- **Executor unit:** mock adapters + fake dispatchers (fake `LlmDispatcher` z scripted stream)
- **DB:** `db::init(Path::new(":memory:"))` (już używane wszędzie)
- **Seed validation:** registry z fake capability traits, BEZ bootowania `Router`. Test `seeded_flows_pass_adapter_validation` musi przejść z nową validation (cycle/1-input/trigger-unique).
- **Streaming finalizer:** dedykowane testy:
  - happy path (EOF → outcome z `FinishReason::Stop`)
  - cancel mid-stream (`cancel_token.cancel()` → outcome z `FinishReason::Cancelled`, persist `status='cancelled'`)
  - disconnect mid-stream (drop `outbound_rx`, finalizer kontynuuje `let _ = send`, persist OK, outcome dochodzi)
  - error mid-stream (adapter stream Err → outcome z `FinishReason::Error`)
- **Routing:** 1-2 cienkie integration tests
- **`tests/flow_engine_v2.rs` (NEW):** seedowane flows (Standardowy pipeline LLM, Default Chat, Audio Chat) wykonują się end-to-end z fake dispatchers

---

## Pliki / LOC estymata

| Plik | Akcja | Δ LOC |
|------|-------|-------|
| `flow_engine/types.rs` | full rewrite | ~400 |
| `flow_engine/blob_store.rs` | NEW | +200 |
| `flow_engine/dispatchers/mod.rs` | NEW | +40 |
| `flow_engine/dispatchers/llm.rs` | NEW (z stream_chat) | +150 |
| `flow_engine/dispatchers/stt.rs` | NEW | +60 |
| `flow_engine/dispatchers/tts.rs` | NEW | +60 |
| `flow_engine/dispatchers/embeddings.rs` | NEW | +60 |
| `flow_engine/dispatchers/prompts.rs` | NEW | +40 |
| `flow_engine/dispatchers/memory.rs` | NEW (decouple) | +120 |
| `flow_engine/dispatchers/conversation.rs` | NEW (decouple) | +100 |
| `flow_engine/dispatchers/clock.rs` | NEW | +30 |
| `flow_engine/adapters/mod.rs` | rewrite | ~150 |
| `flow_engine/adapters/llm.rs` | rewrite (+ prepare_request) | ~450 (z 719) |
| `flow_engine/adapters/stt.rs` | rewrite | ~150 (z 240) |
| `flow_engine/adapters/tts.rs` | rewrite | ~120 (z 210) |
| `flow_engine/adapters/embeddings.rs` | rewrite | ~120 (z 201) |
| `flow_engine/adapters/condition.rs` | rewrite | ~100 (z 179) |
| `flow_engine/adapters/output.rs` | rewrite | ~50 (z 87) |
| `flow_engine/adapters/pii_filter.rs` | rewrite | ~70 (z 109) |
| `flow_engine/adapters/tts_clean.rs` | rewrite | ~40 (z 55) |
| `flow_engine/adapters/memory.rs` | rewrite | ~250 (z 403) |
| `flow_engine/adapters/conversation_history.rs` | rewrite | ~80 (z 124) |
| `flow_engine/adapters/session_context.rs` | rewrite | ~80 (z 123) |
| `flow_engine/adapters/speaker_context.rs` | rewrite | ~150 (z 234) |
| `flow_engine/adapters/trigger.rs` | rewrite (seed messages) | ~60 (z 49) |
| `flow_engine/dispatcher.rs` | refactor (load → CompiledFlow::compile) | ~200 (z 486) |
| `flow_engine/validation.rs` | rewrite + nowe reguły | ~280 (z 310) |
| `flow_engine/executor_async.rs` | full rewrite (execute_blocking + execute_streaming + finalizer) | ~900 (z 1771) |
| `flow_engine/cache.rs` | refactor pod CompiledFlow | ~120 (z 233) |
| `flow_engine/converter.rs` | rewrite (chat + embeddings, BEZ streaming converter) | ~120 (z 301) |
| `routing/chat.rs` | call sites | ~30 |
| `routing/streaming.rs` | call sites + bridge | ~100 |
| `api/openai/server.rs` | on_close handler | ~10 |
| `services/runtime/executor.rs` | call sites + nowe konwertery | ~80 |
| `tests/flow_engine_v2.rs` | NEW | +400 |

**Razem: ~4000-5000 LOC zmian.** Większość to rewrite (LOC redukcja w wielu plikach), nie net-new.

---

## Workflow (zatwierdzony przez user)

1. Plan szczegółowy → codex review (rundy iteratywne)
2. Iteracja planu aż codex passuje (✅ to teraz round 5 z v4.0)
3. **Implementacja** (po passie)
4. Codex review codu (post-impl, fresh session via `/codex resume <id>`)
5. Iteracja codu aż codex passuje
6. Commit + push
7. Następny etap

---

## Etap 2 i 3 (zarys)

**Etap 2 — Typed porty + ArtifactKey registry + TTS-as-flow:**
- `FlowEdge.data_type` (`text`/`audio`/`image`/`video`/`embedding`/`json`)
- Walidacja port matching przy save flow
- `ArtifactKey` registry z deklaracjami producent → klucz → typ
- GUI canvas: kolor portu = typ, walidacja edge przy łączeniu
- TTS-as-flow surface (executor.rs:1535 ResolvedExecutionTarget::Flow → real path)
- Trailery / response headers po `outcome_receiver` (jeśli klient prosi via header)

**Etap 3 — Multi-modality + Loop/Merge + CEL + frame-aware streaming:**
- `TextLlm`/`VisionLlm`/`OmniLlm` jako 3 osobne node types
- Multimodal trigger (N output portów per typ)
- `FlowFrame::Many`, `FrameRef { lineage: Vec<String>, depth }`
- Merge node z explicit policies (zip/concat/cartesian/first/join-by-key)
- ForEach loop z scope/frame_id
- IF/Switch z CEL expression language (`cel-rs`)
- frame-aware streaming
- compiled flow persistence (replay)

---

## Po /compact — co czytać żeby ruszyć implementację

1. Ten plik: pełny plan v4.0 (decyzje + checklisty)
2. `tentaflow-core/src/flow_engine/types.rs` — aktualny stan typów (do podmiany)
3. `tentaflow-core/src/flow_engine/executor_async.rs` — aktualny executor (1771 LOC)
4. `tentaflow-core/src/flow_engine/adapters/mod.rs` — aktualny `NodeAdapter` trait
5. `tentaflow-core/src/services/runtime/executor.rs` — `ModelRuntimeExecutor` (cel wrappera dla dispatchers)
6. `tentaflow-core/src/services/stt/runtime.rs` — `SttRuntime`
7. `tentaflow-core/src/api/openai/server.rs:280` — gdzie axum buduje SSE response
8. `tentaflow-core/src/routing/streaming.rs:232` — `route_chat_completion_stream`
9. `tentaflow-core/.context/codex-session-id` — session ID do `/codex resume` przy review codu

**Pierwsze tasks:**
1. `cargo build` w `tentaflow/` — sprawdzić zielony baseline (✅ potwierdzone v4.0)
2. `flow_engine/types.rs` — full rewrite z nowymi typami
3. `flow_engine/blob_store.rs` (NEW) — trait + 2 impls + tests
4. Compile check po każdym kroku

---

## Otwarte ryzyka (świadomie zaakceptowane)

1. **Brak replay determinism** — adaptery czytają live DB. OK w v1.
2. **Trace zgubiony przy panic OS** — finalizer task spawnowany, więc OS-level kill może uciąć przed persist. OK w v1, real OOM/SIGKILL = wyższy poziom problemu.
3. **Big rewrite (~4-5k LOC)** w jednym etapie — ryzyko, że coś przeoczę. Mitygacja: codex review po impl + testy finalizera (cancel/disconnect/error).
4. **TTS-as-flow blok** — flow `Standardowy pipeline TTS` zostaje WYCIĘTY z seed (v4.1 — codex round 5). Wraca razem z TTS-as-flow surface w Etapie 2. `teams-flow` NIE używa TTS (`seed.rs:567,757` — to `trigger → llm → pii_filter → output`), więc nie jest dotknięty. Mesh path TTS (`executor.rs::execute_tts:1289`) działa dalej bez zmian, bo to nie jest flow.
5. **Decouple memory/conversation_history od `ServiceManager`** — może wymagać szerszych zmian. Jeśli za duże, wyciągnąć w osobny etap.
