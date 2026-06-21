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
    AuditSink, Clock, ConversationHistoryStore, EmbeddingsDispatcher, LlmDispatcher, MemoryStore,
    MetricsSink, NoopMetrics, NoopProgress, PiiRulesStore, ProgressSink, PromptStore,
    RerankDispatcher, SttDispatcher, TtsCleaningStore, TtsDispatcher, VisionDispatcher,
};
use crate::flow_engine::dispatchers_impl::{
    AuditSinkImpl, ConversationHistoryImpl, EmbeddingsDispatcherImpl, LlmDispatcherImpl,
    MemoryStoreImpl, ModelRuntimeSlot, PiiRulesStoreImpl, PromptsImpl, RerankDispatcherImpl,
    ServiceManagerQuicFinder, SttDispatcherImpl, TtsCleaningStoreImpl, TtsDispatcherImpl,
    VisionDispatcherImpl,
};
use crate::flow_engine::envelope::{
    AudioStreamChunk, EnvelopeDelta, FlowEnvelope, FlowExecutionOutcome, FlowValue, LlmStreamChunk,
};
use crate::flow_engine::executor::{execute_blocking, execute_streaming, StreamingExecution};
use crate::flow_engine::node_adapter::{AdapterRegistry, ExecutionContext, NodeAdapter, UsageSink};
use crate::flow_engine::node_adapters::{
    AgentContextNodeAdapter, AgentNodeAdapter, AgentRouterNodeAdapter, AskUserNodeAdapter,
    AwaitSubagentsNodeAdapter, CameraAlertNodeAdapter, CameraVerdictNodeAdapter,
    CombineNodeAdapter, CompactContextNodeAdapter, ConditionNodeAdapter,
    ConversationHistoryNodeAdapter, EmbeddingsNodeAdapter, IntervalNodeAdapter, LlmNodeAdapter,
    LoopNodeAdapter, MapNodeAdapter, MemoryNodeAdapter, OnSubagentCompleteNodeAdapter,
    OutputNodeAdapter, PersistTurnNodeAdapter,
    PiiFilterNodeAdapter, RerankerNodeAdapter, SessionContextNodeAdapter, SpawnNodeAdapter,
    SpeakerContextNodeAdapter,
    SttNodeAdapter, SubagentStatusNodeAdapter, SubflowNodeAdapter, ToolExecNodeAdapter,
    TriggerNodeAdapter, TtsCleanNodeAdapter, TtsNodeAdapter, VisionClassifyNodeAdapter,
    VisionNodeAdapter, VisionOcrNodeAdapter,
};
use crate::flow_engine::resolver;
use crate::flow_engine::subflow_runner::{SubflowRunner, SubflowRunnerSlot};
use crate::flow_engine::synthetic;
use crate::services::runtime::quic_handle::ServiceManager;

const FLOW_TIMEOUT_SECS: u64 = 120;

/// Stage 3d-0b-final: typed dispatch error żeby routing layer mógł
/// mapować na precyzyjne HTTP status codes:
/// - `Denied` → 404 model_not_found (plan v1.5: nie ujawniamy istnienia
///   modelu klientom bez ACL).
/// - `CompileFailed` → 500 z msg ("user-defined flow nie kompiluje się").
/// - `Unsupported` → 500 z msg ("synthetic builder nie wspiera service_type").
/// - `Internal` → 500 (runtime err / timeout / inne).
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("flow {flow_id} ACL denied for user")]
    Denied { flow_id: String },
    #[error("flow {flow_id} compile failed: {msg}")]
    CompileFailed { flow_id: String, msg: String },
    #[error("synthetic dispatch unsupported for service_type='{service_type}', model='{model}'")]
    Unsupported { service_type: String, model: String },
    #[error("flow dispatch internal: {0}")]
    Internal(String),
}

impl From<anyhow::Error> for DispatchError {
    fn from(e: anyhow::Error) -> Self {
        DispatchError::Internal(e.to_string())
    }
}

/// Wynik resolve_cached — rozróżnia 3 stany żeby caller wiedział czy aktywować
/// synthetic fallback (NotFound) czy zwrócić błąd kompilacji (CompileFailed).
enum ResolvedFlow {
    Found(Arc<CachedFlow>),
    /// Resolver nie znalazł user-defined flow dla danego (model, kind, modality).
    /// Caller buduje synthetic ad-hoc flow (Universal Flow Gateway).
    NotFound,
    /// User-defined flow istnieje ale compile failed. Cache'owane jako None
    /// żeby nie próbować ponownie do invalidate. Synthetic NIE aktywuje się
    /// (admin chciał konkretny flow — niech go naprawi).
    CompileFailed,
}

/// Per-request metadata przekazywane przez callera. FlowDispatcher buduje z
/// tego `ExecutionContext` (klonując Arc'i dispatcherów + clock + blobs).
#[derive(Clone)]
pub struct FlowRequestMeta {
    pub request_id: String,
    pub session_id: Option<String>,
    pub user_id: Option<String>,
    pub user_role: Option<String>,
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
    pub fn new(request_id: impl Into<String>) -> Self {
        Self {
            request_id: request_id.into(),
            session_id: None,
            user_id: None,
            user_role: None,
            deadline: None,
            cancel_token: CancellationToken::new(),
            progress_sink: None,
            flow_depth: 0,
        }
    }

    /// Effective broadcast scope for progress events — session id when present,
    /// else the request id.
    fn progress_scope(&self) -> String {
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
    llm: Arc<dyn LlmDispatcher>,
    embeddings: Arc<dyn EmbeddingsDispatcher>,
    reranker: Arc<dyn RerankDispatcher>,
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
    fn make_context(&self, meta: &FlowRequestMeta) -> ExecutionContext {
        ExecutionContext {
            request_id: meta.request_id.clone(),
            execution_id: 0,
            parent_execution_id: None,
            light: false,
            session_id: meta.session_id.clone(),
            user_id: meta.user_id.clone(),
            user_role: meta.user_role.clone(),
            deadline: meta.deadline,
            deadline_extension_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            cancel_token: meta.cancel_token.clone(),
            subflow_depth: meta.flow_depth,
            subflow_visited: Arc::new(Vec::new()),
            initial_envelope: Arc::new(FlowEnvelope::empty()),
            clock: self.clock.clone(),
            blobs: self.blobs.clone(),
            llm: self.llm.clone(),
            embeddings: self.embeddings.clone(),
            reranker: self.reranker.clone(),
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
        let ctx_blobs: Arc<dyn BlobStore> = Arc::new(
            crate::flow_engine::blob_store::CompositeBlobStore::new(
                frame_blobs.clone(),
                blobs.clone(),
            ),
        );

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
        let tts: Arc<dyn TtsDispatcher> =
            Arc::new(TtsDispatcherImpl::new(runtime_slot.clone(), ctx_blobs.clone()));
        let stt: Arc<dyn SttDispatcher> =
            Arc::new(SttDispatcherImpl::new(runtime_slot, ctx_blobs.clone()));
        let vision: Arc<dyn VisionDispatcher> = Arc::new(VisionDispatcherImpl::new());

        let ctx_factory = Arc::new(ContextFactory {
            clock,
            blobs: ctx_blobs,
            llm,
            embeddings,
            reranker,
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
                self.run_blocking(cached.compiled.clone(), initial, meta)
                    .await
                    .map_err(DispatchError::from)
            }
            ResolvedFlow::NotFound => {
                // Universal Flow Gateway — synthetic ad-hoc fallback.
                let compiled = self.compile_synthetic_blocking(service_type, model_name)?;
                self.run_blocking(compiled, initial, meta)
                    .await
                    .map_err(DispatchError::from)
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
        self.run_blocking(cached.compiled.clone(), initial, meta)
            .await
            .map_err(DispatchError::from)
    }

    /// Background-run variant (Harness §3.6/§3.7): runs the agent harness flow
    /// for an `AgentRunManager` task. Unlike `dispatch_by_flow_id` it does NOT
    /// impose the 120 s `FLOW_TIMEOUT_SECS` cap — a long agent run is governed
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
        let ctx = self.ctx_factory.make_context(&meta);
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
                .run_blocking(compiled, initial, meta)
                .await
                .map_err(DispatchError::from)?;
            return Ok(wrap_blocking_as_stream(outcome, self.blobs()));
        }
        let ctx = self.ctx_factory.make_context(&meta);
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

    /// Streaming wariant z WYMUSZONYM synthetic flow — pomija resolver
    /// (binding modelu / default flow z DB). Używany przez UI czatu gdy user
    /// wybrał opcję "Default Chat" (syntetyczny trigger→llm→pii_filter→output).
    pub async fn dispatch_synthetic_streaming(
        &self,
        model_name: &str,
        service_type: &str,
        initial: FlowEnvelope,
        meta: FlowRequestMeta,
    ) -> std::result::Result<StreamingExecution, DispatchError> {
        let compiled = self.compile_synthetic_streaming(service_type, model_name)?;
        let ctx = self.ctx_factory.make_context(&meta);
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
                        .run_blocking(cached.compiled.clone(), initial, meta)
                        .await
                        .map_err(DispatchError::from)?;
                    return Ok(wrap_blocking_as_stream(outcome, self.blobs()));
                }
                cached.compiled.clone()
            }
            ResolvedFlow::NotFound => self.compile_synthetic_streaming(service_type, model_name)?,
            ResolvedFlow::CompileFailed => {
                return Err(DispatchError::CompileFailed {
                    flow_id: String::new(),
                    msg: format!("user-defined streaming flow for '{model_name}/{service_type}'"),
                });
            }
        };
        let ctx = self.ctx_factory.make_context(&meta);
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
    ) -> Result<FlowExecutionOutcome> {
        let ctx = self.ctx_factory.make_context(&meta);
        let flow_id = compiled.flow_id.clone();
        match timeout(
            Duration::from_secs(FLOW_TIMEOUT_SECS),
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
                warn!(flow_id, "Timeout flow po {FLOW_TIMEOUT_SECS}s");
                Err(anyhow::anyhow!(
                    "flow {flow_id} timeout after {FLOW_TIMEOUT_SECS}s"
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

    /// Buduje (lub pobiera z synthetic slot cache'a) compiled synthetic blocking
    /// flow dla pary (service_type, model). Zwraca None gdy service_type nie jest
    /// wspierany (np. niestandardowa wartość jak "image" — Universal Gateway w v1
    /// pokrywa chat/tts/stt/embeddings).
    fn compile_synthetic_blocking(
        &self,
        service_type: &str,
        model: &str,
    ) -> std::result::Result<Arc<CompiledFlow>, DispatchError> {
        self.compile_synthetic_inner(service_type, model, false)
    }

    fn compile_synthetic_streaming(
        &self,
        service_type: &str,
        model: &str,
    ) -> std::result::Result<Arc<CompiledFlow>, DispatchError> {
        self.compile_synthetic_inner(service_type, model, true)
    }

    /// Stage 3d-0b-final P2#2: rozdziela `Unsupported` (service_type bez
    /// synthetic buildera) od `CompileFailed` (synthetic def istnieje ale
    /// kompilacja flow nie przechodzi). Caller dostaje dokładną przyczynę
    /// w error type.
    fn compile_synthetic_inner(
        &self,
        service_type: &str,
        model: &str,
        streaming: bool,
    ) -> std::result::Result<Arc<CompiledFlow>, DispatchError> {
        let kind = match (service_type, streaming) {
            ("chat", false) => "chat",
            ("chat", true) => "chat_stream",
            ("tts", _) => "tts",
            ("stt", _) => "stt",
            ("embeddings", _) => "embeddings",
            _ => {
                return Err(DispatchError::Unsupported {
                    service_type: service_type.to_string(),
                    model: model.to_string(),
                });
            }
        };
        let synth_key = format!("{}:{}", kind, model);
        if let Some(hit) = self.cache.synthetic_get(&synth_key) {
            return Ok(hit);
        }
        let definition = match (service_type, streaming) {
            ("chat", false) => synthetic::synthetic_chat(model),
            ("chat", true) => synthetic::synthetic_chat_stream(model),
            ("tts", _) => synthetic::synthetic_tts(model),
            ("stt", _) => synthetic::synthetic_stt(model),
            ("embeddings", _) => synthetic::synthetic_embeddings(model),
            _ => unreachable!("kind matched powyżej"),
        };
        let compiled = match CompiledFlow::compile("", definition, &self.registry) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                warn!(kind, model, "synthetic compile failed: {e}");
                return Err(DispatchError::CompileFailed {
                    flow_id: String::new(),
                    msg: format!("synthetic '{kind}' compile: {e}"),
                });
            }
        };
        self.cache.synthetic_set(&synth_key, compiled.clone());
        Ok(compiled)
    }
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
                    finish_reason: Some(finish),
                    error: err,
                }))
            }
        }
    })
    .boxed();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = tx.send(outcome);
    StreamingExecution {
        stream,
        outcome: rx,
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
        Arc::new(OutputNodeAdapter::new()),
        Arc::new(ConditionNodeAdapter::new()),
        Arc::new(CombineNodeAdapter::new()),
        Arc::new(SttNodeAdapter::new()),
        Arc::new(EmbeddingsNodeAdapter::new()),
        Arc::new(RerankerNodeAdapter::new()),
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
    ];
    for a in arcs {
        r.register(a);
    }
    // Harness §3.5: agent_context + tool_exec + agent_router + compact_context
    // share the late-bound AgentServiceSlot (filled by main.rs after the
    // AddonManager exists). agent_router/compact_context also issue audited LLM
    // calls via ctx.llm; they need only the registry/service.
    r.register(Arc::new(AgentContextNodeAdapter::new(agent_service.clone())));
    r.register(Arc::new(ToolExecNodeAdapter::new(agent_service.clone())));
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
    r.register_stream_producer(Arc::new(AgentNodeAdapter::new(agent_service, subflow_runner)));
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

#[cfg(test)]
mod tests {
    use super::*;

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
