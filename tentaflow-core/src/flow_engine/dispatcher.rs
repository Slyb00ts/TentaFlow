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
    SttDispatcherImpl, SttRuntimeSlot, TtsCleaningStoreImpl, TtsDispatcherImpl,
};
use crate::flow_engine::envelope::{FlowEnvelope, FlowExecutionOutcome};
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
use crate::services::runtime::quic_handle::ServiceManager;

const FLOW_TIMEOUT_SECS: u64 = 120;

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
        stt_runtime_slot: SttRuntimeSlot,
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
            Arc::new(TtsDispatcherImpl::new(runtime_slot, blobs.clone()));
        let stt: Arc<dyn SttDispatcher> =
            Arc::new(SttDispatcherImpl::new(stt_runtime_slot, blobs.clone()));

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
    ) -> Result<Option<FlowExecutionOutcome>> {
        let modality = derive_modality(&initial);
        let cache_key = format!("{}:{}:{}", model_name, service_type, modality);
        let cached = match self
            .resolve_cached(&cache_key, model_name, service_type, modality)
            .await?
        {
            Some(c) => c,
            None => return Ok(None),
        };
        if !self.acl_allow(cached.flow.id, &meta) {
            return Ok(None);
        }
        self.run_blocking(cached.compiled.clone(), initial, meta).await
    }

    pub async fn dispatch_by_flow_id(
        &self,
        flow_id: i64,
        initial: FlowEnvelope,
        meta: FlowRequestMeta,
    ) -> Result<Option<FlowExecutionOutcome>> {
        let pool = self.db.clone();
        let flow_opt = tokio::task::spawn_blocking(move || repository::get_flow(&pool, flow_id))
            .await??;
        let Some(flow) = flow_opt else {
            return Ok(None);
        };
        if flow.status != "active" {
            warn!(flow_id, status = %flow.status, "flow nieaktywny — pomijam");
            return Ok(None);
        }
        if !self.acl_allow(flow_id, &meta) {
            return Ok(None);
        }
        let compiled = match CompiledFlow::from_json(flow.id, &flow.flow_json, &self.registry, crate::flow_engine::validation::ValidationSource::UserDefined) {
            Ok(c) => Arc::new(c),
            Err(e) => {
                warn!(flow_id, "compile failed: {e}");
                return Ok(None);
            }
        };
        self.run_blocking(compiled, initial, meta).await
    }

    pub async fn try_dispatch_streaming(
        &self,
        model_name: &str,
        service_type: &str,
        initial: FlowEnvelope,
        meta: FlowRequestMeta,
    ) -> Result<Option<StreamingExecution>> {
        let modality = derive_modality(&initial);
        let cache_key = format!("{}:{}:{}", model_name, service_type, modality);
        let cached = match self
            .resolve_cached(&cache_key, model_name, service_type, modality)
            .await?
        {
            Some(c) => c,
            None => return Ok(None),
        };
        if !cached.compiled.is_streaming {
            return Ok(None);
        }
        if !self.acl_allow(cached.flow.id, &meta) {
            return Ok(None);
        }
        let ctx = self.ctx_factory.make_context(&meta);
        let stream_exec = execute_streaming(
            self.db.clone(),
            cached.compiled.clone(),
            initial,
            ctx,
            self.registry.clone(),
        )
        .await?;
        Ok(Some(stream_exec))
    }

    async fn run_blocking(
        &self,
        compiled: Arc<CompiledFlow>,
        initial: FlowEnvelope,
        meta: FlowRequestMeta,
    ) -> Result<Option<FlowExecutionOutcome>> {
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
            Ok(Ok(outcome)) => Ok(Some(outcome)),
            Ok(Err(e)) => {
                warn!(flow_id, "Blad wykonania flow: {e}");
                Ok(None)
            }
            Err(_) => {
                warn!(flow_id, "Timeout flow po {FLOW_TIMEOUT_SECS}s");
                Ok(None)
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
    ) -> Result<Option<Arc<CachedFlow>>> {
        if let Some(slot) = self.cache.get(cache_key) {
            return Ok(slot);
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
                        self.cache.set(cache_key, None);
                        return Ok(None);
                    }
                };
                let cached = Arc::new(CachedFlow { flow, compiled });
                self.cache.set(cache_key, Some(cached.clone()));
                Ok(Some(cached))
            }
            None => {
                self.cache.set(cache_key, None);
                Ok(None)
            }
        }
    }
}

/// Etap 3b: derive request modality z initial envelope payload — vision
/// flows MUSZĄ być explicit bound, default flow działa tylko dla text.
/// `Image` payload → "image", reszta (Text/Empty/Json/...) → "text".
fn derive_modality(envelope: &FlowEnvelope) -> &'static str {
    use crate::flow_engine::envelope::FlowValue;
    match envelope.payload {
        FlowValue::Image { .. } => "image",
        _ => "text",
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
}
