// =============================================================================
// Plik: flow_engine/dispatcher.rs
// Opis: FlowDispatcher — brama wejściowa flow engine. Bootstrap'uje
//       AdapterRegistry (13 node adapters) + ContextFactory (10 dispatcher
//       impls + blob store + clock + metrics). Eksponuje try_dispatch /
//       dispatch_by_flow_id / try_dispatch_streaming dla callerów (routing,
//       services::runtime::executor).
// =============================================================================

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::auth::acl;
use crate::db::{repository, DbPool};
use crate::flow_engine::blob_store::BlobStore;
use crate::flow_engine::cache::{CachedFlow, CompiledFlow, FlowCache};
use crate::flow_engine::dispatchers::clock::SystemClock;
use crate::flow_engine::dispatchers::{
    AuditSink, Clock, ConversationHistoryStore, DocumentsDispatcher, EmbeddingsDispatcher,
    LlmDispatcher, MemoryStore, MetricsSink, NoopMetrics, NoopProgress, PiiRulesStore,
    ProgressSink, PromptStore, RerankDispatcher, SttDispatcher, TtsCleaningStore, TtsDispatcher,
    VisionDispatcher,
};
use crate::flow_engine::dispatchers_impl::{
    AuditSinkImpl, ConversationHistoryImpl, DocumentsDispatcherImpl, EmbeddingsDispatcherImpl,
    LlmDispatcherImpl, MemoryStoreImpl, ModelRuntimeSlot, PiiRulesStoreImpl, PromptsImpl,
    RerankDispatcherImpl, ServiceManagerQuicFinder, SttDispatcherImpl, TtsCleaningStoreImpl,
    TtsDispatcherImpl, VisionDispatcherImpl,
};
use crate::flow_engine::envelope::{
    AudioStreamChunk, EnvelopeDelta, FlowEnvelope, FlowExecutionOutcome, FlowValue, LlmStreamChunk,
};
use crate::flow_engine::executor::{
    execute_blocking, execute_direct_blocking, execute_direct_streaming, execute_streaming,
    StreamingExecution,
};
use crate::flow_engine::node_adapter::{AdapterRegistry, ExecutionContext, NodeAdapter, UsageSink};
use crate::flow_engine::node_adapters::{
    AgentContextNodeAdapter, AgentNodeAdapter, AgentRouterNodeAdapter, AskUserNodeAdapter,
    AwaitSubagentsNodeAdapter, CameraAlertNodeAdapter, CameraVerdictNodeAdapter, ChunkNodeAdapter,
    CombineNodeAdapter, CompactContextNodeAdapter, ConditionNodeAdapter,
    ConversationHistoryNodeAdapter, CriticGateNodeAdapter, DelegateCliNodeAdapter,
    DocumentMergeNodeAdapter, DocumentParseNodeAdapter, DocumentRouterNodeAdapter,
    EmbedChunksNodeAdapter, EmbeddingsNodeAdapter, ExcelExtractNodeAdapter, ExecCommandNodeAdapter,
    GraphExtractNodeAdapter, GraphicElementsNodeAdapter, IntervalNodeAdapter, LlmNodeAdapter,
    LoopNodeAdapter, MapNodeAdapter, MemoryNodeAdapter, OcrNodeAdapter, OcrPagesNodeAdapter,
    OnSubagentCompleteNodeAdapter, OutputNodeAdapter, PageDetectNodeAdapter,
    PageDetectPagesNodeAdapter, PatchReviewNodeAdapter, PdfRasterizeNodeAdapter,
    PersistTurnNodeAdapter, PiiFilterNodeAdapter, PlatformSwitchNodeAdapter,
    PptxExtractNodeAdapter, ProjectKnowledgeNodeAdapter, RagAccumulateNodeAdapter,
    RagFinalizeNodeAdapter, RagGraphFactsNodeAdapter, RagGraphSeedNodeAdapter, RagJudgeNodeAdapter,
    RagQuerySeedNodeAdapter, RerankerNodeAdapter, SessionContextNodeAdapter, SpawnNodeAdapter,
    SpeakerContextNodeAdapter, StoreNodeAdapter, SttNodeAdapter, SubagentStatusNodeAdapter,
    SubflowNodeAdapter, TableStructureNodeAdapter, TaskGateNodeAdapter, TextExtractNodeAdapter,
    ToolExecNodeAdapter, TriggerNodeAdapter, TtsCleanNodeAdapter, TtsNodeAdapter,
    VectorNodeAdapter, VisionClassifyNodeAdapter, VisionNodeAdapter, VisionOcrNodeAdapter,
    VisionParseNodeAdapter, VisionParsePagesNodeAdapter, WordExtractNodeAdapter,
    WorkspaceContextNodeAdapter,
};
use crate::flow_engine::progress_broker::RunDescriptor;
use crate::flow_engine::resolver;
use crate::flow_engine::subflow_runner::{SubflowRunner, SubflowRunnerSlot};
use crate::flow_engine::types::{FlowDefinition, FlowNode};
use crate::flow_engine::validation::{self, FlowValidationError};
use crate::services::runtime::quic_handle::ServiceManager;

/// Globalny cap na CAŁY flow to już TYLKO backstop anty-zawieszeniowy (nie
/// limit „budżetu czasu"): per-node 600 s + `max_iterations` ograniczają normalne
/// multi-hop/extraction flow, więc 1 h nigdy nie ucina poprawnego wykonania, a
/// jedynie łapie faktycznie zawieszony / nieskończony flow.
const FLOW_BACKSTOP_SECS: u64 = 3600;

/// Stage 3d-0b-final: typed dispatch error żeby routing layer mógł
/// mapować na precyzyjne HTTP status codes:
/// - `Denied` → 404 model_not_found (plan v1.5: nie ujawniamy istnienia
///   modelu klientom bez ACL).
/// - `CompileFailed` → 500 z msg ("user-defined flow nie kompiluje się").
/// - `Unsupported` → 500 z msg ("brak capability dla service_type").
/// - `Internal` → 500 (runtime err / timeout / inne).
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("flow {flow_id} ACL denied for user")]
    Denied { flow_id: String },
    #[error("flow {flow_id} compile failed: {msg}")]
    CompileFailed { flow_id: String, msg: String },
    #[error("direct dispatch unsupported for service_type='{service_type}', model='{model}'")]
    Unsupported { service_type: String, model: String },
    #[error("flow dispatch internal: {0}")]
    Internal(String),
    /// No running STT service on this node (typed so the edge answers 503).
    #[error("{}", crate::error::CoreError::SttServiceUnavailable)]
    SttServiceUnavailable,
}

impl From<anyhow::Error> for DispatchError {
    fn from(e: anyhow::Error) -> Self {
        match e.downcast_ref::<crate::error::CoreError>() {
            Some(crate::error::CoreError::SttServiceUnavailable) => {
                DispatchError::SttServiceUnavailable
            }
            _ => DispatchError::Internal(e.to_string()),
        }
    }
}

/// Wynik resolve_cached — rozróżnia 3 stany żeby caller wiedział czy wykonać
/// model bezpośrednio (NotFound) czy zwrócić błąd kompilacji (CompileFailed).
enum ResolvedFlow {
    Found(Arc<CachedFlow>),
    /// Resolver nie znalazł jawnego flow dla danego (model, kind, modality).
    /// Caller wykonuje model BEZPOŚREDNIO na executorze (bez flow engine,
    /// bez pii_filter) — jedna capability: llm/vision_llm/tts/stt/embeddings.
    NotFound,
    /// User-defined flow istnieje ale compile failed. Cache'owane jako None
    /// żeby nie próbować ponownie do invalidate. Direct NIE aktywuje się
    /// (admin chciał konkretny flow — niech go naprawi).
    CompileFailed,
}

/// Where a flow request entered the system. An enum, not a `String`, so a typo
/// cannot reach the event log; the snake_case slug from [`FlowOrigin::as_str`]
/// is the only spelling that ever leaves the process.
///
/// Minted by the entry point AFTER authorization and carried as a struct field
/// — never as an `envelope.meta` key. Meta is writable by every node, including
/// a WASM addon block that deserializes a whole envelope from guest memory, so
/// a provenance stamp living there would be forgeable by model output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowOrigin {
    /// Dashboard chat / audio surface over the binary protocol.
    Chat,
    /// Dashboard administration / builder surfaces over the binary protocol
    /// that call a model but are not a conversation: a voice preview in
    /// Settings, Agent Builder assist, an agent playground run. Separate from
    /// `Chat` because folding them together makes the chat volume wrong and
    /// hides admin-initiated model spend behind end-user traffic.
    Dashboard,
    /// Project Studio (project chat, project ingest).
    Project,
    /// Code Studio assist.
    CodeStudio,
    /// External `/v1/*` REST integration.
    Api,
    /// A WASM addon calling core as a model / ingest client.
    Addon,
    /// Camera analysis pipeline (no human in the loop).
    Camera,
    /// Meeting Bot turn. The bot drives the flow, but a person invited it to the
    /// meeting, so the actor is that owner and not the sidecar.
    Meeting,
    /// Admin scheduler tick.
    Scheduler,
    /// Reverse mesh path — a peer node forwarded the request.
    Mesh,
    /// Agent harness run (including sub-agent and reactive continuations).
    Agent,
    /// Internal core work with no external caller (translate, catalog probes,
    /// benchmarks, ML Studio evaluation). Never a default — a call reaches this
    /// variant only by naming it.
    System,
    /// TentaBus reactor (`bus::reactor`) — a flow whose entry is `bus_consume`,
    /// dispatched in the background when new records land on its subscribed
    /// topic/group. The message carries no human/service identity, so the run
    /// acts under `FlowActor::system()`; this origin is what distinguishes it
    /// in the event log from `System`'s other internal-work callers.
    Bus,
}

impl FlowOrigin {
    /// Stable wire spelling — persisted in the event log and rendered by the UI.
    pub fn as_str(self) -> &'static str {
        match self {
            FlowOrigin::Chat => "chat",
            FlowOrigin::Dashboard => "dashboard",
            FlowOrigin::Project => "project",
            FlowOrigin::CodeStudio => "code_studio",
            FlowOrigin::Api => "api",
            FlowOrigin::Addon => "addon",
            FlowOrigin::Camera => "camera",
            FlowOrigin::Meeting => "meeting",
            FlowOrigin::Scheduler => "scheduler",
            FlowOrigin::Mesh => "mesh",
            FlowOrigin::Agent => "agent",
            FlowOrigin::System => "system",
            FlowOrigin::Bus => "bus",
        }
    }

    /// Exact inverse of [`FlowOrigin::as_str`]. `None` for anything else — a
    /// stored value we cannot read must REFUSE, not fall back to `System`: a
    /// silent default would report a Code Studio run as internal core work and
    /// hide exactly the provenance this enum exists to keep.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "chat" => FlowOrigin::Chat,
            "dashboard" => FlowOrigin::Dashboard,
            "project" => FlowOrigin::Project,
            "code_studio" => FlowOrigin::CodeStudio,
            "api" => FlowOrigin::Api,
            "addon" => FlowOrigin::Addon,
            "camera" => FlowOrigin::Camera,
            "meeting" => FlowOrigin::Meeting,
            "scheduler" => FlowOrigin::Scheduler,
            "mesh" => FlowOrigin::Mesh,
            "agent" => FlowOrigin::Agent,
            "system" => FlowOrigin::System,
            "bus" => FlowOrigin::Bus,
            _ => return None,
        })
    }
}

/// Kind of authenticated caller behind a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    User,
    ApiKey,
    Addon,
    System,
}

impl ActorKind {
    /// Stable wire spelling — persisted in the event log and rendered by the UI.
    pub fn as_str(self) -> &'static str {
        match self {
            ActorKind::User => "user",
            ActorKind::ApiKey => "api_key",
            ActorKind::Addon => "addon",
            ActorKind::System => "system",
        }
    }

    /// Exact inverse of [`ActorKind::as_str`]. `None` for anything else — see
    /// [`FlowOrigin::parse`]: reading an unknown actor kind as `System` would
    /// turn an API key into unattended core work.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "user" => ActorKind::User,
            "api_key" => ActorKind::ApiKey,
            "addon" => ActorKind::Addon,
            "system" => ActorKind::System,
            _ => return None,
        })
    }
}

/// The authenticated caller. Built ONLY by an entry point, after authorization
/// — the constructors are the whole public surface, so no code path can invent
/// an actor from request content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowActor {
    kind: ActorKind,
    /// user_id / API key uid / addon instance id / system component id.
    id: Option<String>,
    /// The user an API key resolves to; `None` marks a service key.
    user_id: Option<String>,
}

impl FlowActor {
    pub fn user(user_id: impl Into<String>) -> Self {
        let user_id = user_id.into();
        Self {
            kind: ActorKind::User,
            id: Some(user_id.clone()),
            user_id: Some(user_id),
        }
    }

    /// `bound_user` is the user an API key resolves to; `None` marks a service
    /// key, which the UI shows explicitly rather than as an empty field. The
    /// binding is resolved server-side while verifying the key — the call
    /// itself does not carry it.
    pub fn api_key(key_uid: impl Into<String>, bound_user: Option<String>) -> Self {
        Self {
            kind: ActorKind::ApiKey,
            id: Some(key_uid.into()),
            user_id: bound_user,
        }
    }

    pub fn addon(addon_id: impl Into<String>) -> Self {
        Self {
            kind: ActorKind::Addon,
            id: Some(addon_id.into()),
            user_id: None,
        }
    }

    pub fn system() -> Self {
        Self {
            kind: ActorKind::System,
            id: None,
            user_id: None,
        }
    }

    /// System actor naming the core component that acted (a camera id, a
    /// scheduler job id). Keeps `ActorKind::System` — there is no user behind
    /// it — while still answering "which part of the system".
    pub fn system_component(id: impl Into<String>) -> Self {
        Self {
            kind: ActorKind::System,
            id: Some(id.into()),
            user_id: None,
        }
    }

    /// Rebuilds an actor from the flat fields it was spread into (a
    /// `FlowRequestMeta` / `ExecutionContext` carries the three parts, not the
    /// struct). `pub(crate)` — outside the crate the constructors above stay
    /// the only way to mint one.
    pub(crate) fn from_parts(kind: ActorKind, id: Option<String>, user_id: Option<String>) -> Self {
        Self { kind, id, user_id }
    }

    pub fn kind(&self) -> ActorKind {
        self.kind
    }

    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// The user behind this actor, when there is one: the user themselves, or
    /// the user an API key is bound to.
    pub fn user_id(&self) -> Option<&str> {
        self.user_id.as_deref()
    }
}

/// §2.5 — origin + actor of one call, carried as ONE value so no layer can pair
/// the wrong actor with the wrong origin.
///
/// Threaded through the capability-dispatcher DTOs (`LlmRequest`,
/// `EmbeddingsRequest`, …) for exactly the reason `flow_depth` is: the runtime
/// `ExecutionContext` a capability dispatcher builds RE-ENTERS the executor, and
/// an alias that resolves onto a flow surface starts THAT flow with this stamp.
/// Without it every capability hop would reset the origin to `system` and the
/// event log would lose the entry point one node below the top.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallProvenance {
    pub origin: FlowOrigin,
    pub actor: FlowActor,
}

impl CallProvenance {
    pub fn new(origin: FlowOrigin, actor: FlowActor) -> Self {
        Self { origin, actor }
    }

    /// Internal core work with no external caller — a statement, not an
    /// omission. Deliberately NOT a `Default` impl: a defaulted provenance is
    /// the silent `system` stamp the whole design exists to prevent, and it
    /// would compile at a call site that simply forgot to say where it came
    /// from.
    pub fn system() -> Self {
        Self {
            origin: FlowOrigin::System,
            actor: FlowActor::system(),
        }
    }
}

/// Per-request metadata przekazywane przez callera. FlowDispatcher buduje z
/// tego `ExecutionContext` (klonując Arc'i dispatcherów + clock + blobs).
#[derive(Clone)]
pub struct FlowRequestMeta {
    pub request_id: String,
    pub session_id: Option<String>,
    pub user_id: Option<String>,
    pub user_role: Option<String>,
    /// RAG E1.0 — tożsamość addona-callera (== instance_id) przeprowadzona z
    /// `CallerContext.addon_id` przez executor. `Some` tylko dla flow wyzwolonego
    /// przez addon JAKO MODEL; `None` dla /v1 user / kamera / agent. `make_context`
    /// kopiuje to do `ExecutionContext.addon_id`.
    pub addon_id: Option<String>,
    /// RAG E1.0 — organizacja-właściciel (`CallerContext.org_id`). `None` =>
    /// domyślny tenant rozwiązywany przy użyciu. Kopiowane do `ExecutionContext.org_id`.
    pub org_id: Option<String>,
    /// Katalog dla NOWEJ przestrzeni wektorowej tego wywolania — patrz
    /// `ExecutionContext::vector_home`. Osobne pole, nie klucz w `options`/meta:
    /// caller-addon nie moze sterowac miejscem zapisu indeksu.
    pub vector_home: Option<std::path::PathBuf>,
    /// Directory for a NEW graph collection of this call — see
    /// `ExecutionContext::graph_home`. A separate field for the same reason as
    /// `vector_home`: a caller addon must not steer where a graph file is
    /// created on disk.
    pub graph_home: Option<std::path::PathBuf>,
    /// §2.5 — where this request entered the system. Minted by the entry point
    /// after authorization. A separate field, not a `meta` key, for the same
    /// reason as `vector_home`: `envelope.meta` is writable by every node
    /// (a WASM addon block included), so identity kept there would be
    /// derivable from model output.
    pub origin: FlowOrigin,
    /// §2.5 — kind of the authenticated caller. Set together with `actor_id` /
    /// `actor_user_id` from a [`FlowActor`] built by the entry point.
    pub actor_kind: ActorKind,
    /// user_id / API key uid / addon instance id / system component id.
    pub actor_id: Option<String>,
    /// The user behind an API key; `None` means a service key with no binding
    /// (the UI must show that explicitly, not as an empty field).
    pub actor_user_id: Option<String>,
    /// Ties this run to the audit trail and to `compliance_ai_events`.
    pub correlation_id: Option<String>,
    pub deadline: Option<Instant>,
    pub cancel_token: CancellationToken,
    /// §3.11 C — per-request progress fan-out. The caller (a handler holding
    /// AppState's `ProgressBroker`) injects the production sink; `None` →
    /// no-op (headless / tests). Scope = session_id, falling back to
    /// request_id so a broadcast key always exists.
    pub progress_sink: Option<Arc<dyn ProgressSink>>,
    /// RAG C2 (recursion guard) — głębokość zagnieżdżenia flow odziedziczona
    /// po runtime'owym `ExecutionContext.flow_stack`. Gdy capability dispatcher
    /// (reranker/embeddings) rozwiąże alias na flow-surface, ten flow re-wchodzi
    /// w silnik przez `dispatch_by_flow_id`; bez przekazania głębokości nowy
    /// `make_context` resetowałby `subflow_depth` do 0 i guard rekurencji nigdy
    /// by nie narósł (nieograniczona rekurencja). `make_context` seeduje z tego
    /// pola `subflow_depth`.
    pub flow_depth: u8,
}

impl FlowRequestMeta {
    /// `origin` and `actor` are mandatory positional arguments on purpose:
    /// there is no defaulted variant, so an entry point that forgets to say
    /// where a request came from fails to COMPILE instead of silently logging
    /// `system`. `correlation_id` starts empty — only callers that already hold
    /// an audit / compliance request id set it.
    pub fn new(request_id: impl Into<String>, origin: FlowOrigin, actor: FlowActor) -> Self {
        Self {
            request_id: request_id.into(),
            session_id: None,
            user_id: None,
            user_role: None,
            addon_id: None,
            vector_home: None,
            graph_home: None,
            org_id: None,
            origin,
            actor_kind: actor.kind,
            actor_id: actor.id,
            actor_user_id: actor.user_id,
            correlation_id: None,
            deadline: None,
            cancel_token: CancellationToken::new(),
            progress_sink: None,
            flow_depth: 0,
        }
    }

    /// Effective broadcast scope for progress events — session id when present,
    /// else the request id.
    pub(crate) fn progress_scope(&self) -> String {
        self.session_id
            .clone()
            .unwrap_or_else(|| self.request_id.clone())
    }
}

/// Process-global handle to the constructed FlowDispatcher, so non-flow callers
/// (the camera cold path) can run a camera's assigned analysis flow. Set once by
/// the router after construction; `None` until then (cold path falls back to the
/// hardcoded enrichment).
static GLOBAL_FLOW_DISPATCHER: std::sync::OnceLock<Arc<FlowDispatcher>> =
    std::sync::OnceLock::new();

pub fn set_global_flow_dispatcher(d: Arc<FlowDispatcher>) {
    let _ = GLOBAL_FLOW_DISPATCHER.set(d);
}
pub fn global_flow_dispatcher() -> Option<Arc<FlowDispatcher>> {
    GLOBAL_FLOW_DISPATCHER.get().cloned()
}

pub struct FlowDispatcher {
    db: DbPool,
    cache: FlowCache,
    registry: Arc<AdapterRegistry>,
    ctx_factory: Arc<ContextFactory>,
    /// `AddonFlowRegistry` udostępniany handlerom GUI (Flow Builder lista
    /// templates dorzuca tu addon blocks). Ustawiany przez `set_addon_resolver`
    /// razem z resolverem dla AdapterRegistry — single touchpoint w main.rs.
    addon_flow_blocks:
        parking_lot::RwLock<Option<Arc<crate::addon::flow_blocks::AddonFlowRegistry>>>,
    /// Harness §3.5.0: AgentServiceSlot — wypełniany z main.rs przez
    /// `set_agent_service` po zbudowaniu AddonManager/AppState. Phase-3 bloki
    /// (`agent_context`, `tool_exec`) trzymają KLON tego slotu (wpięty w
    /// `build_registry`), więc `set_agent_service` zapełnia go dla adapterów i
    /// dispatchera naraz. Pusty slot = błąd node'a (sloty wypełniane na starcie,
    /// przed obsługą ruchu).
    agent_service: crate::agents::AgentServiceSlot,
    /// Harness §3.5.0: SubflowRunnerSlot — w przeciwieństwie do `agent_service`
    /// wypełniany JUŻ w `FlowDispatcher::new` (dispatcher ma DbPool i registry).
    /// `subflow`/`loop`/`map`/`agent` trzymają klon tego slotu (wpięty w
    /// `build_registry`); runner trzyma `Weak<AdapterRegistry>`, więc cykl
    /// registry→adapter→slot→runner→registry jest przerwany.
    subflow_runner: SubflowRunnerSlot,
    /// Ephemeral in-memory blob layer overlaid in front of the durable store for
    /// `ctx.blobs`. The camera cold path puts a raw frame here, dispatches the
    /// analysis flow (whose nodes read it via the composite), then deletes it —
    /// so per-frame frames never hit disk and a frame delete can never touch a
    /// durable blob. See [`crate::flow_engine::blob_store::CompositeBlobStore`].
    frame_blobs: Arc<dyn BlobStore>,
}

/// Pre-zbudowane Arc'i wszystkich capability dispatcherów + clock + blobs.
/// `make_context` klonuje je do nowego `ExecutionContext` per request.
struct ContextFactory {
    clock: Arc<dyn Clock>,
    blobs: Arc<dyn BlobStore>,
    vectors: Arc<crate::services::vector::NamespaceManager>,
    #[cfg(feature = "graph")]
    graph: Arc<crate::services::graph::GraphManager>,
    llm: Arc<dyn LlmDispatcher>,
    embeddings: Arc<dyn EmbeddingsDispatcher>,
    reranker: Arc<dyn RerankDispatcher>,
    documents: Arc<dyn DocumentsDispatcher>,
    stt: Arc<dyn SttDispatcher>,
    tts: Arc<dyn TtsDispatcher>,
    vision: Arc<dyn VisionDispatcher>,
    prompts: Arc<dyn PromptStore>,
    memory: Arc<dyn MemoryStore>,
    history: Arc<dyn ConversationHistoryStore>,
    audit: Arc<dyn AuditSink>,
    metrics: Arc<dyn MetricsSink>,
    pii_rules: Arc<dyn PiiRulesStore>,
    tts_cleaning: Arc<dyn TtsCleaningStore>,
}

impl ContextFactory {
    fn make_context(&self, meta: &FlowRequestMeta, descriptor: RunDescriptor) -> ExecutionContext {
        // §2.5 — the one place every dispatch entry passes through holding the
        // authorized meta. The event log copies the provenance bound here; a
        // `ProgressEvent` carries none, and nothing a node writes can reach it.
        // `descriptor` travels the same way and for the same reason: the
        // resolved model and the compiled flow exist only in the caller's
        // frame, and the log's opening row is an audit record that may not
        // reconstruct them from anything a node could have written.
        crate::flow_engine::progress_broker::begin_run(meta, descriptor);
        ExecutionContext {
            request_id: meta.request_id.clone(),
            execution_id: 0,
            parent_execution_id: None,
            light: false,
            session_id: meta.session_id.clone(),
            user_id: meta.user_id.clone(),
            user_role: meta.user_role.clone(),
            addon_id: meta.addon_id.clone(),
            org_id: meta.org_id.clone(),
            vector_home: meta.vector_home.clone(),
            graph_home: meta.graph_home.clone(),
            origin: meta.origin,
            actor_kind: meta.actor_kind,
            actor_id: meta.actor_id.clone(),
            actor_user_id: meta.actor_user_id.clone(),
            correlation_id: meta.correlation_id.clone(),
            deadline: meta.deadline,
            deadline_extension_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            cancel_token: meta.cancel_token.clone(),
            subflow_depth: meta.flow_depth,
            subflow_visited: Arc::new(Vec::new()),
            initial_envelope: Arc::new(FlowEnvelope::empty()),
            clock: self.clock.clone(),
            blobs: self.blobs.clone(),
            vectors: self.vectors.clone(),
            #[cfg(feature = "graph")]
            graph: self.graph.clone(),
            llm: self.llm.clone(),
            embeddings: self.embeddings.clone(),
            reranker: self.reranker.clone(),
            documents: self.documents.clone(),
            stt: self.stt.clone(),
            tts: self.tts.clone(),
            vision: self.vision.clone(),
            prompts: self.prompts.clone(),
            memory: self.memory.clone(),
            history: self.history.clone(),
            audit: self.audit.clone(),
            metrics: self.metrics.clone(),
            pii_rules: self.pii_rules.clone(),
            tts_cleaning: self.tts_cleaning.clone(),
            progress: meta
                .progress_sink
                .clone()
                .unwrap_or_else(|| Arc::new(NoopProgress) as Arc<dyn ProgressSink>),
            progress_scope: meta.progress_scope(),
            usage_sink: Arc::new(UsageSink::new()),
        }
    }
}

impl FlowDispatcher {
    pub fn new(
        db: DbPool,
        service_manager: Arc<ServiceManager>,
        runtime_slot: ModelRuntimeSlot,
        blobs: Arc<dyn BlobStore>,
    ) -> Self {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let metrics: Arc<dyn MetricsSink> = Arc::new(NoopMetrics);

        let prompts: Arc<dyn PromptStore> =
            Arc::new(PromptsImpl::new(service_manager.prompt_registry.clone()));
        let audit: Arc<dyn AuditSink> = Arc::new(AuditSinkImpl::new(db.clone()));
        let pii_rules: Arc<dyn PiiRulesStore> = Arc::new(PiiRulesStoreImpl::new(db.clone()));
        let tts_cleaning: Arc<dyn TtsCleaningStore> =
            Arc::new(TtsCleaningStoreImpl::new(db.clone()));
        let history: Arc<dyn ConversationHistoryStore> =
            Arc::new(ConversationHistoryImpl::new(db.clone()));
        let quic_finder = Arc::new(ServiceManagerQuicFinder::new(service_manager.clone()));
        let memory: Arc<dyn MemoryStore> = Arc::new(MemoryStoreImpl::new(quic_finder));

        // Harness §3.4: the LLM dispatcher is gateway-aware — it gets the
        // DbPool so it can open one compliance_ai_events row per `execute_chat`,
        // auditing every `llm` node in every flow per call. The node id matches
        // the routing layer's gateway (local hostname; the mesh registry isn't
        // populated yet at FlowDispatcher::new).
        // Ephemeral frame layer for the camera cold path: nodes read frames via
        // the composite `ctx.blobs`, but durable node-produced blobs still write
        // to (and GC from) the original persistent store. Built BEFORE the
        // capability dispatchers so the LLM/TTS/STT dispatchers (which resolve
        // image/audio BlobRefs straight from the request, e.g. `vision_llm`
        // feeding a camera frame to a multimodal model) share the same composite
        // and can therefore see the ephemeral frame, not just durable blobs.
        let frame_blobs: Arc<dyn BlobStore> =
            Arc::new(crate::flow_engine::blob_store::EphemeralBlobStore::new());
        let ctx_blobs: Arc<dyn BlobStore> =
            Arc::new(crate::flow_engine::blob_store::CompositeBlobStore::new(
                frame_blobs.clone(),
                blobs.clone(),
            ));

        let audit_node_id = crate::mesh::node_info_collector::local_hostname();
        let llm: Arc<dyn LlmDispatcher> = Arc::new(LlmDispatcherImpl::new(
            runtime_slot.clone(),
            ctx_blobs.clone(),
            Some(db.clone()),
            audit_node_id,
        ));
        let embeddings: Arc<dyn EmbeddingsDispatcher> =
            Arc::new(EmbeddingsDispatcherImpl::new(runtime_slot.clone()));
        let reranker: Arc<dyn RerankDispatcher> =
            Arc::new(RerankDispatcherImpl::new(runtime_slot.clone()));
        let documents: Arc<dyn DocumentsDispatcher> =
            Arc::new(DocumentsDispatcherImpl::new(runtime_slot.clone()));
        let tts: Arc<dyn TtsDispatcher> = Arc::new(TtsDispatcherImpl::new(
            runtime_slot.clone(),
            ctx_blobs.clone(),
        ));
        let stt: Arc<dyn SttDispatcher> = Arc::new(SttDispatcherImpl::new(
            runtime_slot.clone(),
            ctx_blobs.clone(),
        ));
        let vision: Arc<dyn VisionDispatcher> = Arc::new(VisionDispatcherImpl::new(runtime_slot));

        // RAG E1.0 — współdzielony proces-szeroki rejestr przestrzeni wektorowych.
        // Te same backendy co host functions addona (jeden katalog w procesie),
        // więc flow-node i ingest host-fn widzą tę samą przestrzeń instancji.
        let vectors = crate::services::vector_namespace_manager(&db).clone();

        // RAG E1.1 — współdzielony proces-szeroki rejestr kolekcji grafowych.
        // Te same backendy co host functions addona (jeden katalog w procesie),
        // więc graph_search node i ingest host-fn widzą tę samą kolekcję instancji.
        #[cfg(feature = "graph")]
        let graph = crate::services::graph_manager(&db).clone();

        let ctx_factory = Arc::new(ContextFactory {
            clock,
            blobs: ctx_blobs,
            vectors,
            #[cfg(feature = "graph")]
            graph,
            llm,
            embeddings,
            reranker,
            documents,
            stt,
            tts,
            vision,
            prompts,
            memory,
            history,
            audit,
            metrics,
            pii_rules,
            tts_cleaning,
        });

        let agent_service: crate::agents::AgentServiceSlot =
            Arc::new(parking_lot::RwLock::new(None));
        // SubflowRunnerSlot is filled here (not from main.rs): the runner needs
        // only the DbPool and the registry, both available now. The adapters
        // registered below carry a clone of this still-empty slot; we fill it
        // once the registry Arc exists so the runner's Weak resolves.
        let subflow_runner: SubflowRunnerSlot = Arc::new(parking_lot::RwLock::new(None));
        let registry = Arc::new(build_registry(
            agent_service.clone(),
            subflow_runner.clone(),
        ));
        *subflow_runner.write() = Some(Arc::new(SubflowRunner::new(
            db.clone(),
            Arc::downgrade(&registry),
        )));
        Self {
            db,
            cache: FlowCache::new(60),
            registry,
            ctx_factory,
            addon_flow_blocks: parking_lot::RwLock::new(None),
            agent_service,
            subflow_runner,
            frame_blobs,
        }
    }

    /// Ephemeral frame store for the camera cold path (put/delete a transient RGB
    /// frame around an analysis-flow dispatch). Distinct from [`Self::blobs`],
    /// which is the durable store.
    pub fn frame_blobs(&self) -> Arc<dyn BlobStore> {
        self.frame_blobs.clone()
    }

    pub fn registry(&self) -> &Arc<AdapterRegistry> {
        &self.registry
    }

    /// Save-time validation entry point (plan-app-platform §3.3), called
    /// from `dispatch::handlers::validate_flow_json_str` — the ONLY place a
    /// flow's `bus_*` `instance_id`s get checked against what is actually
    /// installed; every save-shaped handler (flow create, flow update, and
    /// the two other call sites `validate_flow_json_str` covers) routes
    /// through that one function, so this method has exactly one caller by
    /// design rather than four call sites duplicating the DB lookup.
    ///
    /// Runs the generic port/graph rules (`validation::validate`) PLUS every
    /// `bus_consume`/`bus_publish`/`bus_transform` node's `instance_id` must
    /// name an INSTALLED TentaBus instance (`validation::
    /// validate_bus_instances`). `validation::validate` alone (no DB access)
    /// is still what `CompiledFlow::compile` runs on every load — this
    /// wrapper is for the save path specifically, which has a DB handle and
    /// runs far less often than a flow load/execution.
    ///
    /// Two guarantees kept deliberately separate:
    /// - a flow with NO `bus_*` node (`validation::
    ///   flow_references_a_bus_instance` is `false`) never even runs the
    ///   `list_package_instances` query — an unrelated flow's save can never
    ///   be rejected by a TentaBus DB hiccup;
    /// - when the flow DOES reference an instance and the query itself
    ///   fails, this fails CLOSED (`FlowValidationError::
    ///   BusInstanceLookupFailed`, a message distinct from "not installed"
    ///   so the caller does not go hunting for the wrong problem) — the same
    ///   fail-closed posture `dispatch::app_gate` uses for its own DB
    ///   lookups.
    pub fn validate_flow(&self, def: &FlowDefinition) -> Result<(), FlowValidationError> {
        validation::validate(def, &self.registry)?;
        if !validation::flow_references_a_bus_instance(def) {
            return Ok(());
        }
        let installed: std::collections::HashSet<String> = match repository::list_package_instances(
            &self.db,
            crate::bus::instance::BusInstanceId::PACKAGE_ID,
        ) {
            Ok(rows) => rows.into_iter().map(|(addon_id, _, _)| addon_id).collect(),
            Err(e) => {
                tracing::warn!(
                    "flow save validation: TentaBus instance list failed, failing closed: {e}"
                );
                return Err(FlowValidationError::BusInstanceLookupFailed);
            }
        };
        validation::validate_bus_instances(def, &installed)
    }

    /// Wpina addon manager jako resolver custom flow blocks. Wołane raz
    /// z main.rs po `AddonManager::new` — od tego momentu compile flow
    /// pasuje node_type'y w formacie "addon.{id}.{block}" do
    /// `AddonNodeAdapter` zbudowanego z `AddonFlowRegistry::find_block`.
    /// Builtin adaptery (`llm`, `tts`, ...) wygrywają nad addon resolverem,
    /// więc addon nie może nadpisać core node_type.
    pub fn set_addon_resolver(&self, manager: Arc<crate::addon::AddonManager>) {
        use crate::flow_engine::node_adapters::AddonNodeAdapter;
        let blocks_registry = manager.flow_blocks_registry().clone();
        // Zachowaj referencję do registry zeby handlery GUI mogly listowac
        // addon blocks (Flow Builder palette).
        *self.addon_flow_blocks.write() = Some(blocks_registry.clone());
        let resolver: crate::flow_engine::node_adapter::DynamicAdapterResolver =
            Arc::new(move |node_type: &str| -> Option<Arc<dyn NodeAdapter>> {
                // Tylko prefiks "addon." idzie do registry — szybki bail
                // dla wszystkich innych node_type'ów (oszczędność jednego
                // RwLock read na każde compile flow).
                if !node_type.starts_with("addon.") {
                    return None;
                }
                let block = blocks_registry.find_block(node_type)?;
                let adapter = AddonNodeAdapter::from_block(&block, manager.clone());
                Some(Arc::new(adapter) as Arc<dyn NodeAdapter>)
            });
        self.registry.set_dynamic_resolver(resolver);
    }

    /// Zwraca `AddonFlowRegistry` jesli `set_addon_resolver` zostalo wolane.
    /// Handler `FlowNodeTemplatesListRequest` uzywa do dorzucenia addon
    /// blocks do palety Flow Buildera.
    pub fn addon_flow_blocks(&self) -> Option<Arc<crate::addon::flow_blocks::AddonFlowRegistry>> {
        self.addon_flow_blocks.read().clone()
    }

    /// Harness §3.5.0: wpina `AgentService` do slotu. Wołane raz z main.rs po
    /// zbudowaniu `AddonManager`/`AppState` (analogicznie do
    /// `set_addon_resolver`). Phase-3 bloki czytają slot przez `agent_service`.
    pub fn set_agent_service(&self, service: Arc<crate::agents::AgentService>) {
        *self.agent_service.write() = Some(service);
    }

    /// Zwraca `AgentService` jeśli slot został wypełniony. Bloki `agent_context`
    /// / `tool_exec` czytają ten sam slot przez swój klon (wpięty w
    /// `build_registry`); ten accessor służy callerom dispatchera.
    pub fn agent_service(&self) -> Option<Arc<crate::agents::AgentService>> {
        self.agent_service.read().clone()
    }

    /// Zwraca `SubflowRunner` (wypełniony w `FlowDispatcher::new`). Bloki
    /// `subflow`/`loop`/`map`/`agent` czytają ten sam slot przez swój klon
    /// (wpięty w `build_registry`); accessor służy callerom dispatchera, którzy
    /// chcą uruchomić flow jako pod-flow poza grafem.
    pub fn subflow_runner(&self) -> Option<Arc<SubflowRunner>> {
        self.subflow_runner.read().clone()
    }

    /// Etap 2: BlobStore handle — używane przez TTS-as-flow path w
    /// services/runtime/executor.rs do pobrania bytes audio po BlobRef
    /// po zakończeniu flow.
    pub fn blobs(&self) -> Arc<dyn BlobStore> {
        self.ctx_factory.blobs.clone()
    }

    /// Etap 3c: TtsDispatcher handle — używane przez
    /// `/v1/audio/speech/stream` endpoint do uruchomienia
    /// `stream_synthesize` poza flow path.
    pub fn tts(&self) -> Arc<dyn TtsDispatcher> {
        self.ctx_factory.tts.clone()
    }

    pub fn invalidate_cache(&self) {
        self.cache.invalidate_all();
    }

    pub async fn try_dispatch(
        &self,
        model_name: &str,
        service_type: &str,
        initial: FlowEnvelope,
        meta: FlowRequestMeta,
    ) -> std::result::Result<FlowExecutionOutcome, DispatchError> {
        let modality = derive_modality(&initial);
        self.try_dispatch_with_modality(model_name, service_type, modality, initial, meta)
            .await
    }

    /// Wariant `try_dispatch` z JAWNĄ modality zamiast wyprowadzanej z payloadu.
    /// RAG ingest seeduje binarny payload, który może być `Image` ALBO `Other`
    /// (PDF/xlsx/docx) — `derive_modality` rozszczepiłoby to na dwa różne flow
    /// (`:image` vs `:text`). Ingest chce JEDNEGO flow `<model>:ingest:document`
    /// niezależnie od typu pliku, więc resolwuje po stałej modality `"document"`.
    pub async fn try_dispatch_with_modality(
        &self,
        model_name: &str,
        service_type: &str,
        modality: &'static str,
        initial: FlowEnvelope,
        meta: FlowRequestMeta,
    ) -> std::result::Result<FlowExecutionOutcome, DispatchError> {
        let cache_key = format!("{}:{}:{}", model_name, service_type, modality);
        match self
            .resolve_cached(&cache_key, model_name, service_type, modality)
            .await
            .map_err(DispatchError::from)?
        {
            ResolvedFlow::Found(cached) => {
                if !self.acl_allow(&cached.flow.id, &meta) {
                    return Err(DispatchError::Denied {
                        flow_id: cached.flow.id.clone(),
                    });
                }
                self.run_blocking(
                    cached.compiled.clone(),
                    initial,
                    meta,
                    RunDescriptor::resolved(model_name, service_type, modality),
                )
                .await
                .map_err(DispatchError::from)
            }
            ResolvedFlow::NotFound => {
                // Brak jawnego flow — model wykonywany BEZPOŚREDNIO na
                // executorze (jedna capability, bez flow engine, bez pii_filter).
                self.run_direct_blocking(service_type, model_name, modality, initial, meta)
                    .await
            }
            ResolvedFlow::CompileFailed => Err(DispatchError::CompileFailed {
                flow_id: String::new(),
                msg: format!("user-defined flow for '{model_name}/{service_type}'"),
            }),
        }
    }

    pub async fn dispatch_by_flow_id(
        &self,
        flow_id: String,
        initial: FlowEnvelope,
        meta: FlowRequestMeta,
    ) -> std::result::Result<FlowExecutionOutcome, DispatchError> {
        // Compiled-flow cache keyed by id (distinct `byid:` namespace from the
        // model-resolution path). The camera cold path dispatches the same flow
        // on every detection event; without this it would re-fetch + re-compile
        // the flow JSON per event. Flow create/update/delete/version-restore
        // handlers call `invalidate_cache()`, so a cached compile never serves a
        // stale edit; the 60 s TTL bounds any other drift.
        let cache_key = format!("byid:{flow_id}");
        let cached = match self.cache.get(&cache_key) {
            Some(slot) => slot,
            None => {
                let pool = self.db.clone();
                let lookup_id = flow_id.clone();
                let flow_opt =
                    tokio::task::spawn_blocking(move || repository::get_flow(&pool, &lookup_id))
                        .await
                        .map_err(|e| DispatchError::Internal(e.to_string()))?
                        .map_err(|e| DispatchError::Internal(e.to_string()))?;
                let flow = flow_opt.ok_or_else(|| DispatchError::CompileFailed {
                    flow_id: flow_id.clone(),
                    msg: "flow id nie istnieje w DB".to_string(),
                })?;
                match CompiledFlow::from_json(&flow.id, &flow.flow_json, &self.registry) {
                    Ok(c) => {
                        let cached = Arc::new(CachedFlow {
                            flow,
                            compiled: Arc::new(c),
                        });
                        self.cache.set(&cache_key, Some(cached.clone()));
                        Some(cached)
                    }
                    Err(e) => {
                        warn!(flow_id, "compile failed: {e}");
                        // Negative cache: a structurally broken flow JSON stays
                        // broken until an admin edits it (which invalidates).
                        self.cache.set(&cache_key, None);
                        None
                    }
                }
            }
        };
        let cached = cached.ok_or_else(|| DispatchError::CompileFailed {
            flow_id: flow_id.clone(),
            msg: "flow compile failed".to_string(),
        })?;
        if cached.flow.status != "active" {
            warn!(flow_id, status = %cached.flow.status, "flow nieaktywny — pomijam");
            return Err(DispatchError::CompileFailed {
                flow_id,
                msg: format!("flow status='{}' (nie active)", cached.flow.status),
            });
        }
        if !self.acl_allow(&flow_id, &meta) {
            return Err(DispatchError::Denied { flow_id });
        }
        // The caller named a flow outright, so no model, service type or
        // modality was resolved for this run; `run_blocking` fills in the flow
        // that executes and the rest stays empty rather than guessed.
        self.run_blocking(
            cached.compiled.clone(),
            initial,
            meta,
            RunDescriptor::default(),
        )
        .await
        .map_err(DispatchError::from)
    }

    /// Background-run variant (Harness §3.6/§3.7): runs the agent harness flow
    /// for an `AgentRunManager` task. Unlike `dispatch_by_flow_id` it does NOT
    /// impose the `FLOW_BACKSTOP_SECS` cap — a long agent run is governed
    /// solely by `meta.deadline` (the agent's own budget) and `meta.cancel_token`,
    /// both already enforced between nodes by the executor. ACL is skipped: the
    /// manager already verified the spawning principal and the harness flows are
    /// resolver-unreachable system flows (no per-user ACL rows).
    pub async fn dispatch_by_flow_id_background(
        &self,
        flow_id: String,
        initial: FlowEnvelope,
        meta: FlowRequestMeta,
    ) -> std::result::Result<FlowExecutionOutcome, DispatchError> {
        let pool = self.db.clone();
        let lookup_id = flow_id.clone();
        let flow_opt = tokio::task::spawn_blocking(move || repository::get_flow(&pool, &lookup_id))
            .await
            .map_err(|e| DispatchError::Internal(e.to_string()))?
            .map_err(|e| DispatchError::Internal(e.to_string()))?;
        let flow = flow_opt.ok_or_else(|| DispatchError::CompileFailed {
            flow_id: flow_id.clone(),
            msg: "flow id does not exist in DB".to_string(),
        })?;
        if flow.status != "active" {
            return Err(DispatchError::CompileFailed {
                flow_id,
                msg: format!("flow status='{}' (not active)", flow.status),
            });
        }
        let compiled = match CompiledFlow::from_json(&flow.id, &flow.flow_json, &self.registry) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                return Err(DispatchError::CompileFailed {
                    flow_id,
                    msg: e.to_string(),
                });
            }
        };
        // A harness flow is named outright by the run manager, so the only
        // descriptor fact this run has is the flow that executes.
        let ctx = self
            .ctx_factory
            .make_context(&meta, RunDescriptor::default().with_flow(&compiled.flow_id));
        execute_blocking(
            self.db.clone(),
            compiled,
            initial,
            ctx,
            self.registry.clone(),
        )
        .await
        .map_err(DispatchError::from)
    }

    /// Streaming wariant `dispatch_by_flow_id` — odpala KONKRETNY flow po ID
    /// (np. wybrany przez usera w trybie audio chatu), bez rozwiązywania przez
    /// model/service_type. Blocking-only flow opakowany w single-chunk stream.
    pub async fn dispatch_by_flow_id_streaming(
        &self,
        flow_id: String,
        initial: FlowEnvelope,
        meta: FlowRequestMeta,
    ) -> std::result::Result<StreamingExecution, DispatchError> {
        let pool = self.db.clone();
        let lookup_id = flow_id.clone();
        let flow_opt = tokio::task::spawn_blocking(move || repository::get_flow(&pool, &lookup_id))
            .await
            .map_err(|e| DispatchError::Internal(e.to_string()))?
            .map_err(|e| DispatchError::Internal(e.to_string()))?;
        let flow = flow_opt.ok_or_else(|| DispatchError::CompileFailed {
            flow_id: flow_id.clone(),
            msg: "flow id nie istnieje w DB".to_string(),
        })?;
        if flow.status != "active" {
            return Err(DispatchError::CompileFailed {
                flow_id,
                msg: format!("flow status='{}' (nie active)", flow.status),
            });
        }
        if !self.acl_allow(&flow_id, &meta) {
            return Err(DispatchError::Denied { flow_id });
        }
        let compiled = match CompiledFlow::from_json(&flow.id, &flow.flow_json, &self.registry) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                return Err(DispatchError::CompileFailed {
                    flow_id,
                    msg: e.to_string(),
                });
            }
        };
        if !compiled.is_streaming {
            let outcome = self
                .run_blocking(compiled, initial, meta, RunDescriptor::default())
                .await
                .map_err(DispatchError::from)?;
            return Ok(wrap_blocking_as_stream(outcome, self.blobs()));
        }
        // Dispatched by flow id: no model routing key was resolved, so the flow
        // is the whole descriptor.
        let ctx = self
            .ctx_factory
            .make_context(&meta, RunDescriptor::default().with_flow(&compiled.flow_id));
        let stream_exec = execute_streaming(
            self.db.clone(),
            compiled,
            initial,
            ctx,
            self.registry.clone(),
        )
        .await
        .map_err(DispatchError::from)?;
        Ok(stream_exec)
    }

    pub async fn try_dispatch_streaming(
        &self,
        model_name: &str,
        service_type: &str,
        initial: FlowEnvelope,
        meta: FlowRequestMeta,
    ) -> std::result::Result<StreamingExecution, DispatchError> {
        let modality = derive_modality(&initial);
        let cache_key = format!("{}:{}:{}", model_name, service_type, modality);
        let compiled = match self
            .resolve_cached(&cache_key, model_name, service_type, modality)
            .await
            .map_err(DispatchError::from)?
        {
            ResolvedFlow::Found(cached) => {
                if !self.acl_allow(&cached.flow.id, &meta) {
                    return Err(DispatchError::Denied {
                        flow_id: cached.flow.id.clone(),
                    });
                }
                if !cached.compiled.is_streaming {
                    // User-defined blocking-only flow — wykonaj blocking
                    // i opakuj outcome jako single-chunk stream.
                    let outcome = self
                        .run_blocking(
                            cached.compiled.clone(),
                            initial,
                            meta,
                            RunDescriptor::resolved(model_name, service_type, modality),
                        )
                        .await
                        .map_err(DispatchError::from)?;
                    return Ok(wrap_blocking_as_stream(outcome, self.blobs()));
                }
                cached.compiled.clone()
            }
            ResolvedFlow::NotFound => {
                // Brak jawnego flow — model wykonywany BEZPOŚREDNIO. Tylko `llm`
                // (chat text) streamuje natywnie; vision/tts/stt/embeddings są
                // blocking-only, więc wykonują się blocking i opakowują w
                // single-chunk stream (parytet z user-defined blocking flow).
                let node = direct_node(service_type, model_name, modality)?;
                if node.node_type == "llm" {
                    // Direct capability streaming: no flow was resolved, so
                    // `flow_id` stays absent (same reason as
                    // `run_direct_blocking`).
                    let ctx = self.ctx_factory.make_context(
                        &meta,
                        RunDescriptor::resolved(model_name, service_type, modality),
                    );
                    return execute_direct_streaming(
                        self.db.clone(),
                        node,
                        initial,
                        ctx,
                        self.registry.clone(),
                    )
                    .await
                    .map_err(DispatchError::from);
                }
                // Blocking-only capability: idź przez `run_direct_blocking` żeby
                // objąć backstop (FLOW_BACKSTOP_SECS) + cancel_token — goły
                // `execute_direct_blocking` mógłby zawisnąć w nieskończoność, gdy
                // backend stalluje przed wyprodukowaniem single-chunku (i trzymać
                // otwarty top-level stream compliance event do końca awaita).
                let outcome = self
                    .run_direct_blocking(service_type, model_name, modality, initial, meta)
                    .await?;
                return Ok(wrap_blocking_as_stream(outcome, self.blobs()));
            }
            ResolvedFlow::CompileFailed => {
                return Err(DispatchError::CompileFailed {
                    flow_id: String::new(),
                    msg: format!("user-defined streaming flow for '{model_name}/{service_type}'"),
                });
            }
        };
        let ctx = self.ctx_factory.make_context(
            &meta,
            RunDescriptor::resolved(model_name, service_type, modality)
                .with_flow(&compiled.flow_id),
        );
        let stream_exec = execute_streaming(
            self.db.clone(),
            compiled,
            initial,
            ctx,
            self.registry.clone(),
        )
        .await
        .map_err(DispatchError::from)?;
        Ok(stream_exec)
    }

    async fn run_blocking(
        &self,
        compiled: Arc<CompiledFlow>,
        initial: FlowEnvelope,
        meta: FlowRequestMeta,
        descriptor: RunDescriptor,
    ) -> Result<FlowExecutionOutcome> {
        // The flow id is taken from the flow about to execute, not from the id
        // a caller asked for: the record names what ran. The rest of the
        // descriptor is whatever the caller resolved on — empty for a dispatch
        // by flow id, which went through no model routing key at all.
        let ctx = self
            .ctx_factory
            .make_context(&meta, descriptor.with_flow(&compiled.flow_id));
        let flow_id = compiled.flow_id.clone();
        // Per-node 600 s (w executorze) + `max_iterations` w pętlach domykają
        // czas wykonania. Ten globalny `timeout` to wyłącznie backstop na
        // faktycznie zawieszony flow — celowo hojny (1 h), żeby nie ucinał
        // normalnego multi-hop RAG ani per-chunk extraction. `cancel_token`
        // (klient disconnect) działa niezależnie, między nodami w executorze.
        match timeout(
            Duration::from_secs(FLOW_BACKSTOP_SECS),
            execute_blocking(
                self.db.clone(),
                compiled,
                initial,
                ctx,
                self.registry.clone(),
            ),
        )
        .await
        {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(e)) => {
                warn!(flow_id, "Blad wykonania flow: {e}");
                Err(e)
            }
            Err(_) => {
                warn!(
                    flow_id,
                    "Backstop flow po {FLOW_BACKSTOP_SECS}s (zawieszony flow)"
                );
                Err(anyhow::anyhow!(
                    "flow {flow_id} backstop timeout after {FLOW_BACKSTOP_SECS}s"
                ))
            }
        }
    }

    fn acl_allow(&self, flow_id: &str, meta: &FlowRequestMeta) -> bool {
        let Some(uid) = meta.user_id.as_deref() else {
            return true;
        };
        let role = meta.user_role.clone().unwrap_or_else(|| "user".into());
        let allowed = acl::check_access_safe(&self.db, "flow", flow_id, uid, &role);
        if !allowed {
            tracing::warn!(user_id = uid, flow_id, "ACL denied flow execution");
        }
        allowed
    }

    async fn resolve_cached(
        &self,
        cache_key: &str,
        model_name: &str,
        service_type: &str,
        request_modality: &'static str,
    ) -> Result<ResolvedFlow> {
        // Cache hit: Some(cached) = Found, None = CompileFailed (negative cache)
        if let Some(slot) = self.cache.get(cache_key) {
            return Ok(match slot {
                Some(cached) => ResolvedFlow::Found(cached),
                None => ResolvedFlow::CompileFailed,
            });
        }
        let pool = self.db.clone();
        let model_owned = model_name.to_string();
        let service_owned = service_type.to_string();
        let resolved = tokio::task::spawn_blocking(move || {
            resolver::resolve_flow(&pool, &model_owned, &service_owned, request_modality)
        })
        .await??;
        match resolved {
            Some(flow) => {
                let compiled =
                    match CompiledFlow::from_json(&flow.id, &flow.flow_json, &self.registry) {
                        Ok(c) => Arc::new(c),
                        Err(e) => {
                            warn!(cache_key, "compile failed for flow id={}: {e}", flow.id);
                            // Negative cache TYLKO dla compile failure. Admin musi
                            // naprawić flow_json — synthetic fallback NIE aktywuje
                            // tutaj (admin chciał konkretny flow).
                            self.cache.set(cache_key, None);
                            return Ok(ResolvedFlow::CompileFailed);
                        }
                    };
                let cached = Arc::new(CachedFlow { flow, compiled });
                self.cache.set(cache_key, Some(cached.clone()));
                Ok(ResolvedFlow::Found(cached))
            }
            None => {
                // Brak negative cache dla resolver=None — synthetic ma odpalić
                // za każdym razem (z cache w synthetic slot, LRU).
                Ok(ResolvedFlow::NotFound)
            }
        }
    }

    /// Direct (flow-less) blocking execution dla pary (service_type, model):
    /// model uruchamiany BEZPOŚREDNIO na executorze przez pojedynczą capability
    /// (llm/vision_llm/tts/stt/embeddings) — bez trigger/output wrappera, bez
    /// pii_filter. Backstop timeout jak w `run_blocking` łapie zawieszony
    /// backend; `meta.deadline`/`cancel_token` działają niezależnie w dispatcherze
    /// capability.
    async fn run_direct_blocking(
        &self,
        service_type: &str,
        model: &str,
        modality: &str,
        initial: FlowEnvelope,
        meta: FlowRequestMeta,
    ) -> std::result::Result<FlowExecutionOutcome, DispatchError> {
        let node = direct_node(service_type, model, modality)?;
        let node_type = node.node_type.clone();
        // No `flow_id`: this path runs ONE capability node straight on the
        // executor because the resolver found no user-defined flow. There is no
        // flow to name, and naming the synthetic node type instead would put a
        // value in the audit row that matches no row in `flows`.
        let ctx = self.ctx_factory.make_context(
            &meta,
            RunDescriptor::resolved(model, service_type, modality),
        );
        match timeout(
            Duration::from_secs(FLOW_BACKSTOP_SECS),
            execute_direct_blocking(node, initial, ctx, self.registry.clone()),
        )
        .await
        {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(e)) => {
                warn!(model, node_type, "Blad direct execution: {e}");
                Err(DispatchError::from(e))
            }
            Err(_) => {
                warn!(
                    model,
                    node_type, "Backstop direct execution po {FLOW_BACKSTOP_SECS}s"
                );
                Err(DispatchError::Internal(format!(
                    "direct '{node_type}' backstop timeout after {FLOW_BACKSTOP_SECS}s"
                )))
            }
        }
    }
}

/// Buduje węzeł capability dla direct (flow-less) wykonania pary
/// `model:service_type:modality`. Odwzorowuje capability którą wykonywał usunięty
/// synthetic flow (llm / vision_llm / tts / stt / embeddings), ale węzeł jest
/// uruchamiany bezpośrednio na executorze — bez trigger/output i bez pii_filter.
/// Nieznany `service_type` → `Unsupported`.
fn direct_node(
    service_type: &str,
    model: &str,
    modality: &str,
) -> std::result::Result<FlowNode, DispatchError> {
    // Image-modality chat: vision_llm konsumuje obraz i zwraca tekst (blocking).
    let is_vision = service_type == "chat" && modality == "image";
    let node_type = match service_type {
        "chat" if is_vision => "vision_llm",
        "chat" => "llm",
        "tts" => "tts",
        "stt" => "stt",
        "embeddings" => "embeddings",
        _ => {
            return Err(DispatchError::Unsupported {
                service_type: service_type.to_string(),
                model: model.to_string(),
            });
        }
    };
    Ok(FlowNode {
        id: format!("direct_{node_type}"),
        node_type: node_type.to_string(),
        config: serde_json::json!({ "model": model }),
        position: None,
        label: None,
        region: None,
    })
}

/// Etap 3b: derive request modality z initial envelope payload — vision
/// flows MUSZĄ być explicit bound, default flow działa tylko dla text.
/// `Image` payload → "image", reszta (Text/Empty/Json/...) → "text".
fn derive_modality(envelope: &FlowEnvelope) -> &'static str {
    match envelope.payload {
        FlowValue::Image { .. } => "image",
        _ => "text",
    }
}

/// Stage 3d-0a-5: opakowuje blocking `FlowExecutionOutcome` w `StreamingExecution`
/// żeby user-defined blocking-only flow miał ten sam wire shape co native
/// streaming flow. Klient SSE konsumuje jednolicie — single chunk z całością
/// payloadu + finish_reason ze stop. Outcome `oneshot` channel jest natychmiast
/// rozwiązany — wrapper nie czeka na EOF, blocking już skończył.
///
/// Dla `FlowValue::Audio { blob_ref, mime, .. }` (np. blocking TTS-as-flow przez
/// `/v1/audio/speech/flow-stream`) wrapper fetchuje bytes z `BlobStore` i emit'uje
/// `EnvelopeDelta::Audio` zamiast Llm-z-JSON-em — żeby audio sink endpoint dostał
/// realne ramki.
fn wrap_blocking_as_stream(
    outcome: FlowExecutionOutcome,
    blobs: Arc<dyn BlobStore>,
) -> StreamingExecution {
    use futures::stream::StreamExt;
    let payload_for_stream = outcome.final_envelope.payload.clone();
    let usage = outcome.usage.clone();
    let perf = outcome.perf;
    let finish = outcome.finish_reason.clone();
    let err = outcome.error.clone();
    let stream = futures::stream::once(async move {
        match payload_for_stream {
            FlowValue::Audio {
                blob_ref,
                mime,
                sample_rate,
            } => {
                let bytes = blobs
                    .get(&blob_ref)
                    .await
                    .map_err(|e| anyhow::anyhow!("audio blob fetch: {e}"))?;
                Ok(EnvelopeDelta::Audio(AudioStreamChunk {
                    choice_index: 0,
                    bytes_delta: bytes,
                    mime,
                    sample_rate,
                    finish_reason: Some(finish),
                }))
            }
            other => {
                let text_delta = match &other {
                    FlowValue::Text(t) => t.clone(),
                    FlowValue::Empty => String::new(),
                    v => serde_json::to_string(&crate::flow_engine::converter::payload_to_json(v))
                        .unwrap_or_default(),
                };
                Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                    choice_index: 0,
                    text_delta,
                    reasoning_delta: None,
                    tool_calls: Vec::new(),
                    usage: Some(usage),
                    perf,
                    finish_reason: Some(finish),
                    error: err,
                }))
            }
        }
    })
    .boxed();
    // No producer ran here — the final envelope is the closest equivalent, it
    // carries the meta every node wrote on the way.
    let producer_input = Arc::new(outcome.final_envelope.clone());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = tx.send(outcome);
    StreamingExecution {
        stream,
        outcome: rx,
        producer_input,
    }
}

/// Buduje AdapterRegistry z wszystkimi 14 adapterami (13 stage 1c +
/// tts_stream_bridge stage 3d Krok 2b). Side effect-free.
///
/// Streaming-aware adaptery (`pii_filter`, `tts_stream_bridge`)
/// rejestrowane przez `register_streaming<T>` — landują w obu slotach
/// (blocking + streaming). Czysta NodeAdapter rejestracja przez
/// `register` dla nodów które nie mają stream variant'a.
fn build_registry(
    agent_service: crate::agents::AgentServiceSlot,
    subflow_runner: SubflowRunnerSlot,
) -> AdapterRegistry {
    use crate::flow_engine::node_adapters::SentenceBufferNodeAdapter;

    let mut r = AdapterRegistry::new();
    let arcs: Vec<Arc<dyn NodeAdapter>> = vec![
        Arc::new(TriggerNodeAdapter::new()),
        // Reactive entry: a flow keyed on a sub-agent completion event. Same
        // entry shape as `trigger`; the reactor seeds its initial envelope.
        Arc::new(OnSubagentCompleteNodeAdapter::new()),
        // M3a (PLAN §6.3): TentaBus flow blocks. `bus_consume` is the third
        // R5 entry kind — same no-slot shape as `on_subagent_complete`, seeded
        // by `bus::reactor` reaching the process-global `bus::global()`
        // directly, not through a registry dependency slot. `bus_publish`/
        // `bus_transform` are mid-flow and also reach `bus::global()` on
        // their own.
        Arc::new(crate::flow_engine::node_adapters::BusConsumeNodeAdapter::new()),
        Arc::new(crate::flow_engine::node_adapters::BusPublishNodeAdapter::new()),
        Arc::new(crate::flow_engine::node_adapters::BusTransformNodeAdapter::new()),
        Arc::new(OutputNodeAdapter::new()),
        Arc::new(ConditionNodeAdapter::new()),
        Arc::new(CombineNodeAdapter::new()),
        Arc::new(SttNodeAdapter::new()),
        Arc::new(EmbeddingsNodeAdapter::new()),
        Arc::new(RerankerNodeAdapter::new()),
        // RAG E2.2 — węzły pętli multi-hop: seed pod-pytania, akumulacja+dedup
        // pasaży, parsowanie werdyktu sędziego LLM.
        Arc::new(RagQuerySeedNodeAdapter::new()),
        Arc::new(RagAccumulateNodeAdapter::new()),
        Arc::new(RagJudgeNodeAdapter::new()),
        Arc::new(RagFinalizeNodeAdapter::new()),
        // RAG E3.2 — hop grafowy (GraphRAG): identyfikacja encji zapytania ->
        // seedy PPR, oraz PPR/neighbors -> fakty grafowe fuzowane z pasazami.
        // Best-effort: bez feature `graph` / bez encji w grafie -> pass-through.
        Arc::new(RagGraphSeedNodeAdapter::new()),
        Arc::new(RagGraphFactsNodeAdapter::new()),
        // RAG E1.0 — węzeł retrievalu scoped do (org, addon_instance, namespace).
        Arc::new(VectorNodeAdapter::new()),
        // Project Studio knowledge base as a flow block: per-member ACL search
        // over a project's `passages` namespace + source catalog listing.
        Arc::new(ProjectKnowledgeNodeAdapter::new()),
        // PARTIA 1 (flow-ingest RAG) — czysto-rustowe węzły ingestu bez modeli:
        // klasyfikacja+routing pliku, rasteryzacja PDF, ekstrakcja office,
        // chunking, scalanie stron i zapis chunków do przestrzeni wektorowej.
        Arc::new(DocumentRouterNodeAdapter::new()),
        // Jawny switch platformy — jedno wejście (Any), 5 wyjść (android/ios/
        // macos/windows/linux); aktywuje port = bieżący target_os. Widoczny na
        // diagramie router gałęzi per-urządzenie (uniwersalny flow).
        Arc::new(PlatformSwitchNodeAdapter::new()),
        Arc::new(TextExtractNodeAdapter::new()),
        Arc::new(PdfRasterizeNodeAdapter::new()),
        Arc::new(ExcelExtractNodeAdapter::new()),
        Arc::new(WordExtractNodeAdapter::new()),
        Arc::new(PptxExtractNodeAdapter::new()),
        Arc::new(ChunkNodeAdapter::new()),
        Arc::new(DocumentMergeNodeAdapter::new()),
        // Mostek chunk→store: wektoryzuje listę chunków i dokłada `embedding` do
        // każdego, dając kształt wprost konsumowalny przez `store`.
        Arc::new(EmbedChunksNodeAdapter::new()),
        // PARTIA 2 (flow-ingest RAG) — węzły zależne od modeli: parsowanie strony
        // na markdown przez VLM (vision-chat) oraz detektory struktury dokumentu
        // (layout / tabele / grafika / OCR) przez typed surface Documents.
        Arc::new(VisionParseNodeAdapter::new()),
        // Jawny blok parse na powierzchni document-parse (ctx.documents.parse).
        // Model widoczny w configu (paddle-ocr-mlx / nemotron-parse / ...);
        // resolver dobiera backend per-urządzenie (embedded MLX / docker / mesh).
        Arc::new(DocumentParseNodeAdapter::new()),
        Arc::new(PageDetectNodeAdapter::new()),
        Arc::new(TableStructureNodeAdapter::new()),
        Arc::new(GraphicElementsNodeAdapter::new()),
        Arc::new(OcrNodeAdapter::new()),
        // Batch-owe warianty gałęzi PDF (cardinality 1:1): cała lista stron
        // (Json{pages:[blob_refs]}) jako JEDEN envelope → wzbogacona lista stron
        // konsumowalna przez `document_merge`. Reużywają ścieżki single-image.
        Arc::new(VisionParsePagesNodeAdapter::new()),
        Arc::new(PageDetectPagesNodeAdapter::new()),
        Arc::new(OcrPagesNodeAdapter::new()),
        Arc::new(StoreNodeAdapter::new()),
        Arc::new(MemoryNodeAdapter::new()),
        Arc::new(ConversationHistoryNodeAdapter::new()),
        Arc::new(PersistTurnNodeAdapter::new()),
        Arc::new(SessionContextNodeAdapter::new()),
        Arc::new(SpeakerContextNodeAdapter::new()),
        Arc::new(VisionNodeAdapter::new()),
        Arc::new(VisionOcrNodeAdapter::new()),
        Arc::new(VisionClassifyNodeAdapter::new()),
        Arc::new(CameraVerdictNodeAdapter::new()),
        Arc::new(CameraAlertNodeAdapter::new()),
        // ask_user (§3.13 C) — BPMN User Task: no dependency slot, it uses the
        // process-global interaction registry + run manager.
        Arc::new(AskUserNodeAdapter::new()),
        // Deterministic background blocks (§3.3–3.5): await_subagents + subagent_
        // status + interval reach the process-global AgentRunManager directly (no
        // slot); only `spawn` needs the AgentService slot to resolve agent_id→name.
        Arc::new(AwaitSubagentsNodeAdapter::new()),
        Arc::new(SubagentStatusNodeAdapter::new()),
        Arc::new(IntervalNodeAdapter::new()),
        // The block that ends a review loop on a VERDICT rather than on the
        // absence of tool calls.
        Arc::new(CriticGateNodeAdapter::new()),
        // Knowledge-graph write side of an ingest. Registered UNCONDITIONALLY,
        // unlike `graph_search` below: a flow seeded with this node has to pass
        // adapter validation on a default-features build too, and the node
        // degrades to a passthrough there instead of failing to resolve.
        Arc::new(GraphExtractNodeAdapter::new()),
    ];
    for a in arcs {
        r.register(a);
    }
    // RAG E1.1 — węzeł graph-retrievalu scoped do (org, addon_instance, collection).
    // Pod `feature = "graph"` (cozo opt-in), obok `vector`.
    #[cfg(feature = "graph")]
    r.register(Arc::new(
        crate::flow_engine::node_adapters::GraphSearchNodeAdapter::new(),
    ));
    // Harness §3.5: agent_context + tool_exec + agent_router + compact_context
    // share the late-bound AgentServiceSlot (filled by main.rs after the
    // AddonManager exists). agent_router/compact_context also issue audited LLM
    // calls via ctx.llm; they need only the registry/service.
    r.register(Arc::new(AgentContextNodeAdapter::new(
        agent_service.clone(),
    )));
    r.register(Arc::new(ToolExecNodeAdapter::new(agent_service.clone())));
    // `workspace_context` reads the running agent's allowlist to publish
    // `harness_tools`, so it shares the same late-bound service slot.
    r.register(Arc::new(WorkspaceContextNodeAdapter::new(
        agent_service.clone(),
    )));
    // The plan gate reads the session's task rows, so it needs the same slot to
    // reach the registry database that names the workspace.
    r.register(Arc::new(TaskGateNodeAdapter::new(agent_service.clone())));
    // Code Studio (§16.4). `patch_review`, `exec_command` and `delegate_cli`
    // reach the process-global interaction registry and run manager directly,
    // like `ask_user` — and the SAME service slot: `exec_command` needs it for
    // the agent's allowlist (§10), `delegate_cli` for the registry database and
    // the node's settings key, which is what opens the provider credential in
    // the node-local vault (§5.2).
    r.register(Arc::new(PatchReviewNodeAdapter::new(agent_service.clone())));
    r.register(Arc::new(ExecCommandNodeAdapter::new(agent_service.clone())));
    r.register(Arc::new(DelegateCliNodeAdapter::new(agent_service.clone())));
    r.register(Arc::new(AgentRouterNodeAdapter::new(agent_service.clone())));
    // `spawn` resolves an agent_id config to the agent's name through the service
    // before delegating in the background (§3.3).
    r.register(Arc::new(SpawnNodeAdapter::new(agent_service.clone())));
    r.register(Arc::new(CompactContextNodeAdapter::new()));
    // Harness §3.5 blocks 1/2/6/8: subflow + loop + map + agent all share the
    // SubflowRunnerSlot (filled by FlowDispatcher::new) — each runs another flow
    // as its body. `agent` additionally needs the AgentServiceSlot to resolve
    // the agent's harness flow id.
    //
    // §3.11 B: subflow / loop / agent register as stream producers too (dual
    // NodeAdapter + StreamProducerAdapter) — a flow wiring their `stream` output
    // port forwards the inner final unit's token stream (subflow → child flow,
    // loop → final iteration, agent → harness flow's loop). `map` stays blocking
    // (its result is an aggregated array, not a single streamable unit).
    r.register_stream_producer(Arc::new(SubflowNodeAdapter::new(subflow_runner.clone())));
    r.register_stream_producer(Arc::new(LoopNodeAdapter::new(subflow_runner.clone())));
    r.register(Arc::new(MapNodeAdapter::new(subflow_runner.clone())));
    r.register_stream_producer(Arc::new(AgentNodeAdapter::new(
        agent_service,
        subflow_runner,
    )));
    r.register_llm(Arc::new(LlmNodeAdapter::new()));
    // Streaming-aware adaptery (dual-trait NodeAdapter + StreamingNodeAdapter)
    // trafiają do obu slotów. `tts` jest dual: blocking (całość) + streaming
    // (buforowanie zdań + synteza per zdanie) — jeden node na oba tryby.
    r.register_streaming(Arc::new(PiiFilterNodeAdapter::new()));
    r.register_streaming(Arc::new(TtsNodeAdapter::new()));
    r.register_streaming(Arc::new(TtsCleanNodeAdapter::new()));
    r.register_streaming(Arc::new(SentenceBufferNodeAdapter::new()));
    r
}

/// Builds an AdapterRegistry with empty dependency slots — for tests that need
/// the full builtin adapter set (e.g. compiling a flow body for the
/// SubflowRunner) without a live AgentService or SubflowRunner. The subflow
/// adapter's own slot is filled by the caller after the registry Arc exists.
#[cfg(any(test, feature = "test-support"))]
pub fn build_registry_for_test() -> AdapterRegistry {
    build_registry(
        Arc::new(parking_lot::RwLock::new(None)),
        Arc::new(parking_lot::RwLock::new(None)),
    )
}

/// Like `build_registry_for_test` but wires the given `SubflowRunnerSlot` into
/// the subflow / loop / map / agent adapters — for tests that drive these
/// blocks through a live `SubflowRunner` (e.g. end-to-end harness streaming).
/// The caller fills the slot with a runner whose registry `Weak` points at the
/// returned registry once it is `Arc`-wrapped.
#[cfg(any(test, feature = "test-support"))]
pub fn build_registry_with_runner(subflow_runner: SubflowRunnerSlot) -> AdapterRegistry {
    build_registry(Arc::new(parking_lot::RwLock::new(None)), subflow_runner)
}

/// Minimal `ContextFactory` over the `test_support` stubs — for tests that need
/// the REAL `FlowRequestMeta` → `ExecutionContext` copy without a live
/// `FlowDispatcher`. That copy is the last link of the identity chain: caller
/// (`service_call`) → executor (`FlowRequestMeta` for a flow target) →
/// `make_context` → the `ExecutionContext` the nodes run under.
#[cfg(any(test, feature = "test-support"))]
fn stub_context_factory() -> ContextFactory {
    use crate::flow_engine::dispatchers::clock::SystemClock;
    use crate::flow_engine::dispatchers::metrics::NoopMetrics;
    use crate::flow_engine::node_adapter::test_support::{
        stub_vectors, StubAudit, StubDocuments, StubEmbeddings, StubHistory, StubLlm, StubMemory,
        StubPiiRules, StubPrompts, StubReranker, StubStt, StubTts, StubTtsCleaning,
    };
    ContextFactory {
        clock: Arc::new(SystemClock),
        blobs: Arc::new(crate::flow_engine::blob_store::InMemoryBlobStore::new()),
        vectors: stub_vectors(),
        #[cfg(feature = "graph")]
        graph: crate::flow_engine::node_adapter::test_support::stub_graph(),
        llm: Arc::new(StubLlm),
        embeddings: Arc::new(StubEmbeddings),
        reranker: Arc::new(StubReranker),
        documents: Arc::new(StubDocuments),
        stt: Arc::new(StubStt),
        tts: Arc::new(StubTts),
        // Pusty slot executora — stub factory testów idzie ścieżką fallback
        // (bezpośrednie singletony) w VisionDispatcherImpl.
        vision: Arc::new(
            crate::flow_engine::dispatchers_impl::VisionDispatcherImpl::new(Arc::new(
                parking_lot::RwLock::new(None),
            )),
        ),
        prompts: Arc::new(StubPrompts),
        memory: Arc::new(StubMemory),
        history: Arc::new(StubHistory),
        audit: Arc::new(StubAudit),
        metrics: Arc::new(NoopMetrics),
        pii_rules: Arc::new(StubPiiRules),
        tts_cleaning: Arc::new(StubTtsCleaning),
    }
}

/// Builds a flow `ExecutionContext` on stub dispatchers from a `FlowRequestMeta`
/// — for tests in other modules (the subflow runner) that need the REAL
/// meta → context copy without a live `FlowDispatcher`.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn make_test_context(
    meta: &FlowRequestMeta,
) -> crate::flow_engine::node_adapter::ExecutionContext {
    stub_context_factory().make_context(meta, RunDescriptor::default())
}

/// Walks a closed cycle over the variants of a fieldless enum, returning every
/// variant it visits.
///
/// The point is what the CALLER has to write to use it: `after` is an
/// exhaustive, wildcard-free match, so a variant added to the enum stops this
/// crate's tests compiling until it is linked into the cycle. `index` is the
/// variant's own discriminant, and variants of a fieldless enum are numbered
/// from zero — a variant left dangling instead of linked in therefore leaves the
/// walk shorter than the highest discriminant it saw, which is the second net.
#[cfg(test)]
fn walk_variant_cycle<T>(start: T, after: fn(T) -> T, index: fn(T) -> usize) -> Vec<T>
where
    T: Copy + PartialEq + std::fmt::Debug,
{
    let mut walk = vec![start];
    let mut current = after(start);
    while current != start {
        assert!(
            walk.len() < 256,
            "the successor match never leads back to {start:?}"
        );
        walk.push(current);
        current = after(current);
    }
    let highest = walk
        .iter()
        .map(|variant| index(*variant))
        .max()
        .expect("the walk always holds at least `start`");
    assert_eq!(
        walk.len(),
        highest + 1,
        "a variant is missing from the cycle; walked {walk:?}"
    );
    walk
}

/// Every [`FlowOrigin`], enumerated by the compiler rather than by memory.
///
/// A hand-written list is what let `Dashboard` and `Meeting` sit outside the
/// round-trip guard while their `parse` arms were, in `Meeting`'s case, the only
/// thing keeping meeting-bot rows readable.
#[cfg(test)]
pub(crate) fn all_flow_origins() -> Vec<FlowOrigin> {
    fn after(origin: FlowOrigin) -> FlowOrigin {
        match origin {
            FlowOrigin::Chat => FlowOrigin::Dashboard,
            FlowOrigin::Dashboard => FlowOrigin::Project,
            FlowOrigin::Project => FlowOrigin::CodeStudio,
            FlowOrigin::CodeStudio => FlowOrigin::Api,
            FlowOrigin::Api => FlowOrigin::Addon,
            FlowOrigin::Addon => FlowOrigin::Camera,
            FlowOrigin::Camera => FlowOrigin::Meeting,
            FlowOrigin::Meeting => FlowOrigin::Scheduler,
            FlowOrigin::Scheduler => FlowOrigin::Mesh,
            FlowOrigin::Mesh => FlowOrigin::Agent,
            FlowOrigin::Agent => FlowOrigin::System,
            FlowOrigin::System => FlowOrigin::Bus,
            FlowOrigin::Bus => FlowOrigin::Chat,
        }
    }
    walk_variant_cycle(FlowOrigin::Chat, after, |origin| origin as usize)
}

/// Every [`ActorKind`] — same construction, same reason.
#[cfg(test)]
pub(crate) fn all_actor_kinds() -> Vec<ActorKind> {
    fn after(kind: ActorKind) -> ActorKind {
        match kind {
            ActorKind::User => ActorKind::ApiKey,
            ActorKind::ApiKey => ActorKind::Addon,
            ActorKind::Addon => ActorKind::System,
            ActorKind::System => ActorKind::User,
        }
    }
    walk_variant_cycle(ActorKind::User, after, |kind| kind as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_context_propagates_addon_identity_from_meta() {
        let factory = stub_context_factory();
        let mut meta = FlowRequestMeta::new("req-1", FlowOrigin::Api, FlowActor::system());
        meta.addon_id = Some("inst-rag-1".to_string());
        meta.org_id = Some("org-7".to_string());
        let ctx = factory.make_context(&meta, RunDescriptor::default());
        assert_eq!(ctx.addon_id.as_deref(), Some("inst-rag-1"));
        assert_eq!(ctx.org_id.as_deref(), Some("org-7"));
    }

    #[test]
    fn make_context_non_addon_call_leaves_identity_none() {
        let factory = stub_context_factory();
        // Domyślne meta (np. routing /v1 user / kamera / agent) — bez tożsamości.
        let meta = FlowRequestMeta::new("req-2", FlowOrigin::Api, FlowActor::system());
        let ctx = factory.make_context(&meta, RunDescriptor::default());
        assert!(ctx.addon_id.is_none());
        assert!(ctx.org_id.is_none());
    }

    /// Records the provenance the ENGINE ran a node under. Registered into a
    /// real `AdapterRegistry` and driven through `execute_blocking`, so the
    /// hostile envelope actually reaches a node instead of being assigned to a
    /// context after the fact.
    struct ProvenanceRecorder {
        seen: Arc<
            std::sync::Mutex<
                Option<(
                    FlowOrigin,
                    ActorKind,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                )>,
            >,
        >,
    }

    #[async_trait::async_trait]
    impl crate::flow_engine::node_adapter::NodeAdapter for ProvenanceRecorder {
        fn node_type(&self) -> &str {
            "provenance_recorder"
        }
        fn input_ports(&self) -> Vec<crate::flow_engine::node_adapter::PortSpec> {
            vec![crate::flow_engine::node_adapter::PortSpec {
                name: "text".to_string(),
                data_type: crate::flow_engine::types::FlowDataType::Any,
            }]
        }
        fn output_ports(&self) -> Vec<crate::flow_engine::node_adapter::PortSpec> {
            vec![crate::flow_engine::node_adapter::PortSpec {
                name: "text".to_string(),
                data_type: crate::flow_engine::types::FlowDataType::Any,
            }]
        }
        async fn execute(
            &self,
            _node: &crate::flow_engine::types::FlowNode,
            inputs: &[crate::flow_engine::envelope::NodeInput],
            ctx: &crate::flow_engine::node_adapter::ExecutionContext,
        ) -> Result<FlowEnvelope, anyhow::Error> {
            *self.seen.lock().unwrap() = Some((
                ctx.origin,
                ctx.actor_kind,
                ctx.actor_id.clone(),
                ctx.actor_user_id.clone(),
                ctx.correlation_id.clone(),
            ));
            Ok(inputs
                .first()
                .map(|i| (*i.envelope).clone())
                .unwrap_or_else(|| (*ctx.initial_envelope).clone()))
        }
    }

    fn forgery_test_db() -> DbPool {
        let pool = crate::db::init(std::path::Path::new(":memory:")).expect("in-memory db");
        {
            let conn = pool.write().expect("db lock");
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status) VALUES ('0', 'forge', '{}', 'active')",
                [],
            )
            .expect("seed flow");
        }
        pool
    }

    /// §3 invariant 1 — `origin` / `actor*` / `correlation_id` are minted by the
    /// server and are NOT derivable from model output. `envelope.meta` is
    /// writable by every node (a WASM addon block deserializes a whole envelope
    /// from the guest's answer), so an envelope carrying its own
    /// `origin`/`actor_kind`/`actor_id` must change NOTHING about what a node
    /// sees.
    ///
    /// This drives the REAL path: the hostile envelope is the one
    /// `execute_blocking` seeds as `ctx.initial_envelope` and hands to the
    /// nodes, and a recording adapter reports what the engine actually ran
    /// under. Asserting on a context built by `make_context(&meta)` — which
    /// takes no envelope — and assigning the envelope afterwards would prove
    /// nothing, because the forged values never reach anything.
    #[tokio::test]
    async fn envelope_meta_cannot_forge_server_minted_provenance() {
        let seen = Arc::new(std::sync::Mutex::new(None));
        let mut registry = AdapterRegistry::new();
        registry.register(Arc::new(
            crate::flow_engine::node_adapters::TriggerNodeAdapter::new(),
        ));
        registry.register(Arc::new(
            crate::flow_engine::node_adapters::OutputNodeAdapter::new(),
        ));
        registry.register(Arc::new(ProvenanceRecorder { seen: seen.clone() }));
        let registry = Arc::new(registry);

        let flow_json = r#"{
            "nodes":[
                {"id":"t1","type":"trigger","config":{}},
                {"id":"r1","type":"provenance_recorder","config":{}},
                {"id":"o1","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t1","to":"r1","from_port":"text","to_port":"text"},
                {"from":"r1","to":"o1","from_port":"text","to_port":"text"}
            ]
        }"#;
        let compiled = Arc::new(
            crate::flow_engine::cache::CompiledFlow::from_json("0", flow_json, &registry)
                .expect("compile"),
        );

        // What a compromised node / model output would try to inject.
        let mut forged = FlowEnvelope::empty();
        forged.payload = crate::flow_engine::envelope::FlowValue::Text("hi".into());
        for (k, v) in [
            ("origin", "chat"),
            ("actor_kind", "user"),
            ("actor_id", "root"),
            ("actor_user_id", "root"),
            ("correlation_id", "forged-corr"),
        ] {
            forged
                .meta
                .insert(k.into(), serde_json::Value::String(v.into()));
        }

        let mut meta = FlowRequestMeta::new(
            "req-forge",
            FlowOrigin::Camera,
            FlowActor::system_component("cam-hall"),
        );
        meta.correlation_id = Some("server-corr".into());
        let ctx = stub_context_factory().make_context(&meta, RunDescriptor::default());

        crate::flow_engine::executor::execute_blocking(
            forgery_test_db(),
            compiled,
            forged,
            ctx,
            registry,
        )
        .await
        .expect("flow runs");

        let (origin, actor_kind, actor_id, actor_user_id, correlation_id) =
            seen.lock().unwrap().clone().expect("recorder ran");
        assert_eq!(origin, FlowOrigin::Camera);
        assert_eq!(actor_kind, ActorKind::System);
        assert_eq!(actor_id.as_deref(), Some("cam-hall"));
        assert_eq!(actor_user_id, None);
        assert_eq!(correlation_id.as_deref(), Some("server-corr"));
    }

    /// §2.10.3 — `correlation_id` is the DEEP LINK between an accountability
    /// entry in `audit_log` and the point on the timeline that produced it, and
    /// a link is only a link while both ends say the same thing. The two ends
    /// are written by two different copies of the same value: the run row takes
    /// it off the `ExecutionContext`, the audit mirror takes it off the
    /// provenance `begin_run` bound for the run. Checking either end against a
    /// literal the test itself wrote cannot see them drift apart, so this runs
    /// ONE execution and compares the two OBSERVED values.
    #[tokio::test]
    async fn one_run_stamps_one_correlation_id_on_its_row_and_its_audit_entry() {
        use rusqlite::OptionalExtension;

        let main_db = forgery_test_db();
        let events_dir = tempfile::tempdir().expect("tempdir");
        let events_pool =
            crate::events::db::open_pool_at(&events_dir.path().join("events.db")).expect("events");
        // `begin_run` keeps the provenance binding only while something is
        // watching the scope; without the process-wide log there is no
        // subscriber, no timeline row and therefore no audit mirror to compare.
        crate::events::progress_log::start(events_pool.clone(), main_db.clone());

        let run_id = format!("req-{}", uuid::Uuid::new_v4());
        let mut meta = FlowRequestMeta::new(
            run_id.clone(),
            FlowOrigin::Api,
            FlowActor::api_key("key-77", Some("u-5".into())),
        );
        meta.correlation_id = Some(format!("corr-{}", uuid::Uuid::new_v4()));
        meta.progress_sink = Some(Arc::new(
            crate::flow_engine::progress_broker::BrokerProgressSink::new(
                crate::flow_engine::progress_broker::global_broker(),
            ),
        ));
        let ctx = stub_context_factory().make_context(&meta, RunDescriptor::default());

        let mut registry = AdapterRegistry::new();
        registry.register(Arc::new(
            crate::flow_engine::node_adapters::TriggerNodeAdapter::new(),
        ));
        registry.register(Arc::new(
            crate::flow_engine::node_adapters::OutputNodeAdapter::new(),
        ));
        let registry = Arc::new(registry);
        let flow_json = r#"{
            "nodes":[
                {"id":"t1","type":"trigger","config":{}},
                {"id":"o1","type":"output","config":{}}
            ],
            "edges":[
                {"from":"t1","to":"o1","from_port":"text","to_port":"text"}
            ]
        }"#;
        let compiled = Arc::new(
            crate::flow_engine::cache::CompiledFlow::from_json("0", flow_json, &registry)
                .expect("compile"),
        );
        crate::flow_engine::executor::execute_blocking(
            main_db.clone(),
            compiled,
            FlowEnvelope::empty(),
            ctx,
            registry,
        )
        .await
        .expect("flow runs");

        // The timeline writer is a task and the audit copy crosses databases
        // through the outbox, so both are polled rather than slept on.
        let mut audit_correlation = None;
        for _ in 0..300 {
            crate::events::audit_outbox::deliver_pending(&main_db, &events_pool, 16)
                .expect("audit delivery");
            let conn = main_db.read().expect("db lock");
            audit_correlation = conn
                .query_row(
                    "SELECT correlation_id FROM audit_log \
                     WHERE resource_type = 'flow_run' AND resource_id = ?1",
                    rusqlite::params![run_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()
                .expect("read audit_log");
            drop(conn);
            if audit_correlation.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let audit_correlation =
            audit_correlation.expect("the run's audit entry reached the main database");

        let run_row = {
            let conn = main_db.read().expect("db lock");
            conn.query_row(
                "SELECT correlation_id FROM flow_executions WHERE request_id = ?1",
                rusqlite::params![run_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .expect("the run wrote a flow_executions row")
        };

        assert!(
            run_row.is_some(),
            "the run row must carry the correlation id, not NULL"
        );
        assert_eq!(
            run_row, audit_correlation,
            "the audit entry and the run row of ONE execution must carry the \
             same correlation id — otherwise the audit entry links to nothing"
        );

        // `start` publishes a PROCESS-WIDE log that outlives this test, and the
        // events database goes away with `events_dir` at the end of it. Stopping
        // the subscribers puts the process back where it was: `attach_scope`
        // answers `false`, `begin_run` hands its binding straight back, and no
        // later test's run writes into a directory that no longer exists.
        crate::events::progress_log::stop();
        drop(events_dir);
    }

    /// The copy site in `make_context` — all five provenance fields reach the
    /// context the adapters see.
    #[test]
    fn make_context_propagates_provenance_from_meta() {
        let factory = stub_context_factory();
        let mut meta = FlowRequestMeta::new(
            "req-prov",
            FlowOrigin::Api,
            FlowActor::api_key("key-42", Some("user-7".to_string())),
        );
        meta.correlation_id = Some("corr-9".into());
        let ctx = factory.make_context(&meta, RunDescriptor::default());
        assert_eq!(ctx.origin, FlowOrigin::Api);
        assert_eq!(ctx.actor_kind, ActorKind::ApiKey);
        assert_eq!(ctx.actor_id.as_deref(), Some("key-42"));
        assert_eq!(ctx.actor_user_id.as_deref(), Some("user-7"));
        assert_eq!(ctx.correlation_id.as_deref(), Some("corr-9"));
        // A service key stays a service key: `actor_user_id` is the binding the
        // server resolved while verifying the key, and nothing downstream may
        // invent one. Sub-flow inheritance of these values is covered by
        // `subflow_runner::tests` against the real `SubflowRunner::prepare`.
        assert_eq!(
            ctx.actor(),
            FlowActor::api_key("key-42", Some("user-7".to_string()))
        );
    }

    /// The slugs are a wire contract: the event log stores them and the UI
    /// filters on them, so a rename silently breaks stored history.
    ///
    /// Driven by [`all_flow_origins`] / [`all_actor_kinds`] and asserted through
    /// exhaustive, wildcard-free matches, so a variant cannot reach the wire
    /// without its spelling being written down HERE, where changing it is
    /// visibly a wire change. The list this test used to carry silently omitted
    /// `Dashboard` and `Meeting`.
    #[test]
    fn provenance_slugs_are_stable_wire_spellings() {
        for origin in all_flow_origins() {
            let slug = match origin {
                FlowOrigin::Chat => "chat",
                FlowOrigin::Dashboard => "dashboard",
                FlowOrigin::Project => "project",
                FlowOrigin::CodeStudio => "code_studio",
                FlowOrigin::Api => "api",
                FlowOrigin::Addon => "addon",
                FlowOrigin::Camera => "camera",
                FlowOrigin::Meeting => "meeting",
                FlowOrigin::Scheduler => "scheduler",
                FlowOrigin::Mesh => "mesh",
                FlowOrigin::Agent => "agent",
                FlowOrigin::System => "system",
                FlowOrigin::Bus => "bus",
            };
            assert_eq!(origin.as_str(), slug);
        }
        for kind in all_actor_kinds() {
            let slug = match kind {
                ActorKind::User => "user",
                ActorKind::ApiKey => "api_key",
                ActorKind::Addon => "addon",
                ActorKind::System => "system",
            };
            assert_eq!(kind.as_str(), slug);
        }
    }

    #[test]
    fn registry_includes_all_node_types() {
        let r = build_registry(
            Arc::new(parking_lot::RwLock::new(None)),
            Arc::new(parking_lot::RwLock::new(None)),
        );
        let types: std::collections::BTreeSet<&str> = r.registered_types().into_iter().collect();
        for expected in [
            "trigger",
            "output",
            "condition",
            "pii_filter",
            "tts_clean",
            "stt",
            "tts",
            "embeddings",
            "vector",
            "memory",
            "conversation_history",
            "persist_turn",
            "session_context",
            "speaker_context",
            "llm",
            "sentence_buffer",
            "agent_context",
            "tool_exec",
            "agent_router",
            "compact_context",
            "subflow",
            "loop",
            "map",
            "agent",
            "ask_user",
            "spawn",
            "await_subagents",
            "subagent_status",
            "interval",
            "critic_gate",
            "task_gate",
            // Registered without the `graph` feature as well — the seeded RAG
            // ingest flow names it, so its adapter must resolve in every build.
            "graph_extract",
            // Code Studio (§16.4).
            "workspace_context",
            "patch_review",
            "exec_command",
            "delegate_cli",
        ] {
            assert!(types.contains(expected), "missing adapter '{expected}'");
        }
        assert!(r.llm().is_some(), "LLM typed accessor must be wired");
        // Streaming-aware adaptery dostępne też w streaming slot rejestru.
        for streaming in ["pii_filter", "tts", "tts_clean", "sentence_buffer"] {
            assert!(
                r.streaming_adapter(streaming).is_some(),
                "{streaming} must be registered in streaming slot"
            );
        }
    }

    #[test]
    fn wrap_blocking_as_stream_emits_text_payload() {
        use crate::flow_engine::envelope::{FinishReason, FlowEnvelope, FlowValue, TokenUsage};
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("hello world".into());
        let outcome = FlowExecutionOutcome {
            final_envelope: env,
            trace: Vec::new(),
            usage: TokenUsage {
                prompt_tokens: 5,
                completion_tokens: 7,
                total_tokens: 12,
            },
            model: None,
            perf: None,
            finish_reason: FinishReason::Stop,
            total_latency_ms: 42,
            error: None,
        };
        let blobs: Arc<dyn BlobStore> =
            Arc::new(crate::flow_engine::blob_store::InMemoryBlobStore::new());
        let exec = wrap_blocking_as_stream(outcome, blobs);
        let collected: Vec<EnvelopeDelta> = futures::executor::block_on(async {
            use futures::StreamExt;
            exec.stream
                .filter_map(|r| async move { r.ok() })
                .collect()
                .await
        });
        assert_eq!(collected.len(), 1);
        let EnvelopeDelta::Llm(chunk) = &collected[0] else {
            panic!("expected Llm variant");
        };
        assert_eq!(chunk.text_delta, "hello world");
        assert_eq!(chunk.finish_reason, Some(FinishReason::Stop));
        assert_eq!(chunk.usage.as_ref().unwrap().total_tokens, 12);
    }

    #[test]
    fn wrap_blocking_as_stream_serializes_non_text_payload_as_json() {
        use crate::flow_engine::envelope::{FinishReason, FlowEnvelope, FlowValue, TokenUsage};
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Embedding(vec![0.5, 0.25]);
        let outcome = FlowExecutionOutcome {
            final_envelope: env,
            trace: Vec::new(),
            usage: TokenUsage::default(),
            model: None,
            perf: None,
            finish_reason: FinishReason::Stop,
            total_latency_ms: 0,
            error: None,
        };
        let blobs: Arc<dyn BlobStore> =
            Arc::new(crate::flow_engine::blob_store::InMemoryBlobStore::new());
        let exec = wrap_blocking_as_stream(outcome, blobs);
        let collected: Vec<EnvelopeDelta> = futures::executor::block_on(async {
            use futures::StreamExt;
            exec.stream
                .filter_map(|r| async move { r.ok() })
                .collect()
                .await
        });
        assert_eq!(collected.len(), 1);
        let EnvelopeDelta::Llm(chunk) = &collected[0] else {
            panic!("expected Llm variant");
        };
        // Parytet z flow_outcome_to_chat_response — Embedding leci jako JSON
        assert!(
            chunk.text_delta.contains("0.5"),
            "expected JSON serialization, got: {}",
            chunk.text_delta
        );
    }

    /// Krok 5: blocking flow który zwraca FlowValue::Audio (np. synthetic
    /// TTS) musi wyjść jako EnvelopeDelta::Audio z prawdziwymi bajtami,
    /// nie jako JSON-z-blob_ref. Wrapper fetchuje BlobStore przed emitem.
    #[test]
    fn wrap_blocking_as_stream_fetches_audio_blob() {
        use crate::flow_engine::blob_store::InMemoryBlobStore;
        use crate::flow_engine::envelope::{FinishReason, FlowEnvelope, FlowValue, TokenUsage};

        let blobs = Arc::new(InMemoryBlobStore::new());
        let bytes = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let blob_ref =
            futures::executor::block_on(blobs.put(bytes.clone(), "audio/wav")).expect("put");

        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Audio {
            blob_ref,
            mime: "audio/wav".into(),
            sample_rate: Some(22_050),
        };
        let outcome = FlowExecutionOutcome {
            final_envelope: env,
            trace: Vec::new(),
            usage: TokenUsage::default(),
            model: None,
            perf: None,
            finish_reason: FinishReason::Stop,
            total_latency_ms: 0,
            error: None,
        };
        let blobs_dyn: Arc<dyn BlobStore> = blobs;
        let exec = wrap_blocking_as_stream(outcome, blobs_dyn);
        let collected: Vec<EnvelopeDelta> = futures::executor::block_on(async {
            use futures::StreamExt;
            exec.stream
                .filter_map(|r| async move { r.ok() })
                .collect()
                .await
        });
        assert_eq!(collected.len(), 1);
        let EnvelopeDelta::Audio(chunk) = &collected[0] else {
            panic!("expected Audio variant, got {:?}", collected[0].kind());
        };
        assert_eq!(chunk.bytes_delta, bytes);
        assert_eq!(chunk.mime, "audio/wav");
        assert_eq!(chunk.sample_rate, Some(22_050));
        assert_eq!(chunk.finish_reason, Some(FinishReason::Stop));
    }
}
