// =============================================================================
// Plik: flow_engine/cache.rs
// Opis: CompiledFlow + cache. Stage 1d zastępuje legacy ParsedFlow — compile()
//       woła validation::validate jako pierwszy krok i buduje immutable
//       snapshot (toposort + adjacency + streaming detection). FlowCache trzyma
//       sparsowany flow per (model, service_type) z TTL.
// =============================================================================

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crate::db::models::DbFlow;
use crate::flow_engine::node_adapter::AdapterRegistry;
use crate::flow_engine::types::{FlowDefinition, FlowNode};
use crate::flow_engine::validation::{validate, FlowValidationError};

const MAX_FLOW_NODES: usize = 256;
const MAX_FLOW_EDGES: usize = 1024;

/// Skompilowany flow gotowy do wykonania. Trzyma definicję + immutable
/// metadane (kolejność topo, adjacency, streaming detection).
#[derive(Debug)]
pub struct CompiledFlow {
    pub flow_id: i64,
    pub definition: Arc<FlowDefinition>,
    /// Kolejność wykonywania jako indeksy do `definition.nodes`. Używana przez
    /// executor w pętli topo.
    pub execution_order: Vec<usize>,
    /// Per-pozycja w execution_order: indeksy krawędzi wchodzących do tego
    /// node'a (indeksy do `definition.edges`).
    pub incoming_edges_per_pos: Vec<Vec<usize>>,
    /// node_id → pozycja w `execution_order` (zarówno producer jak i consumer
    /// edge'a używa tego do mapowania na slot outputs[]).
    pub run_idx_by_id: HashMap<String, usize>,
    /// Flag: czy flow ma streaming end-shape (przynajmniej jeden edge z
    /// `from_port == "stream"`). Detekcja w compile time, nie scanowanie
    /// per-execution.
    pub is_streaming: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("flow has no nodes")]
    Empty,
    #[error("flow exceeds {limit} nodes (has {actual})")]
    TooManyNodes { limit: usize, actual: usize },
    #[error("flow exceeds {limit} edges (has {actual})")]
    TooManyEdges { limit: usize, actual: usize },
    #[error("flow has a cycle (sorted {sorted} of {total})")]
    Cycle { sorted: usize, total: usize },
    #[error("validation failed: {0}")]
    Validation(#[from] FlowValidationError),
    #[error("invalid flow_json: {0}")]
    Json(String),
}

impl CompiledFlow {
    pub fn from_json(
        flow_id: i64,
        flow_json: &str,
        registry: &AdapterRegistry,
        source: crate::flow_engine::validation::ValidationSource,
    ) -> Result<Self, CompileError> {
        let definition: FlowDefinition = serde_json::from_str(flow_json)
            .map_err(|e| CompileError::Json(e.to_string()))?;
        Self::compile(flow_id, definition, registry, source)
    }

    pub fn compile(
        flow_id: i64,
        definition: FlowDefinition,
        registry: &AdapterRegistry,
        source: crate::flow_engine::validation::ValidationSource,
    ) -> Result<Self, CompileError> {
        if definition.nodes.is_empty() {
            return Err(CompileError::Empty);
        }
        if definition.nodes.len() > MAX_FLOW_NODES {
            return Err(CompileError::TooManyNodes {
                limit: MAX_FLOW_NODES,
                actual: definition.nodes.len(),
            });
        }
        if definition.edges.len() > MAX_FLOW_EDGES {
            return Err(CompileError::TooManyEdges {
                limit: MAX_FLOW_EDGES,
                actual: definition.edges.len(),
            });
        }
        validate(&definition, registry, source)?;

        let order_ids = topological_sort(&definition)?;
        let node_idx_in_def: HashMap<&str, usize> = definition
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id.as_str(), i))
            .collect();
        let execution_order: Vec<usize> = order_ids
            .iter()
            .map(|id| node_idx_in_def[id.as_str()])
            .collect();
        let run_idx_by_id: HashMap<String, usize> = order_ids
            .iter()
            .enumerate()
            .map(|(pos, id)| (id.clone(), pos))
            .collect();
        let n = execution_order.len();
        let mut incoming_edges_per_pos: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (edge_idx, edge) in definition.edges.iter().enumerate() {
            if let Some(&to_pos) = run_idx_by_id.get(edge.to.as_str()) {
                incoming_edges_per_pos[to_pos].push(edge_idx);
            }
        }
        let is_streaming = definition.edges.iter().any(|e| e.from_port == "stream");
        Ok(Self {
            flow_id,
            definition: Arc::new(definition),
            execution_order,
            incoming_edges_per_pos,
            run_idx_by_id,
            is_streaming,
        })
    }

    /// Pozycja node'a "trigger" w execution_order. Walidacja gwarantuje że
    /// dokładnie jeden trigger istnieje, więc zwracamy `Option` defensywnie
    /// dla executora zanim runtime zacznie wymagać Some.
    pub fn trigger_run_idx(&self) -> Option<usize> {
        self.execution_order
            .iter()
            .position(|&def_idx| self.definition.nodes[def_idx].node_type == "trigger")
    }

    pub fn trigger_node(&self) -> Option<&FlowNode> {
        self.trigger_run_idx()
            .map(|i| &self.definition.nodes[self.execution_order[i]])
    }

    pub fn continue_on_error(&self) -> bool {
        self.trigger_node()
            .and_then(|n| n.config.get("continue_on_error"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Pozycja w execution_order node'a LLM, którego krawędź wyjściowa ma
    /// `from_port="stream"`. Walidacja streaming end-shape gwarantuje co
    /// najwyżej jeden taki node; brak = `None`.
    pub fn streaming_llm_run_idx(&self) -> Option<usize> {
        if !self.is_streaming {
            return None;
        }
        for edge in self.definition.edges.iter() {
            if edge.from_port == "stream" {
                if let Some(&pos) = self.run_idx_by_id.get(edge.from.as_str()) {
                    return Some(pos);
                }
            }
        }
        None
    }

    /// Stage 3d Krok 2c-2: chain stream nodes po LLM (intermediate
    /// streaming-aware nody między LLM a output sink). Walks `from_port=
    /// "stream"` edges starting from LLM, kolejność topologiczna
    /// (execution_order indices). Zatrzymuje się gdy konsument to
    /// `output` node (sink) — output nie jest w chain'ie.
    ///
    /// Przykład: `llm.stream → pii_filter.stream → tts_stream_bridge.full →
    /// output` zwraca `[run_idx(pii_filter), run_idx(tts_stream_bridge)]`.
    pub fn streaming_chain_run_idxs(&self) -> Vec<usize> {
        let Some(llm_idx) = self.streaming_llm_run_idx() else {
            return Vec::new();
        };
        let llm_def_idx = self.execution_order[llm_idx];
        let llm_node_id = self.definition.nodes[llm_def_idx].id.as_str();

        let mut chain: Vec<usize> = Vec::new();
        let mut current_id = llm_node_id.to_string();
        loop {
            // Find edge `from_port="stream"` z current_id.
            let next_edge = self
                .definition
                .edges
                .iter()
                .find(|e| e.from == current_id && e.from_port == "stream");
            let Some(edge) = next_edge else { break };
            // Sprawdź czy konsument to output (sink). Output node
            // zatrzymuje chain — nie idzie do chain Vec.
            let consumer_def_idx = self
                .definition
                .nodes
                .iter()
                .position(|n| n.id == edge.to);
            let Some(consumer_pos) = consumer_def_idx else { break };
            let consumer_node = &self.definition.nodes[consumer_pos];
            if consumer_node.node_type == "output" {
                break;
            }
            // Streaming-aware intermediate node — zapisz w chain'ie.
            if let Some(&run_idx) = self.run_idx_by_id.get(edge.to.as_str()) {
                chain.push(run_idx);
            }
            current_id = edge.to.clone();
        }
        chain
    }
}

/// Sortowanie topologiczne (Kahn). Zwraca błąd CompileError::Cycle gdy graph
/// ma cykl.
fn topological_sort(def: &FlowDefinition) -> Result<Vec<String>, CompileError> {
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();

    for node in &def.nodes {
        in_degree.entry(node.id.as_str()).or_insert(0);
        adjacency.entry(node.id.as_str()).or_default();
    }
    for edge in &def.edges {
        adjacency
            .entry(edge.from.as_str())
            .or_default()
            .push(edge.to.as_str());
        *in_degree.entry(edge.to.as_str()).or_insert(0) += 1;
    }

    let mut queue: VecDeque<&str> = in_degree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&n, _)| n)
        .collect();
    let mut sorted: Vec<String> = Vec::with_capacity(def.nodes.len());
    let mut seen: HashSet<&str> = HashSet::new();
    while let Some(node) = queue.pop_front() {
        if !seen.insert(node) {
            continue;
        }
        sorted.push(node.to_string());
        if let Some(neighbors) = adjacency.get(node) {
            for &next in neighbors {
                if let Some(d) = in_degree.get_mut(next) {
                    *d -= 1;
                    if *d == 0 {
                        queue.push_back(next);
                    }
                }
            }
        }
    }
    if sorted.len() != def.nodes.len() {
        return Err(CompileError::Cycle {
            sorted: sorted.len(),
            total: def.nodes.len(),
        });
    }
    Ok(sorted)
}

// =============================================================================
// Cache
// =============================================================================

pub struct CachedFlow {
    pub flow: DbFlow,
    pub compiled: Arc<CompiledFlow>,
}

/// Domyślny limit slotu synthetic w `FlowCache`. Każdy unikalny model w
/// produkcji generuje wpis `__synthetic__:<kind>:<model>` — bez capu pamięć
/// rosłaby liniowo z liczbą modelu × kindów (chat/stream/tts/stt/embeddings).
pub const DEFAULT_SYNTHETIC_CACHE_SIZE: usize = 256;

pub struct FlowCache {
    entries: RwLock<HashMap<String, CacheEntry>>,
    ttl: Duration,
    synthetic: RwLock<SyntheticSlot>,
}

struct CacheEntry {
    flow: Option<Arc<CachedFlow>>,
    inserted_at: Instant,
}

/// LRU-bounded slot dla synthetic ad-hoc flows zbudowanych w runtime przez
/// `FlowDispatcher` gdy resolver nie ma user-defined flow dla modelu. Klucze
/// mają format `<kind>:<model>` (kind ∈ chat/chat_stream/tts/stt/embeddings).
struct SyntheticSlot {
    entries: HashMap<String, Arc<CompiledFlow>>,
    /// Kolejność dostępu: front = najstarszy (next-to-evict), back = najnowszy.
    /// Każdy `set`/`get` przesuwa klucz na koniec.
    lru_order: VecDeque<String>,
    max_size: usize,
}

impl SyntheticSlot {
    fn new(max_size: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(max_size.min(64)),
            lru_order: VecDeque::with_capacity(max_size.min(64)),
            max_size,
        }
    }

    fn touch(&mut self, key: &str) {
        if let Some(pos) = self.lru_order.iter().position(|k| k == key) {
            self.lru_order.remove(pos);
        }
        self.lru_order.push_back(key.to_string());
    }

    fn get(&mut self, key: &str) -> Option<Arc<CompiledFlow>> {
        let val = self.entries.get(key).cloned()?;
        self.touch(key);
        Some(val)
    }

    fn set(&mut self, key: String, flow: Arc<CompiledFlow>) {
        if self.entries.contains_key(&key) {
            self.entries.insert(key.clone(), flow);
            self.touch(&key);
            return;
        }
        while self.entries.len() >= self.max_size {
            match self.lru_order.pop_front() {
                Some(oldest) => {
                    self.entries.remove(&oldest);
                }
                None => break,
            }
        }
        self.entries.insert(key.clone(), flow);
        self.lru_order.push_back(key);
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.lru_order.clear();
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

impl FlowCache {
    pub fn new(ttl_secs: u64) -> Self {
        Self::with_synthetic_capacity(ttl_secs, DEFAULT_SYNTHETIC_CACHE_SIZE)
    }

    pub fn with_synthetic_capacity(ttl_secs: u64, synthetic_max: usize) -> Self {
        let cap = synthetic_max.max(1);
        Self {
            entries: RwLock::new(HashMap::new()),
            ttl: Duration::from_secs(ttl_secs),
            synthetic: RwLock::new(SyntheticSlot::new(cap)),
        }
    }

    pub fn get(&self, key: &str) -> Option<Option<Arc<CachedFlow>>> {
        let entries = self.entries.read().ok()?;
        let entry = entries.get(key)?;
        if entry.inserted_at.elapsed() > self.ttl {
            return None;
        }
        Some(entry.flow.clone())
    }

    pub fn set(&self, key: &str, value: Option<Arc<CachedFlow>>) {
        if let Ok(mut entries) = self.entries.write() {
            entries.insert(
                key.to_string(),
                CacheEntry {
                    flow: value,
                    inserted_at: Instant::now(),
                },
            );
        }
    }

    pub fn invalidate(&self, key: &str) {
        if let Ok(mut entries) = self.entries.write() {
            entries.remove(key);
        }
    }

    pub fn invalidate_all(&self) {
        if let Ok(mut entries) = self.entries.write() {
            entries.clear();
        }
        if let Ok(mut synth) = self.synthetic.write() {
            synth.clear();
        }
    }

    /// Pobiera synthetic flow dla pary `<kind>:<model>`. Hit przesuwa klucz na
    /// koniec LRU. Brak TTL — synthetic flowy nie mają „świeżości” jak user
    /// flows; ich invalidacja idzie wyłącznie przez `invalidate_all`.
    pub fn synthetic_get(&self, key: &str) -> Option<Arc<CompiledFlow>> {
        self.synthetic.write().ok()?.get(key)
    }

    /// Zapisuje synthetic flow. Gdy slot przekracza `max_size`, ewicowany jest
    /// najstarszy entry (LRU). Powtarzalny zapis pod ten sam klucz odświeża
    /// pozycję LRU.
    pub fn synthetic_set(&self, key: &str, flow: Arc<CompiledFlow>) {
        if let Ok(mut synth) = self.synthetic.write() {
            synth.set(key.to_string(), flow);
        }
    }

    #[cfg(test)]
    pub fn synthetic_len(&self) -> usize {
        self.synthetic.read().map(|s| s.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapters::{
        ConditionNodeAdapter, LlmNodeAdapter, OutputNodeAdapter, TriggerNodeAdapter,
    };
    use std::sync::Arc;

    fn registry() -> AdapterRegistry {
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register(Arc::new(ConditionNodeAdapter::new()));
        r.register_llm(Arc::new(LlmNodeAdapter::new()));
        r
    }

    #[test]
    fn compile_simple_two_node_flow() {
        let json = r#"{
            "nodes": [
                {"id":"t","type":"trigger","config":{}},
                {"id":"o","type":"output","config":{}}
            ],
            "edges": [{"from":"t","to":"o"}]
        }"#;
        let cf = CompiledFlow::from_json(1, json, &registry(), crate::flow_engine::validation::ValidationSource::UserDefined).unwrap();
        assert_eq!(cf.execution_order.len(), 2);
        assert!(!cf.is_streaming);
        assert_eq!(cf.trigger_run_idx(), Some(0));
    }

    #[test]
    fn compile_detects_streaming_end_shape() {
        let json = r#"{
            "nodes": [
                {"id":"t","type":"trigger","config":{}},
                {"id":"l","type":"llm","config":{"model":"m"}},
                {"id":"o","type":"output","config":{"mode":"stream"}}
            ],
            "edges": [
                {"from":"t","to":"l"},
                {"from":"l","to":"o","from_port":"stream"}
            ]
        }"#;
        let cf = CompiledFlow::from_json(1, json, &registry(), crate::flow_engine::validation::ValidationSource::UserDefined).unwrap();
        assert!(cf.is_streaming);
        assert_eq!(cf.streaming_llm_run_idx(), Some(1));
        // Stage 3d Krok 2c: chain pusty dla direct LLM → output (output
        // jest sink'iem, NIE w chain'ie).
        assert!(cf.streaming_chain_run_idxs().is_empty());
    }

    /// Stage 3d Krok 2c: streaming_chain_run_idxs walks intermediate
    /// streaming-aware nodes po LLM. Test używa pii_filter (rejestrowany
    /// jako StreamingNodeAdapter w lokalnym registry).
    #[test]
    fn compile_streaming_chain_run_idxs_intermediate_node() {
        use crate::flow_engine::node_adapters::PiiFilterNodeAdapter;
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register(Arc::new(ConditionNodeAdapter::new()));
        r.register_streaming(Arc::new(PiiFilterNodeAdapter::new()));
        r.register_llm(Arc::new(LlmNodeAdapter::new()));

        let json = r#"{
            "nodes": [
                {"id":"t","type":"trigger","config":{}},
                {"id":"l","type":"llm","config":{"model":"m"}},
                {"id":"p","type":"pii_filter","config":{}},
                {"id":"o","type":"output","config":{"mode":"stream"}}
            ],
            "edges": [
                {"from":"t","to":"l"},
                {"from":"l","to":"p","from_port":"stream"},
                {"from":"p","to":"o","from_port":"stream"}
            ]
        }"#;
        let cf = CompiledFlow::from_json(
            1,
            json,
            &r,
            crate::flow_engine::validation::ValidationSource::UserDefined,
        )
        .unwrap();
        let chain = cf.streaming_chain_run_idxs();
        assert_eq!(chain.len(), 1);
        // Chain pos to run_idx pii_filter — czyli execution_order[chain[0]]
        // wskazuje na node z node_type=='pii_filter'.
        let def_idx = cf.execution_order[chain[0]];
        assert_eq!(cf.definition.nodes[def_idx].node_type, "pii_filter");
    }

    #[test]
    fn compile_rejects_cycle() {
        // Cycle musi być w segmentcie odłączonym od trigger'a (R4 inaczej
        // zatrzymałby flow na multi-input). Disconnected trigger + para
        // condition→condition w cyklu — validation przepuszcza, topo łapie.
        let json = r#"{
            "nodes": [
                {"id":"t","type":"trigger","config":{}},
                {"id":"a","type":"condition","config":{}},
                {"id":"b","type":"condition","config":{}}
            ],
            "edges": [
                {"from":"a","to":"b","from_port":"true"},
                {"from":"b","to":"a","from_port":"true"}
            ]
        }"#;
        let err = CompiledFlow::from_json(1, json, &registry(), crate::flow_engine::validation::ValidationSource::UserDefined).unwrap_err();
        assert!(matches!(err, CompileError::Cycle { .. }));
    }

    #[test]
    fn compile_rejects_empty_flow() {
        let json = r#"{"nodes":[],"edges":[]}"#;
        let err = CompiledFlow::from_json(1, json, &registry(), crate::flow_engine::validation::ValidationSource::UserDefined).unwrap_err();
        assert!(matches!(err, CompileError::Empty));
    }

    #[test]
    fn cache_roundtrip() {
        let cache = FlowCache::new(60);
        assert!(cache.get("k").is_none());
        cache.set("k", None);
        let neg = cache.get("k").unwrap();
        assert!(neg.is_none());
        cache.invalidate("k");
        assert!(cache.get("k").is_none());
    }

    fn synthetic_compiled() -> Arc<CompiledFlow> {
        let json = r#"{
            "nodes":[
                {"id":"t","type":"trigger","position":{"x":0,"y":0}},
                {"id":"l","type":"llm","position":{"x":1,"y":0}},
                {"id":"o","type":"output","position":{"x":2,"y":0}}
            ],
            "edges":[
                {"from":"t","to":"l"},
                {"from":"l","to":"o"}
            ]
        }"#;
        Arc::new(
            CompiledFlow::from_json(
                42,
                json,
                &registry(),
                crate::flow_engine::validation::ValidationSource::Synthetic,
            )
            .expect("compile"),
        )
    }

    #[test]
    fn synthetic_slot_roundtrip() {
        let cache = FlowCache::new(60);
        assert!(cache.synthetic_get("chat:foo").is_none());
        cache.synthetic_set("chat:foo", synthetic_compiled());
        assert!(cache.synthetic_get("chat:foo").is_some());
        assert_eq!(cache.synthetic_len(), 1);
    }

    #[test]
    fn synthetic_slot_evicts_lru_when_over_cap() {
        let cache = FlowCache::with_synthetic_capacity(60, 3);
        cache.synthetic_set("a", synthetic_compiled());
        cache.synthetic_set("b", synthetic_compiled());
        cache.synthetic_set("c", synthetic_compiled());
        assert_eq!(cache.synthetic_len(), 3);

        // touch "a" żeby został (najnowszy access);
        // "b" jest teraz najstarszy.
        let _ = cache.synthetic_get("a");

        cache.synthetic_set("d", synthetic_compiled());
        assert_eq!(cache.synthetic_len(), 3);
        assert!(cache.synthetic_get("a").is_some(), "a powinien zostać (touch)");
        assert!(cache.synthetic_get("b").is_none(), "b powinien być evicted (LRU)");
        assert!(cache.synthetic_get("c").is_some());
        assert!(cache.synthetic_get("d").is_some());
    }

    #[test]
    fn synthetic_slot_overwrite_preserves_cap() {
        let cache = FlowCache::with_synthetic_capacity(60, 2);
        cache.synthetic_set("k1", synthetic_compiled());
        cache.synthetic_set("k1", synthetic_compiled());
        cache.synthetic_set("k1", synthetic_compiled());
        assert_eq!(cache.synthetic_len(), 1);
    }

    #[test]
    fn invalidate_all_clears_synthetic_too() {
        let cache = FlowCache::new(60);
        cache.synthetic_set("k", synthetic_compiled());
        assert_eq!(cache.synthetic_len(), 1);
        cache.invalidate_all();
        assert_eq!(cache.synthetic_len(), 0);
    }
}
