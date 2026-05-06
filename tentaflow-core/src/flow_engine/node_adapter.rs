// =============================================================================
// Plik: flow_engine/node_adapter.rs
// Opis: Nowy NodeAdapter trait + ExecutionContext + AdapterRegistry. Plan v4.1
//       hard rule 8 (single execute method, streaming on executor not adapter)
//       i v4.1 typed accessor pattern (registry.llm: Arc<LlmNodeAdapter> obok
//       generic mapy). Stage 1b: standalone — stary `flow_engine::adapters`
//       pozostaje nietknięty do czasu executor rewrite w stage 1c.
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

use super::dispatchers::{
    AuditSink, Clock, ConversationHistoryStore, EmbeddingsDispatcher, LlmDispatcher, MemoryStore,
    MetricsSink, PiiRulesStore, PromptStore, SttDispatcher, TtsCleaningStore, TtsDispatcher,
};
use super::envelope::{FlowEnvelope, NodeInput, TokenUsage};
use super::types::{FlowDataType, FlowNode};
use crate::flow_engine::blob_store::BlobStore;

/// Akumulator usage per-node — adaptery LLM/Embeddings pushują tu wynik,
/// executor zlicza po topo loopie do `FlowExecutionOutcome.usage` i mapuje do
/// `TraceStep.usage`.
#[derive(Default)]
pub struct UsageSink {
    inner: Mutex<Vec<(String, TokenUsage)>>,
}

impl UsageSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, node_id: impl Into<String>, usage: TokenUsage) {
        if let Ok(mut g) = self.inner.lock() {
            g.push((node_id.into(), usage));
        }
    }

    /// Zwraca per-node usage w kolejności wpisywania, zachowuje wewnętrzny
    /// stan (executor woła to per-node po execute żeby dorzucić do TraceStep).
    pub fn snapshot(&self) -> Vec<(String, TokenUsage)> {
        self.inner
            .lock()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Suma wszystkich token usage zarejestrowanych do tej pory.
    pub fn aggregate(&self) -> TokenUsage {
        let mut total = TokenUsage::default();
        if let Ok(g) = self.inner.lock() {
            for (_, u) in g.iter() {
                total.add(u);
            }
        }
        total
    }

    /// Zwraca i czyści usage zapisany od ostatniego pobrania. Używane przez
    /// executor po `execute()` node'a — usage przypisany do TraceStep tego
    /// node'a, mapa nie kumuluje globalnie.
    pub fn drain(&self) -> Vec<(String, TokenUsage)> {
        self.inner
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default()
    }
}

/// Pełny zestaw zależności dostępny adapterom podczas execute(). Wszystkie pola
/// to Arc<dyn Trait> z dispatchers/ — zero ServiceManager, zero god-objectu.
pub struct ExecutionContext {
    pub request_id: String,
    pub execution_id: i64,
    pub session_id: Option<String>,
    pub user_id: Option<i64>,
    pub user_role: Option<String>,
    pub deadline: Option<Instant>,
    pub cancel_token: CancellationToken,

    /// Seed envelope dostarczony przez routing (request_id, model, payload,
    /// initial messages). Plan v4.2 D2: używa go TYLKO trigger.execute().
    /// Inne adaptery czytają inputs[0]; streaming LLM czyta envelope po
    /// wszystkich pre-LLM nodach, NIE initial.
    pub initial_envelope: Arc<FlowEnvelope>,

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
    pub pii_rules: Arc<dyn PiiRulesStore>,
    pub tts_cleaning: Arc<dyn TtsCleaningStore>,

    pub usage_sink: Arc<UsageSink>,
}

#[async_trait]
pub trait NodeAdapter: Send + Sync {
    fn node_type(&self) -> &str;
    fn supported_input_ports(&self) -> &[&'static str];
    fn supported_output_ports(&self) -> &[&'static str];

    /// Pojedyncza metoda execute — zgodnie z hard rule 8 z planu v4.1.
    /// Streaming jest cechą flow (executor decyduje), nie adaptera. LLM
    /// adapter ma osobną concrete metodę `prepare_request` w impl.
    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope>;

    /// Etap 2: typ danych przyjmowanych na danym input port. Default `Any`
    /// (passthrough adaptery: trigger, output, condition, conversation_history,
    /// session_context, speaker_context). Walidacja R8 sprawdza zgodność z
    /// `edge.data_type` i z `output_port_type` producenta.
    fn input_port_type(&self, _port: &str) -> FlowDataType {
        FlowDataType::Any
    }

    /// Etap 2: typ danych emitowanych na danym output port. Default `Any`.
    fn output_port_type(&self, _port: &str) -> FlowDataType {
        FlowDataType::Any
    }

    /// Etap 2: ArtifactKey deklaracje — klucze które adapter MOŻE wyprodukować
    /// w `envelope.artifacts`. Etap 2 używa to tylko jako dokumentacji i hint
    /// dla GUI; walidacja R9 (consumer ↔ producent typu artefaktu) zostaje na
    /// Etap 3.
    fn produced_artifacts(&self) -> &[(&'static str, FlowDataType)] {
        &[]
    }

    /// Etap 2: ArtifactKey deklaracje — klucze które adapter CZYTA z
    /// `envelope.artifacts` (przez node config `read_artifact = "key"` albo
    /// dedykowany input port w przyszłości). Etap 2 — same dokumentacja.
    fn consumed_artifact_types(&self) -> &[(&'static str, FlowDataType)] {
        &[]
    }
}

/// Marker trait dla LLM adaptera — executor potrzebuje typed accessor żeby
/// wywołać `prepare_request` (concrete method spoza traita NodeAdapter).
/// Implementuje to konkretny `LlmNodeAdapter` w stage 1b dalej.
pub trait LlmAdapter: NodeAdapter {
    fn prepare_llm_request(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> super::dispatchers::LlmRequest;
}

/// Registry z typed accessorem dla LLM (plan v4.1 — bez downcastu). Dodatkowe
/// typed pola dochodzą jeśli executor będzie potrzebował kolejnych concrete
/// metod (dziś tylko llm).
pub struct AdapterRegistry {
    adapters: HashMap<String, Arc<dyn NodeAdapter>>,
    llm: Option<Arc<dyn LlmAdapter>>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self {
            adapters: HashMap::new(),
            llm: None,
        }
    }

    /// Rejestracja adaptera. Duplicate node_type → ostatnia rejestracja wygrywa
    /// (executor i tak woła `get` po node_type — adapter rejestrowany dwa razy
    /// znaczy że ktoś źle skonfigurował bootstrap).
    pub fn register(&mut self, adapter: Arc<dyn NodeAdapter>) {
        let key = adapter.node_type().to_string();
        self.adapters.insert(key, adapter);
    }

    /// Rejestracja LLM adaptera — equivalent `register` plus zapamiętanie
    /// typed referencji. Wymaga osobnej metody bo `Arc<dyn LlmAdapter>` nie
    /// koerc'uje się do `Arc<dyn NodeAdapter>` automatycznie.
    pub fn register_llm<A>(&mut self, adapter: Arc<A>)
    where
        A: LlmAdapter + 'static,
    {
        let typed: Arc<dyn LlmAdapter> = adapter.clone();
        let generic: Arc<dyn NodeAdapter> = adapter;
        self.adapters.insert(generic.node_type().to_string(), generic);
        self.llm = Some(typed);
    }

    pub fn get(&self, node_type: &str) -> Option<&Arc<dyn NodeAdapter>> {
        self.adapters.get(node_type)
    }

    pub fn has(&self, node_type: &str) -> bool {
        self.adapters.contains_key(node_type)
    }

    pub fn llm(&self) -> Option<&Arc<dyn LlmAdapter>> {
        self.llm.as_ref()
    }

    pub fn registered_types(&self) -> Vec<&str> {
        self.adapters.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for AdapterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
pub mod test_support {
    //! Stub dispatcherów + builder ExecutionContext dla testów adapterów.
    //! Każdy stub panickuje na call — testy które używają konkretnej
    //! capability nadpisują pole na własny mock.

    use super::*;
    use crate::flow_engine::blob_store::{BlobStore, InMemoryBlobStore};
    use crate::flow_engine::dispatchers::audit::AuditEvent;
    use crate::flow_engine::dispatchers::clock::SystemClock;
    use crate::flow_engine::dispatchers::embeddings::{EmbeddingsRequest, EmbeddingsResponse};
    use crate::flow_engine::dispatchers::llm::{LlmRequest, LlmResponse};
    use crate::flow_engine::dispatchers::memory::{
        MemoryQuery, MemoryRecall, MemoryRecord, MemoryStoreReceipt,
    };
    use crate::flow_engine::dispatchers::metrics::NoopMetrics;
    use crate::flow_engine::dispatchers::pii_rules::PiiRule;
    use crate::flow_engine::dispatchers::stt::{SttRequest, SttResponse};
    use crate::flow_engine::dispatchers::tts::{TtsRequest, TtsResponse};
    use crate::flow_engine::envelope::{ChatMessage, FlowEnvelope, LlmStreamChunk};
    use anyhow::Result;
    use async_trait::async_trait;
    use futures::stream::BoxStream;

    pub struct StubLlm;
    #[async_trait]
    impl LlmDispatcher for StubLlm {
        async fn execute_chat(&self, _req: LlmRequest) -> Result<LlmResponse> {
            panic!("stub LlmDispatcher: execute_chat called");
        }
        async fn stream_chat(
            &self,
            _req: LlmRequest,
        ) -> Result<BoxStream<'static, Result<LlmStreamChunk>>> {
            panic!("stub LlmDispatcher: stream_chat called");
        }
    }

    pub struct StubEmbeddings;
    #[async_trait]
    impl EmbeddingsDispatcher for StubEmbeddings {
        async fn embed(&self, _req: EmbeddingsRequest) -> Result<EmbeddingsResponse> {
            panic!("stub EmbeddingsDispatcher: embed called");
        }
    }

    pub struct StubStt;
    #[async_trait]
    impl SttDispatcher for StubStt {
        async fn transcribe(&self, _req: SttRequest) -> Result<SttResponse> {
            panic!("stub SttDispatcher: transcribe called");
        }
    }

    pub struct StubTts;
    #[async_trait]
    impl TtsDispatcher for StubTts {
        async fn synthesize(&self, _req: TtsRequest) -> Result<TtsResponse> {
            panic!("stub TtsDispatcher: synthesize called");
        }
        async fn stream_synthesize(
            &self,
            _req: TtsRequest,
        ) -> Result<futures::stream::BoxStream<'static, Result<crate::flow_engine::dispatchers::TtsStreamChunk>>>
        {
            panic!("stub TtsDispatcher: stream_synthesize called");
        }
    }

    pub struct StubPrompts;
    #[async_trait]
    impl PromptStore for StubPrompts {
        async fn get_prompt(&self, _key: &str, _locale: Option<&str>) -> Result<Option<String>> {
            panic!("stub PromptStore: get_prompt called");
        }
    }

    pub struct StubMemory;
    #[async_trait]
    impl MemoryStore for StubMemory {
        async fn recall(&self, _q: MemoryQuery) -> Result<MemoryRecall> {
            panic!("stub MemoryStore: recall called");
        }
        async fn store(&self, _r: MemoryRecord) -> Result<MemoryStoreReceipt> {
            panic!("stub MemoryStore: store called");
        }
    }

    pub struct StubHistory;
    #[async_trait]
    impl ConversationHistoryStore for StubHistory {
        async fn recent(&self, _s: &str, _n: u32) -> Result<Vec<ChatMessage>> {
            panic!("stub ConversationHistoryStore: recent called");
        }
        async fn append(&self, _s: &str, _m: ChatMessage) -> Result<()> {
            panic!("stub ConversationHistoryStore: append called");
        }
    }

    pub struct StubAudit;
    #[async_trait]
    impl AuditSink for StubAudit {
        async fn record(&self, _e: AuditEvent) -> Result<()> {
            panic!("stub AuditSink: record called");
        }
    }

    pub struct StubPiiRules;
    #[async_trait]
    impl PiiRulesStore for StubPiiRules {
        async fn active_rules(&self) -> Result<Vec<PiiRule>> {
            // Default empty — testy które potrzebują reguł nadpisują pole.
            Ok(Vec::new())
        }
    }

    pub struct StubTtsCleaning;
    #[async_trait]
    impl TtsCleaningStore for StubTtsCleaning {
        async fn clean(&self, text: &str) -> Result<String> {
            // Default identity — testy które potrzebują cleaningu nadpisują pole.
            Ok(text.to_string())
        }
    }

    pub fn stub_ctx() -> ExecutionContext {
        ExecutionContext {
            request_id: "test".into(),
            execution_id: 0,
            session_id: None,
            user_id: None,
            user_role: None,
            deadline: None,
            cancel_token: CancellationToken::new(),
            initial_envelope: Arc::new(FlowEnvelope::empty()),
            clock: Arc::new(SystemClock),
            blobs: Arc::new(InMemoryBlobStore::new()) as Arc<dyn BlobStore>,
            llm: Arc::new(StubLlm),
            embeddings: Arc::new(StubEmbeddings),
            stt: Arc::new(StubStt),
            tts: Arc::new(StubTts),
            prompts: Arc::new(StubPrompts),
            memory: Arc::new(StubMemory),
            history: Arc::new(StubHistory),
            audit: Arc::new(StubAudit),
            metrics: Arc::new(NoopMetrics),
            pii_rules: Arc::new(StubPiiRules),
            tts_cleaning: Arc::new(StubTtsCleaning),
            usage_sink: Arc::new(UsageSink::new()),
        }
    }

    /// Builder ułatwiający test który potrzebuje custom initial envelope —
    /// np. trigger.execute() musi widzieć określony payload/messages.
    pub fn stub_ctx_with_initial(initial: FlowEnvelope) -> ExecutionContext {
        let mut ctx = stub_ctx();
        ctx.initial_envelope = Arc::new(initial);
        ctx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_sink_aggregate_sums_records() {
        let sink = UsageSink::new();
        sink.record(
            "n1",
            TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
            },
        );
        sink.record(
            "n2",
            TokenUsage {
                prompt_tokens: 3,
                completion_tokens: 7,
                total_tokens: 10,
            },
        );
        let agg = sink.aggregate();
        assert_eq!(agg.prompt_tokens, 13);
        assert_eq!(agg.completion_tokens, 12);
        assert_eq!(agg.total_tokens, 25);
    }

    #[test]
    fn usage_sink_drain_clears_state() {
        let sink = UsageSink::new();
        sink.record("a", TokenUsage::default());
        let first = sink.drain();
        assert_eq!(first.len(), 1);
        let second = sink.drain();
        assert!(second.is_empty());
        assert_eq!(sink.aggregate(), TokenUsage::default());
    }

    #[test]
    fn empty_registry_has_no_adapters() {
        let r = AdapterRegistry::new();
        assert!(!r.has("anything"));
        assert!(r.get("anything").is_none());
        assert!(r.llm().is_none());
        assert!(r.registered_types().is_empty());
    }
}
