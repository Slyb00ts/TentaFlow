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
use crate::flow_engine::dispatchers::{
    AuditSink, Clock, ConversationHistoryStore, EmbeddingsDispatcher, LlmDispatcher, MemoryStore,
    MetricsSink, NoopMetrics, PiiRulesStore, PromptStore, SttDispatcher, TtsCleaningStore,
    TtsDispatcher,
};
use crate::flow_engine::dispatchers::clock::SystemClock;
use crate::flow_engine::dispatchers_impl::{
    AuditSinkImpl, ConversationHistoryImpl, EmbeddingsDispatcherImpl, LlmDispatcherImpl,
    MemoryStoreImpl, ModelRuntimeSlot, PiiRulesStoreImpl, PromptsImpl, ServiceManagerQuicFinder,
    SttDispatcherImpl, TtsCleaningStoreImpl, TtsDispatcherImpl,
};
use crate::flow_engine::envelope::{
    EnvelopeDelta, FlowEnvelope, FlowExecutionOutcome, FlowValue, LlmStreamChunk,
};
use crate::flow_engine::executor::{execute_blocking, execute_streaming, StreamingExecution};
use crate::flow_engine::node_adapter::{
    AdapterRegistry, ExecutionContext, NodeAdapter, UsageSink,
};
use crate::flow_engine::node_adapters::{
    ConditionNodeAdapter, ConversationHistoryNodeAdapter, EmbeddingsNodeAdapter, LlmNodeAdapter,
    MemoryNodeAdapter, OutputNodeAdapter, PiiFilterNodeAdapter, SessionContextNodeAdapter,
    SpeakerContextNodeAdapter, SttNodeAdapter, TriggerNodeAdapter, TtsCleanNodeAdapter,
    TtsNodeAdapter, VisionNodeAdapter,
};
use crate::flow_engine::resolver;
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
    Denied { flow_id: i64 },
    #[error("flow {flow_id} compile failed: {msg}")]
    CompileFailed { flow_id: i64, msg: String },
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
#[derive(Debug, Clone)]
pub struct FlowRequestMeta {
    pub request_id: String,
    pub session_id: Option<String>,
    pub user_id: Option<i64>,
    pub user_role: Option<String>,
    pub deadline: Option<Instant>,
    pub cancel_token: CancellationToken,
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
        }
    }
}

pub struct FlowDispatcher {
    db: DbPool,
    cache: FlowCache,
    registry: Arc<AdapterRegistry>,
    ctx_factory: Arc<ContextFactory>,
}

/// Pre-zbudowane Arc'i wszystkich capability dispatcherów + clock + blobs.
/// `make_context` klonuje je do nowego `ExecutionContext` per request.
struct ContextFactory {
    clock: Arc<dyn Clock>,
    blobs: Arc<dyn BlobStore>,
    llm: Arc<dyn LlmDispatcher>,
    embeddings: Arc<dyn EmbeddingsDispatcher>,
    stt: Arc<dyn SttDispatcher>,
    tts: Arc<dyn TtsDispatcher>,
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
            session_id: meta.session_id.clone(),
            user_id: meta.user_id,
            user_role: meta.user_role.clone(),
            deadline: meta.deadline,
            cancel_token: meta.cancel_token.clone(),
            initial_envelope: Arc::new(FlowEnvelope::empty()),
            clock: self.clock.clone(),
            blobs: self.blobs.clone(),
            llm: self.llm.clone(),
            embeddings: self.embeddings.clone(),
            stt: self.stt.clone(),
            tts: self.tts.clone(),
            prompts: self.prompts.clone(),
            memory: self.memory.clone(),
            history: self.history.clone(),
            audit: self.audit.clone(),
            metrics: self.metrics.clone(),
            pii_rules: self.pii_rules.clone(),
            tts_cleaning: self.tts_cleaning.clone(),
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
        let history: Arc<dyn ConversationHistoryStore> = Arc::new(
            ConversationHistoryImpl::new(service_manager.conversation_cache.clone()),
        );
        let quic_finder = Arc::new(ServiceManagerQuicFinder::new(service_manager.clone()));
        let memory: Arc<dyn MemoryStore> = Arc::new(MemoryStoreImpl::new(quic_finder));

        let llm: Arc<dyn LlmDispatcher> =
            Arc::new(LlmDispatcherImpl::new(runtime_slot.clone(), blobs.clone()));
        let embeddings: Arc<dyn EmbeddingsDispatcher> =
            Arc::new(EmbeddingsDispatcherImpl::new(runtime_slot.clone()));
        let tts: Arc<dyn TtsDispatcher> =
            Arc::new(TtsDispatcherImpl::new(runtime_slot.clone(), blobs.clone()));
        let stt: Arc<dyn SttDispatcher> =
            Arc::new(SttDispatcherImpl::new(runtime_slot, blobs.clone()));

        let ctx_factory = Arc::new(ContextFactory {
            clock,
            blobs,
            llm,
            embeddings,
            stt,
            tts,
            prompts,
            memory,
            history,
            audit,
            metrics,
            pii_rules,
            tts_cleaning,
        });

        let registry = build_registry();
        Self {
            db,
            cache: FlowCache::new(60),
            registry: Arc::new(registry),
            ctx_factory,
        }
    }

    pub fn registry(&self) -> &Arc<AdapterRegistry> {
        &self.registry
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
                if !self.acl_allow(cached.flow.id, &meta) {
                    return Err(DispatchError::Denied {
                        flow_id: cached.flow.id,
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
                flow_id: 0,
                msg: format!("user-defined flow for '{model_name}/{service_type}'"),
            }),
        }
    }

    pub async fn dispatch_by_flow_id(
        &self,
        flow_id: i64,
        initial: FlowEnvelope,
        meta: FlowRequestMeta,
    ) -> std::result::Result<FlowExecutionOutcome, DispatchError> {
        let pool = self.db.clone();
        let flow_opt = tokio::task::spawn_blocking(move || repository::get_flow(&pool, flow_id))
            .await
            .map_err(|e| DispatchError::Internal(e.to_string()))?
            .map_err(|e| DispatchError::Internal(e.to_string()))?;
        let flow = flow_opt.ok_or_else(|| DispatchError::CompileFailed {
            flow_id,
            msg: "flow id nie istnieje w DB".to_string(),
        })?;
        if flow.status != "active" {
            warn!(flow_id, status = %flow.status, "flow nieaktywny — pomijam");
            return Err(DispatchError::CompileFailed {
                flow_id,
                msg: format!("flow status='{}' (nie active)", flow.status),
            });
        }
        if !self.acl_allow(flow_id, &meta) {
            return Err(DispatchError::Denied { flow_id });
        }
        let compiled = match CompiledFlow::from_json(flow.id, &flow.flow_json, &self.registry, crate::flow_engine::validation::ValidationSource::UserDefined) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                warn!(flow_id, "compile failed: {e}");
                return Err(DispatchError::CompileFailed {
                    flow_id,
                    msg: e.to_string(),
                });
            }
        };
        self.run_blocking(compiled, initial, meta)
            .await
            .map_err(DispatchError::from)
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
                if !self.acl_allow(cached.flow.id, &meta) {
                    return Err(DispatchError::Denied {
                        flow_id: cached.flow.id,
                    });
                }
                if !cached.compiled.is_streaming {
                    // User-defined blocking-only flow — wykonaj blocking
                    // i opakuj outcome jako single-chunk stream.
                    let outcome = self
                        .run_blocking(cached.compiled.clone(), initial, meta)
                        .await
                        .map_err(DispatchError::from)?;
                    return Ok(wrap_blocking_as_stream(outcome));
                }
                cached.compiled.clone()
            }
            ResolvedFlow::NotFound => {
                self.compile_synthetic_streaming(service_type, model_name)?
            }
            ResolvedFlow::CompileFailed => {
                return Err(DispatchError::CompileFailed {
                    flow_id: 0,
                    msg: format!(
                        "user-defined streaming flow for '{model_name}/{service_type}'"
                    ),
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
        let flow_id = compiled.flow_id;
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

    fn acl_allow(&self, flow_id: i64, meta: &FlowRequestMeta) -> bool {
        let Some(uid) = meta.user_id else {
            return true;
        };
        let role = meta.user_role.clone().unwrap_or_else(|| "user".into());
        let allowed = acl::check_access_safe(&self.db, "flow", &flow_id.to_string(), uid, &role);
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
                let compiled = match CompiledFlow::from_json(flow.id, &flow.flow_json, &self.registry, crate::flow_engine::validation::ValidationSource::UserDefined) {
                    Ok(c) => Arc::new(c),
                    Err(e) => {
                        warn!(
                            cache_key,
                            "compile failed for flow id={}: {e}", flow.id
                        );
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
        let compiled = match CompiledFlow::compile(
            0,
            definition,
            &self.registry,
            crate::flow_engine::validation::ValidationSource::Synthetic,
        ) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                warn!(kind, model, "synthetic compile failed: {e}");
                return Err(DispatchError::CompileFailed {
                    flow_id: 0,
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
fn wrap_blocking_as_stream(outcome: FlowExecutionOutcome) -> StreamingExecution {
    use futures::stream::StreamExt;
    // Parytet z flow_outcome_to_chat_response: Text → raw, Empty → "",
    // pozostałe (Image/Audio/Embedding/Json) → serde_json string. Inaczej
    // streaming-wrapped blocking-only flow gubiłby non-text payload.
    let text_delta = match &outcome.final_envelope.payload {
        FlowValue::Text(t) => t.clone(),
        FlowValue::Empty => String::new(),
        other => serde_json::to_string(&crate::flow_engine::converter::payload_to_json(other))
            .unwrap_or_default(),
    };
    let chunk = LlmStreamChunk {
        choice_index: 0,
        text_delta,
        reasoning_delta: None,
        tool_calls: Vec::new(),
        usage: Some(outcome.usage.clone()),
        finish_reason: Some(outcome.finish_reason.clone()),
        error: outcome.error.clone(),
    };
    let stream = futures::stream::once(async move { Ok(EnvelopeDelta::Llm(chunk)) }).boxed();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let _ = tx.send(outcome);
    StreamingExecution {
        stream,
        outcome: rx,
    }
}

/// Buduje AdapterRegistry z wszystkimi 13 adapterami stage 1c. Side effect-free
/// (adaptery są stateless / leniwie pobierają state z ExecutionContext).
fn build_registry() -> AdapterRegistry {
    let mut r = AdapterRegistry::new();
    let arcs: Vec<Arc<dyn NodeAdapter>> = vec![
        Arc::new(TriggerNodeAdapter::new()),
        Arc::new(OutputNodeAdapter::new()),
        Arc::new(ConditionNodeAdapter::new()),
        Arc::new(PiiFilterNodeAdapter::new()),
        Arc::new(TtsCleanNodeAdapter::new()),
        Arc::new(SttNodeAdapter::new()),
        Arc::new(TtsNodeAdapter::new()),
        Arc::new(EmbeddingsNodeAdapter::new()),
        Arc::new(MemoryNodeAdapter::new()),
        Arc::new(ConversationHistoryNodeAdapter::new()),
        Arc::new(SessionContextNodeAdapter::new()),
        Arc::new(SpeakerContextNodeAdapter::new()),
        Arc::new(VisionNodeAdapter::new()),
    ];
    for a in arcs {
        r.register(a);
    }
    r.register_llm(Arc::new(LlmNodeAdapter::new()));
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_includes_all_node_types() {
        let r = build_registry();
        let types: std::collections::BTreeSet<&str> =
            r.registered_types().into_iter().collect();
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
            "session_context",
            "speaker_context",
            "llm",
        ] {
            assert!(types.contains(expected), "missing adapter '{expected}'");
        }
        assert!(r.llm().is_some(), "LLM typed accessor must be wired");
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
        let exec = wrap_blocking_as_stream(outcome);
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
        let exec = wrap_blocking_as_stream(outcome);
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
}
