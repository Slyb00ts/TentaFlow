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
use parking_lot::RwLock;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

use super::dispatchers::{
    AuditSink, Clock, ConversationHistoryStore, DocumentsDispatcher, EmbeddingsDispatcher,
    LlmDispatcher, MemoryStore, MetricsSink, PiiRulesStore, ProgressSink, PromptStore,
    RerankDispatcher, SttDispatcher, TtsCleaningStore, TtsDispatcher, VisionDispatcher,
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
        self.inner.lock().map(|g| g.clone()).unwrap_or_default()
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
///
/// `Clone` jest tani — każde pole to `Arc`/`Option`/`Copy`. Klonowanie jest
/// wymagane przez `SubflowRunner` (§3.5): blok `subflow`/`loop`/`map` wykonuje
/// flow-ciało na KLONIE kontekstu rodzica ze świeżym `execution_id` i świeżym
/// `usage_sink`, zachowując współdzielone dispatchery i guard rekurencji.
#[derive(Clone)]
pub struct ExecutionContext {
    pub request_id: String,
    pub execution_id: i64,
    /// §3.5 — id of the run that spawned this one, recorded as
    /// `flow_executions.parent_execution_id` so the execution tree (parent →
    /// subflow / loop body / map element) is reconstructable. `SubflowRunner`
    /// sets it for real nested runs; top-level and light-mode runs leave it
    /// `None`. Kept separate from `execution_id` (the child's OWN id, assigned
    /// by `execute_blocking`) so the two never collide.
    pub parent_execution_id: Option<i64>,
    /// §3.5 blocks 1/2 — light run mode for `loop` iterations and `map`
    /// elements: `execute_blocking` neither creates nor persists a
    /// `flow_executions` row, so a 25-iteration agent loop (or a 50-element map)
    /// never spams the audit table. The iteration accounting lives in the agent
    /// run log and the parent's single `TraceStep` instead. `subflow`/`agent`
    /// leave this false — they DO get their own audit row.
    pub light: bool,
    pub session_id: Option<String>,
    pub user_id: Option<String>,
    pub user_role: Option<String>,
    /// RAG E1.0 — tożsamość addona-callera (== instance_id; `install_instance`
    /// przepisuje `[addon].id` na instancję, więc to klucz izolacji per-instancja).
    /// `Some` gdy flow został wyzwolony przez addon JAKO MODEL (przez
    /// `service_request`→executor→`FlowDispatcher`). `None` dla wywołań
    /// nie-addonowych (routing /v1 user, kamera, agent) — węzeł retrievalu odmawia
    /// zapisu bez tożsamości zamiast trafiać w cudzą przestrzeń.
    pub addon_id: Option<String>,
    /// RAG E1.0 — organizacja-właściciel wywołania. `None` => domyślny tenant
    /// (`DEFAULT_ORG_ID`) rozwiązywany przy użyciu w węźle. Razem z `addon_id`
    /// składa się na `(org, addon_instance, namespace)` izolacji wektorowej.
    pub org_id: Option<String>,
    pub deadline: Option<Instant>,
    /// §3.13 — accumulated human-wait time (millis) that the deadline check adds
    /// back to `deadline`, so time a run spends parked in `waiting_user` (an
    /// `ask_user` question / permission grant) does NOT consume `agent.timeout_secs`.
    /// Shared (`Arc`) so a clone of the context — a `loop`/`map`/`subflow` body —
    /// extends the SAME deadline its parent enforces. An `ask_user` block / the
    /// permission path bumps it by the millis it waited on the human; the
    /// executor adds it to `deadline` between nodes.
    pub deadline_extension_ms: Arc<std::sync::atomic::AtomicU64>,
    pub cancel_token: CancellationToken,

    /// §3.5 / §3.10 — guard rekurencji sub-flow, trzymany TU (nie w
    /// `envelope.meta`), bo meta jest zapisywalne przez każdy node, w tym blok
    /// addonu WASM deserializujący cały envelope z odpowiedzi gościa
    /// (`addon.rs:173`) — guard w meta dałby się wyzerować i otworzyć
    /// nieograniczoną rekurencję. `subflow_depth` to liczba zagnieżdżeń
    /// sub-flow nad bieżącym wykonaniem (0 = top-level).
    pub subflow_depth: u8,
    /// §3.5 — zbiór flow_id'ów na ścieżce stosu sub-flow (do detekcji cyklu:
    /// flow A → subflow B → subflow A jest błędem). `Arc` żeby klon kontekstu
    /// dzielił tę samą listę bez kopiowania; `SubflowRunner` rozszerza ją o
    /// flow dziecka przed zejściem w głąb.
    pub subflow_visited: Arc<Vec<String>>,

    /// Seed envelope dostarczony przez routing (request_id, model, payload,
    /// initial messages). Plan v4.2 D2: używa go TYLKO trigger.execute().
    /// Inne adaptery czytają inputs[0]; streaming LLM czyta envelope po
    /// wszystkich pre-LLM nodach, NIE initial.
    pub initial_envelope: Arc<FlowEnvelope>,

    pub clock: Arc<dyn Clock>,
    pub blobs: Arc<dyn BlobStore>,

    /// RAG E1.0 — rejestr przestrzeni wektorowych `(org, addon_instance, namespace)`.
    /// `VectorNodeAdapter` uderza w niego z `ctx.addon_id`/`ctx.org_id`. Współdzielony
    /// proces-szeroki manager (`services::vector_namespace_manager`); testy wstrzykują
    /// `with_root(tempdir)`.
    pub vectors: Arc<crate::services::vector::NamespaceManager>,

    /// RAG E1.1 — rejestr kolekcji grafowych `(org, addon_instance, collection)`.
    /// `GraphSearchNodeAdapter` uderza w niego z `ctx.addon_id`/`ctx.org_id`,
    /// dokładnie jak `vectors`. Współdzielony proces-szeroki manager
    /// (`services::graph_manager`); testy wstrzykują `with_root(tempdir)`. Pod
    /// `feature = "graph"` — graf jest opt-in (cozo nie w default features), więc
    /// pole i węzeł graph_search istnieją tylko gdy feature włączone.
    #[cfg(feature = "graph")]
    pub graph: Arc<crate::services::graph::GraphManager>,

    pub llm: Arc<dyn LlmDispatcher>,
    pub embeddings: Arc<dyn EmbeddingsDispatcher>,
    /// RAG C2 — cross-encoder reranker (/v1/rerank, alias `rag-reranker`).
    /// Krok retrievalu między vector-search a LLM.
    pub reranker: Arc<dyn RerankDispatcher>,
    /// PARTIA 0 (flow-ingest RAG) — typed surface `Documents` (`/v1/infer`).
    /// Detektory struktury strony dla node-adapterów page_detect/table/ocr (PARTIA 2).
    pub documents: Arc<dyn DocumentsDispatcher>,
    pub stt: Arc<dyn SttDispatcher>,
    pub tts: Arc<dyn TtsDispatcher>,
    pub vision: Arc<dyn VisionDispatcher>,
    pub prompts: Arc<dyn PromptStore>,
    pub memory: Arc<dyn MemoryStore>,
    pub history: Arc<dyn ConversationHistoryStore>,
    pub audit: Arc<dyn AuditSink>,
    pub metrics: Arc<dyn MetricsSink>,
    pub pii_rules: Arc<dyn PiiRulesStore>,
    pub tts_cleaning: Arc<dyn TtsCleaningStore>,

    /// §3.11 C — ephemeral execution progress fan-out. The executor emits
    /// NodeStarted/NodeFinished here; later phases (loop/map/router/child)
    /// emit their own variants. `scope` for emission is `progress_scope`.
    /// Defaults to a no-op when no broker is wired (headless / tests).
    pub progress: Arc<dyn ProgressSink>,
    /// Broadcast key for `progress` emissions — session id, falling back to
    /// the request id so a scope always exists even without a session.
    pub progress_scope: String,

    pub usage_sink: Arc<UsageSink>,
}

impl ExecutionContext {
    /// Effective deadline = the base deadline pushed back by the human-wait time
    /// accumulated in `deadline_extension_ms` (§3.13). The executor checks this
    /// between nodes instead of the bare `deadline`, so a run parked in
    /// `waiting_user` does not burn its `agent.timeout_secs`.
    pub fn effective_deadline(&self) -> Option<Instant> {
        let base = self.deadline?;
        let extra = self
            .deadline_extension_ms
            .load(std::sync::atomic::Ordering::Relaxed);
        Some(base + std::time::Duration::from_millis(extra))
    }

    /// Records `waited` human-wait time so it is added back to the deadline. An
    /// `ask_user` block / the permission path calls this after a human reply
    /// (or timeout) so the time spent blocked on a person is not charged against
    /// the run's budget.
    pub fn extend_deadline(&self, waited: std::time::Duration) {
        self.deadline_extension_ms.fetch_add(
            waited.as_millis() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

/// Pojedynczy port — nazwa + typ danych. Adapter zwraca `Vec<PortSpec>` z
/// `input_ports()`/`output_ports()`. Owned String pozwala na dynamiczne porty
/// (addon block adapter buduje listę z manifest blocks.json) bez `'static`
/// constraintu na rdzeniowych adapterach.
#[derive(Debug, Clone)]
pub struct PortSpec {
    pub name: String,
    pub data_type: FlowDataType,
}

impl PortSpec {
    pub fn new(name: impl Into<String>, data_type: FlowDataType) -> Self {
        Self {
            name: name.into(),
            data_type,
        }
    }
}

#[async_trait]
pub trait NodeAdapter: Send + Sync {
    fn node_type(&self) -> &str;

    /// Lista wspieranych input portów. Każdy z deklaracją typu (FlowDataType)
    /// dla walidacji R8 (edge type compatibility). Walidacja R3 sprawdza
    /// `edge.to_port` ∈ {p.name}.
    fn input_ports(&self) -> Vec<PortSpec>;

    /// Lista wspieranych output portów (analogicznie do `input_ports`).
    fn output_ports(&self) -> Vec<PortSpec>;

    /// Pojedyncza metoda execute — zgodnie z hard rule 8 z planu v4.1.
    /// Streaming jest cechą flow (executor decyduje), nie adaptera. LLM
    /// adapter ma osobną concrete metodę `prepare_request` w impl.
    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope>;

    /// Etap 2: typ danych przyjmowanych na danym input port. Default —
    /// derive z `input_ports()` (lookup po nazwie); jeśli port nie jest
    /// zadeklarowany zwraca `Any` (passthrough). Adapter z prostym mappingiem
    /// 1:1 może nie nadpisywać tej metody.
    fn input_port_type(&self, port: &str) -> FlowDataType {
        self.input_ports()
            .into_iter()
            .find(|p| p.name == port)
            .map(|p| p.data_type)
            .unwrap_or(FlowDataType::Any)
    }

    /// Etap 2: typ danych emitowanych na danym output port. Default —
    /// derive z `output_ports()` (analogicznie do `input_port_type`).
    fn output_port_type(&self, port: &str) -> FlowDataType {
        self.output_ports()
            .into_iter()
            .find(|p| p.name == port)
            .map(|p| p.data_type)
            .unwrap_or(FlowDataType::Any)
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

    /// Faza 4 §3.11 A — bramkowanie gałęzi (skip-semantyka). Po `execute`
    /// executor pyta adapter, które output porty są AKTYWNE dla tego konkretnego
    /// wyniku; następnik osiągalny WYŁĄCZNIE krawędziami z nieaktywnych portów
    /// dostaje status `Skipped` i nie wykonuje się.
    ///
    /// `None` (default) = wszystkie porty aktywne — zero zmian dla istniejących
    /// adapterów. Dodane jako metoda traitu z domyślną implementacją (NIE zmiana
    /// sygnatury `execute`), żeby równolegle rozwijane adaptery dalej się
    /// kompilowały bez dotykania ich kodu.
    ///
    /// `condition` nadpisuje to, zwracając dokładnie `{"true"}` albo `{"false"}`.
    fn active_output_ports(
        &self,
        _node: &FlowNode,
        _result: &FlowEnvelope,
    ) -> Option<HashSet<String>> {
        None
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

/// Stage 3d Krok 2: streaming-aware adapter dla nodów które konsumują
/// upstream `EnvelopeDelta` stream i produkują downstream stream.
/// Używane do chain budowy w `executor::execute_streaming`:
/// `LLM → pii_filter (StreamingNodeAdapter) → tts_stream_bridge
/// (StreamingNodeAdapter) → output(stream)`.
///
/// Adapter implementujący ten trait MUSI też implementować `NodeAdapter`
/// (blocking ścieżka) — `register_streaming<T>` rejestruje go w obu slotach
/// rejestru.
#[async_trait]
pub trait StreamingNodeAdapter: NodeAdapter {
    /// Konsumuje upstream envelope stream, produkuje downstream envelope
    /// stream. `seed_envelope` to ostatni FlowEnvelope przed stream chain'em
    /// (zazwyczaj producer LLM blocking output) — pozwala adapterowi zasiać
    /// stan z payload + meta przed pierwszym chunkiem.
    async fn process_stream(
        &self,
        node: &FlowNode,
        upstream: futures::stream::BoxStream<
            'static,
            anyhow::Result<crate::flow_engine::envelope::EnvelopeDelta>,
        >,
        seed_envelope: std::sync::Arc<crate::flow_engine::envelope::FlowEnvelope>,
        ctx: &ExecutionContext,
    ) -> anyhow::Result<
        futures::stream::BoxStream<
            'static,
            anyhow::Result<crate::flow_engine::envelope::EnvelopeDelta>,
        >,
    >;

    /// Typ delty który adapter konsumuje (np. `Llm` dla pii_filter). R8
    /// chain compatibility: producer.stream_output_kind == consumer.stream_input_kind.
    fn stream_input_kind(&self) -> crate::flow_engine::envelope::EnvelopeDeltaKind;

    /// Typ delty który adapter emituje. `pii_filter` Llm→Llm; `tts_stream_bridge`
    /// Llm→Audio; future STT bridges Audio→Text.
    fn stream_output_kind(&self) -> crate::flow_engine::envelope::EnvelopeDeltaKind;
}

/// Faza 4 §3.11 B — uogólnienie producenta strumienia. Dziś tylko LLM potrafi
/// produkować `EnvelopeDelta` (executor zakładał slot `registry.llm()`); ten
/// trait pozwala KAŻDEMU node'owi być źródłem strumienia (harness loop /
/// subflow forward, addon stream block itp.). Producent = node z wychodzącą
/// krawędzią `from_port="stream"` który ma zarejestrowany `StreamProducerAdapter`.
///
/// Adapter implementujący ten trait MUSI też implementować `NodeAdapter`
/// (blocking ścieżka dla non-streaming flow); `register_stream_producer<T>`
/// rejestruje go w obu slotach. `LlmNodeAdapter` jest jednym z producentów —
/// jego impl owija dotychczasową ścieżkę `prepare_llm_request` +
/// `ctx.llm.stream_chat`, BEZ duplikacji budowania requestu.
#[async_trait]
pub trait StreamProducerAdapter: NodeAdapter {
    /// Buduje strumień `EnvelopeDelta` dla tego node'a. Wołane przez
    /// `execute_streaming` po wykonaniu wszystkich pre-producent nodów —
    /// `inputs` to rozwiązane wejścia producenta (zwykle jedno). Strumień
    /// jest `'static` (spawnowany do finalizera), więc adapter klonuje co
    /// potrzebne z `ctx` przed zwróceniem.
    async fn produce_stream(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<
        futures::stream::BoxStream<
            'static,
            anyhow::Result<crate::flow_engine::envelope::EnvelopeDelta>,
        >,
    >;
}

/// Resolver dla dynamicznych typów node — adaptery rejestrowane runtime po
/// instalacji addonu (np. block z `addon.{id}.{name}`). Zwraca `None` gdy nie
/// znajduje match'a; registry zwraca wynik z `dynamic_resolver` jeśli builtin
/// map nie zawiera node_type.
pub type DynamicAdapterResolver = Arc<dyn Fn(&str) -> Option<Arc<dyn NodeAdapter>> + Send + Sync>;

/// Registry z typed accessorem dla LLM (plan v4.1 — bez downcastu) + streaming
/// slot (Krok 2) + dynamic_resolver dla addon block adapterów. Adaptery
/// dual-trait (NodeAdapter + StreamingNodeAdapter) rejestrują się przez
/// `register_streaming` w obu slotach.
///
/// Lookup priority: builtin `adapters` > `dynamic_resolver` (jeśli ustawiony).
/// To pozwala core adapterowi wygrać z addonem deklarującym ten sam node_type
/// (np. addon malicious rejestrujący `llm` nie nadpisze prawdziwego).
pub struct AdapterRegistry {
    adapters: HashMap<String, Arc<dyn NodeAdapter>>,
    llm: Option<Arc<dyn LlmAdapter>>,
    streaming_adapters: HashMap<String, Arc<dyn StreamingNodeAdapter>>,
    /// §3.11 B — node_type → stream producer. Generalizuje stary slot
    /// `llm`-only: dowolny node_type może produkować `EnvelopeDelta`.
    /// `LlmNodeAdapter` rejestruje się tu obok `llm` slotu (jeden node, oba
    /// kontrakty).
    stream_producers: HashMap<String, Arc<dyn StreamProducerAdapter>>,
    /// Resolver dla node_type'ów nie znalezionych w `adapters`. Cache wynikow
    /// (jeden lookup = jedno wywolanie) wewnatrz resolver-impl, registry nie
    /// memoize'uje — co compile flow to nowe pytanie. RwLock bo `set` jest
    /// jednorazowe (po inicjalizacji `AddonManager`), reads tylko klonują
    /// Arc — kontencja zerowa w praktyce.
    dynamic_resolver: RwLock<Option<DynamicAdapterResolver>>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self {
            adapters: HashMap::new(),
            llm: None,
            streaming_adapters: HashMap::new(),
            stream_producers: HashMap::new(),
            dynamic_resolver: RwLock::new(None),
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
    /// typed referencji ORAZ rejestracja w slocie producentów strumienia
    /// (§3.11 B — LLM jest jednym z producentów). Wymaga osobnej metody bo
    /// `Arc<dyn LlmAdapter>` nie koerc'uje się do `Arc<dyn NodeAdapter>`
    /// automatycznie. Bound `StreamProducerAdapter` jest wymuszony — LLM
    /// MUSI umieć produkować strumień, a wpis w `stream_producers` zastępuje
    /// stary slot `llm`-only w detekcji producenta (cache/validation/executor).
    pub fn register_llm<A>(&mut self, adapter: Arc<A>)
    where
        A: LlmAdapter + StreamProducerAdapter + 'static,
    {
        let key = adapter.node_type().to_string();
        let typed: Arc<dyn LlmAdapter> = adapter.clone();
        let producer: Arc<dyn StreamProducerAdapter> = adapter.clone();
        let generic: Arc<dyn NodeAdapter> = adapter;
        self.adapters.insert(key.clone(), generic);
        self.stream_producers.insert(key, producer);
        self.llm = Some(typed);
    }

    /// §3.11 B — rejestracja non-LLM producenta strumienia (np. harness
    /// `loop`/`subflow` forward, addon stream block). Generic bound + osobna
    /// koercja per slot — trait-object upcasting nie wymagany.
    pub fn register_stream_producer<T>(&mut self, adapter: Arc<T>)
    where
        T: NodeAdapter + StreamProducerAdapter + 'static,
    {
        let key = adapter.node_type().to_string();
        let blocking: Arc<dyn NodeAdapter> = adapter.clone();
        let producer: Arc<dyn StreamProducerAdapter> = adapter;
        self.adapters.insert(key.clone(), blocking);
        self.stream_producers.insert(key, producer);
    }

    /// §3.11 B — accessor producenta strumienia per node_type. Executor i
    /// detekcja producenta (cache/validation) używają tego zamiast zakładać
    /// LLM. `None` = node nie potrafi produkować strumienia.
    pub fn stream_producer(&self, node_type: &str) -> Option<&Arc<dyn StreamProducerAdapter>> {
        self.stream_producers.get(node_type)
    }

    /// §3.11 B — czy dany node_type ma zarejestrowanego producenta strumienia.
    pub fn is_stream_producer(&self, node_type: &str) -> bool {
        self.stream_producers.contains_key(node_type)
    }

    /// Ustawia dynamic resolver dla node_type'ów nie zarejestrowanych jako
    /// builtin. Może być wołane z innego wątku po inicjalizacji rejestru.
    /// Nadpisuje poprzedni resolver.
    pub fn set_dynamic_resolver(&self, resolver: DynamicAdapterResolver) {
        *self.dynamic_resolver.write() = Some(resolver);
    }

    /// Zwraca adapter dla podanego node_type. Najpierw szuka w builtin map,
    /// fallback przez `dynamic_resolver` (jeśli skonfigurowany). Wynik
    /// resolvera jest klonem `Arc` — nie cache'ujemy w registry, bo addon
    /// może w międzyczasie zostać odinstalowany.
    pub fn get(&self, node_type: &str) -> Option<Arc<dyn NodeAdapter>> {
        if let Some(a) = self.adapters.get(node_type) {
            return Some(a.clone());
        }
        let resolver = self.dynamic_resolver.read().clone();
        resolver.and_then(|r| r(node_type))
    }

    pub fn has(&self, node_type: &str) -> bool {
        if self.adapters.contains_key(node_type) {
            return true;
        }
        let resolver = self.dynamic_resolver.read().clone();
        resolver.map(|r| r(node_type).is_some()).unwrap_or(false)
    }

    pub fn llm(&self) -> Option<&Arc<dyn LlmAdapter>> {
        self.llm.as_ref()
    }

    /// Zwraca tylko statycznie zarejestrowane node_type'y. Dynamiczne addon
    /// block typy nie są tu wymienione, bo resolver nie wie a priori jakie
    /// typy potrafi obsłużyć — to znana niedokładność (acceptable: GUI list
    /// addon blocks z `AddonFlowRegistry`, builtin types z tego API).
    pub fn registered_types(&self) -> Vec<&str> {
        self.adapters.keys().map(|s| s.as_str()).collect()
    }

    /// Stage 3d Krok 2: rejestracja adaptera implementującego `NodeAdapter` +
    /// `StreamingNodeAdapter`. Generic bound + osobna koercja per slot —
    /// trait-object upcasting nie wymagany.
    pub fn register_streaming<T>(&mut self, adapter: Arc<T>)
    where
        T: NodeAdapter + StreamingNodeAdapter + 'static,
    {
        let key = adapter.node_type().to_string();
        let blocking: Arc<dyn NodeAdapter> = adapter.clone();
        let streaming: Arc<dyn StreamingNodeAdapter> = adapter;
        self.adapters.insert(key.clone(), blocking);
        self.streaming_adapters.insert(key, streaming);
    }

    /// Streaming-aware accessor — zwraca `Some` gdy node_type ma rejestrację
    /// `StreamingNodeAdapter`. Executor stream chain woła to żeby zbudować
    /// fold pipeline; brak rejestracji oznacza że node nie obsługuje stream.
    pub fn streaming_adapter(&self, node_type: &str) -> Option<&Arc<dyn StreamingNodeAdapter>> {
        self.streaming_adapters.get(node_type)
    }
}

impl Default for AdapterRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-support"))]
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
    use crate::flow_engine::dispatchers::progress::NoopProgress;
    use crate::flow_engine::dispatchers::rerank::{
        RerankRequest, RerankResponse, RerankResult,
    };
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

    /// Deterministyczny stub rerankera: score = pozycja od końca (pierwszy
    /// dokument dostaje najwyższy score), wynik posortowany malejąco, ucięty
    /// do `top_n`. NIE panickuje — inne węzły z `stub_ctx` mają działać.
    pub struct StubReranker;
    #[async_trait]
    impl RerankDispatcher for StubReranker {
        async fn rerank(&self, req: RerankRequest) -> Result<RerankResponse> {
            let n = req.documents.len();
            let mut results: Vec<RerankResult> = (0..n)
                .map(|i| RerankResult {
                    index: i,
                    score: (n - i) as f32,
                })
                .collect();
            results.sort_by(|a, b| b.score.total_cmp(&a.score));
            if let Some(top) = req.top_n {
                results.truncate(top as usize);
            }
            Ok(RerankResponse {
                results,
                usage: crate::flow_engine::envelope::TokenUsage::default(),
            })
        }
    }

    /// Stub typed-surface Documents: zwraca pusty `regions` — inne węzły z
    /// `stub_ctx` mają działać (nie panickuje). Realna detekcja idzie przez
    /// `DocumentsDispatcherImpl` wpięty w Router::new.
    pub struct StubDocuments;
    #[async_trait]
    impl DocumentsDispatcher for StubDocuments {
        async fn infer(
            &self,
            _model: &str,
            _image: &[u8],
            _mime: &str,
            _task: &str,
        ) -> std::result::Result<tentaflow_protocol::DocumentInferResult, String> {
            Ok(tentaflow_protocol::DocumentInferResult {
                regions: Vec::new(),
            })
        }
        async fn parse(
            &self,
            _model: &str,
            _image: &[u8],
            _mime: &str,
        ) -> std::result::Result<String, String> {
            Ok(String::new())
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
        ) -> Result<
            futures::stream::BoxStream<
                'static,
                Result<crate::flow_engine::dispatchers::TtsStreamChunk>,
            >,
        > {
            Ok(Box::pin(futures::stream::empty()))
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
        async fn append_batch(&self, _s: &str, _m: &[ChatMessage]) -> Result<()> {
            panic!("stub ConversationHistoryStore: append_batch called");
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

    /// Stub `NamespaceManager` na izolowanym tempdirze (root pod `TMPDIR`, więc
    /// nie dotyka `~/.tentaflow` ani sieciowego dysku). DB in-memory z pełnymi
    /// migracjami (tabela `addon_vector_namespaces`). Każde wywołanie daje świeży
    /// katalog (unikalny suffix), więc testy nie współdzielą stanu. Katalog NIE
    /// jest sprzątany — to ephemeralny tmpfs/scratch w testach.
    pub fn stub_vectors() -> Arc<crate::services::vector::NamespaceManager> {
        use rusqlite::Connection;
        let conn = Connection::open_in_memory().expect("in-memory db");
        crate::db::migrations::run(&conn).expect("run migrations");
        let pool = Arc::new(crate::db::Db::from_connection(conn));
        let root = std::env::temp_dir().join(format!(
            "tf-vec-stub-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).expect("create stub vectors root");
        Arc::new(crate::services::vector::NamespaceManager::with_root(
            pool, root,
        ))
    }

    /// Stub `GraphManager` na izolowanym tempdirze (root pod `TMPDIR`), lustro
    /// `stub_vectors`. DB in-memory z pełnymi migracjami (tabela
    /// `addon_graph_collections`). Każde wywołanie daje świeży katalog, więc testy
    /// nie współdzielą stanu. Katalog NIE jest sprzątany (ephemeralny scratch).
    #[cfg(feature = "graph")]
    pub fn stub_graph() -> Arc<crate::services::graph::GraphManager> {
        use rusqlite::Connection;
        let conn = Connection::open_in_memory().expect("in-memory db");
        crate::db::migrations::run(&conn).expect("run migrations");
        let pool = Arc::new(crate::db::Db::from_connection(conn));
        let root = std::env::temp_dir().join(format!(
            "tf-graph-stub-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).expect("create stub graph root");
        Arc::new(crate::services::graph::GraphManager::with_root(pool, root))
    }

    pub fn stub_ctx() -> ExecutionContext {
        ExecutionContext {
            request_id: "test".into(),
            execution_id: 0,
            parent_execution_id: None,
            light: false,
            session_id: None,
            user_id: None,
            user_role: None,
            addon_id: None,
            org_id: None,
            deadline: None,
            deadline_extension_ms: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            cancel_token: CancellationToken::new(),
            subflow_depth: 0,
            subflow_visited: Arc::new(Vec::new()),
            initial_envelope: Arc::new(FlowEnvelope::empty()),
            clock: Arc::new(SystemClock),
            blobs: Arc::new(InMemoryBlobStore::new()) as Arc<dyn BlobStore>,
            vectors: stub_vectors(),
            #[cfg(feature = "graph")]
            graph: stub_graph(),
            llm: Arc::new(StubLlm),
            embeddings: Arc::new(StubEmbeddings),
            reranker: Arc::new(StubReranker),
            documents: Arc::new(StubDocuments),
            stt: Arc::new(StubStt),
            tts: Arc::new(StubTts),
            vision: Arc::new(crate::flow_engine::dispatchers_impl::VisionDispatcherImpl::new()),
            prompts: Arc::new(StubPrompts),
            memory: Arc::new(StubMemory),
            history: Arc::new(StubHistory),
            audit: Arc::new(StubAudit),
            metrics: Arc::new(NoopMetrics),
            pii_rules: Arc::new(StubPiiRules),
            tts_cleaning: Arc::new(StubTtsCleaning),
            progress: Arc::new(NoopProgress),
            progress_scope: "test".into(),
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

    /// Capturing `ProgressSink` — records every (scope, event) for assertions.
    /// Used by executor / dispatcher tests to prove NodeStarted/NodeFinished
    /// emission (§3.11 C).
    #[derive(Default)]
    pub struct CapturingProgress {
        events: Mutex<Vec<(String, super::super::dispatchers::ProgressEvent)>>,
    }

    impl CapturingProgress {
        pub fn new() -> Self {
            Self::default()
        }

        /// Snapshot of all captured events in emission order.
        pub fn events(&self) -> Vec<(String, super::super::dispatchers::ProgressEvent)> {
            self.events.lock().map(|g| g.clone()).unwrap_or_default()
        }
    }

    impl super::super::dispatchers::ProgressSink for CapturingProgress {
        fn emit(&self, scope: &str, event: super::super::dispatchers::ProgressEvent) {
            if let Ok(mut g) = self.events.lock() {
                g.push((scope.to_string(), event));
            }
        }
    }

    /// Minimalny non-LLM `StreamProducerAdapter` dla testów (§3.11 B). Emituje
    /// stały dwuchunkowy strumień `EnvelopeDelta::Llm` (text + terminal), żeby
    /// dowieść że executor streamuje z dowolnego zarejestrowanego producenta,
    /// nie tylko ze slotu LLM.
    pub struct TestStreamProducer {
        node_type: String,
    }

    impl TestStreamProducer {
        pub fn new(node_type: impl Into<String>) -> Self {
            Self {
                node_type: node_type.into(),
            }
        }
    }

    #[async_trait]
    impl NodeAdapter for TestStreamProducer {
        fn node_type(&self) -> &str {
            &self.node_type
        }
        fn input_ports(&self) -> Vec<PortSpec> {
            vec![PortSpec::new(
                "in",
                crate::flow_engine::types::FlowDataType::Text,
            )]
        }
        fn output_ports(&self) -> Vec<PortSpec> {
            vec![
                PortSpec::new("stream", crate::flow_engine::types::FlowDataType::Text),
                PortSpec::new("full", crate::flow_engine::types::FlowDataType::Text),
            ]
        }
        async fn execute(
            &self,
            _node: &FlowNode,
            inputs: &[NodeInput],
            _ctx: &ExecutionContext,
        ) -> Result<FlowEnvelope> {
            let mut out = inputs
                .first()
                .map(|i| (*i.envelope).clone())
                .unwrap_or_else(FlowEnvelope::empty);
            out.payload = crate::flow_engine::envelope::FlowValue::Text("test-produced".into());
            Ok(out)
        }
    }

    #[async_trait]
    impl StreamProducerAdapter for TestStreamProducer {
        async fn produce_stream(
            &self,
            _node: &FlowNode,
            _inputs: &[NodeInput],
            _ctx: &ExecutionContext,
        ) -> Result<
            BoxStream<'static, Result<crate::flow_engine::envelope::EnvelopeDelta>>,
        > {
            use crate::flow_engine::envelope::{EnvelopeDelta, FinishReason};
            use futures::StreamExt;
            let first = LlmStreamChunk {
                text_delta: "hello from test producer".into(),
                ..Default::default()
            };
            let last = LlmStreamChunk {
                finish_reason: Some(FinishReason::Stop),
                ..Default::default()
            };
            let items = vec![
                Ok(EnvelopeDelta::Llm(first)),
                Ok(EnvelopeDelta::Llm(last)),
            ];
            Ok(futures::stream::iter(items).boxed())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extend_deadline_pushes_effective_deadline_back() {
        // §3.13 — a run's human-wait time is added back to the deadline so a
        // run parked in waiting_user does not burn its budget.
        let base = Instant::now() + std::time::Duration::from_secs(10);
        let mut ctx = test_support::stub_ctx();
        ctx.deadline = Some(base);
        // No extension yet: effective == base.
        assert_eq!(ctx.effective_deadline(), Some(base));
        // A 5 s human wait pushes the effective deadline back by 5 s.
        ctx.extend_deadline(std::time::Duration::from_secs(5));
        let eff = ctx.effective_deadline().expect("deadline");
        assert!(eff >= base + std::time::Duration::from_secs(5));
        // Extensions accumulate.
        ctx.extend_deadline(std::time::Duration::from_secs(3));
        let eff2 = ctx.effective_deadline().expect("deadline");
        assert!(eff2 >= base + std::time::Duration::from_secs(8));
        // No base deadline → no effective deadline (an unbounded run).
        ctx.deadline = None;
        assert_eq!(ctx.effective_deadline(), None);
    }

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
