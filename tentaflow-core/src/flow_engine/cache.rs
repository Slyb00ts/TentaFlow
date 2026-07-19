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
    pub flow_id: String,
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
    /// Inline loop regions resolved at compile time (one per distinct
    /// `FlowNode.region` id). The outer scheduler treats a region as a single
    /// unit entered at `entry_pos` and exiting at `exit_pos`; the executor runs
    /// the region's internal sub-DAG inline. Empty for flows with no region.
    pub regions: Vec<LoopRegion>,
}

/// A compiled inline loop region: a marked subgraph with exactly one entry
/// (target of the back edge AND of the external incoming edge) and one exit
/// (source of the back edge). All positions are indices into
/// `CompiledFlow::execution_order`.
#[derive(Debug, Clone)]
pub struct LoopRegion {
    pub id: String,
    /// Region members in topological order of the internal sub-DAG (after the
    /// `loop_back` edge is removed). `entry_pos` is first, `exit_pos` last.
    pub member_pos: Vec<usize>,
    pub entry_pos: usize,
    pub exit_pos: usize,
    /// Index into `definition.edges` of the region's `loop_back` edge.
    pub back_edge_idx: usize,
    /// Iteration budget, clamped to `LOOP_REGION_MAX_ITERATIONS_CAP`.
    pub max_iterations: u32,
    /// Whether one extra grace iteration runs after the budget is exhausted.
    pub final_pass: bool,
}

/// Default iteration budget for an inline loop region when the entry node's
/// config does not set `loop_max_iterations`.
pub const LOOP_REGION_DEFAULT_MAX_ITERATIONS: u32 = 25;
/// Hard cap on an inline loop region's iteration budget (R11 also enforces it).
pub const LOOP_REGION_MAX_ITERATIONS_CAP: u32 = 100;

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
        flow_id: &str,
        flow_json: &str,
        registry: &AdapterRegistry,
    ) -> Result<Self, CompileError> {
        let definition: FlowDefinition =
            serde_json::from_str(flow_json).map_err(|e| CompileError::Json(e.to_string()))?;
        Self::compile(flow_id, definition, registry)
    }

    pub fn compile(
        flow_id: &str,
        definition: FlowDefinition,
        registry: &AdapterRegistry,
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
        validate(&definition, registry)?;

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
        let regions = build_regions(&definition, &run_idx_by_id, &execution_order)?;
        Ok(Self {
            flow_id: flow_id.to_string(),
            definition: Arc::new(definition),
            execution_order,
            incoming_edges_per_pos,
            run_idx_by_id,
            is_streaming,
            regions,
        })
    }

    /// The loop region whose `entry_pos` equals `pos`, if any. The executor
    /// uses this to enter a region inline when the scheduler reaches its entry.
    pub fn region_at_entry(&self, pos: usize) -> Option<&LoopRegion> {
        self.regions.iter().find(|r| r.entry_pos == pos)
    }

    /// Run-index position of a region's member (non-entry) so the outer
    /// scheduler can recognise and skip it — internal members are driven by the
    /// region runner, never spawned standalone.
    pub fn position_is_region_internal(&self, pos: usize) -> bool {
        self.regions
            .iter()
            .any(|r| r.entry_pos != pos && r.member_pos.contains(&pos))
    }

    /// Contracted producer position for an external consumer: a region is one
    /// unit whose output is stored at its `entry_pos`, so an edge originating at
    /// any region member (notably the exit) reads from the entry slot. Non-member
    /// positions map to themselves. Mirrors the `owner` remap in
    /// `build_dependency_graph` so input resolution and dependency gating agree.
    pub fn contracted_producer_pos(&self, pos: usize) -> usize {
        self.regions
            .iter()
            .find(|r| r.member_pos.contains(&pos))
            .map(|r| r.entry_pos)
            .unwrap_or(pos)
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

    /// §3.11 B — pozycja w execution_order node'a-producenta strumienia:
    /// node, którego krawędź wyjściowa ma `from_port="stream"` ORAZ który ma
    /// zarejestrowanego `StreamProducerAdapter` (LLM pozostaje jednym z nich).
    /// Walidacja streaming end-shape gwarantuje co najwyżej jeden taki node;
    /// brak = `None`. Generalizacja starego `llm`-only slotu — harness
    /// `loop`/`subflow`/addon stream block też mogą tu trafić.
    ///
    /// An inline loop region is also a producer: when the `from_port="stream"`
    /// edge originates at a region's exit node, the region itself is the stream
    /// source (the real tokens come from the `llm` block inside it, but the
    /// region is the contracted unit on the outer graph). The returned position
    /// is then the region's `entry_pos` — the unit slot — so the executor enters
    /// the streaming region runner instead of looking for a node-level producer
    /// adapter on the exit.
    pub fn stream_producer_run_idx(&self, registry: &AdapterRegistry) -> Option<usize> {
        if !self.is_streaming {
            return None;
        }
        for edge in self.definition.edges.iter() {
            if edge.from_port != "stream" {
                continue;
            }
            let Some(&pos) = self.run_idx_by_id.get(edge.from.as_str()) else {
                continue;
            };
            // A stream edge out of a region exit makes the region the producer.
            if let Some(region) = self.regions.iter().find(|r| r.exit_pos == pos) {
                return Some(region.entry_pos);
            }
            let node = &self.definition.nodes[self.execution_order[pos]];
            if registry.is_stream_producer(&node.node_type) {
                return Some(pos);
            }
        }
        None
    }

    /// The inline loop region whose contracted unit produces the stream, if the
    /// stream producer for this flow is a region. `stream_producer_run_idx`
    /// returns the region's `entry_pos`; this resolves that position back to the
    /// region so the executor can drive its streaming runner.
    pub fn stream_producer_region(&self, registry: &AdapterRegistry) -> Option<&LoopRegion> {
        let pos = self.stream_producer_run_idx(registry)?;
        self.regions.iter().find(|r| r.entry_pos == pos)
    }

    /// Stage 3d Krok 2c-2: chain stream nodes po producencie (intermediate
    /// streaming-aware nody między producentem a output sink). Walks `from_port=
    /// "stream"` edges starting from the stream producer, kolejność topologiczna
    /// (execution_order indices). Zatrzymuje się gdy konsument to
    /// `output` node (sink) — output nie jest w chain'ie.
    ///
    /// Przykład: `llm.stream → pii_filter.stream → tts_stream_bridge.full →
    /// output` zwraca `[run_idx(pii_filter), run_idx(tts_stream_bridge)]`.
    pub fn streaming_chain_run_idxs(&self, registry: &AdapterRegistry) -> Vec<usize> {
        let Some(producer_idx) = self.stream_producer_run_idx(registry) else {
            return Vec::new();
        };
        let producer_def_idx = self.execution_order[producer_idx];
        let producer_node_id = self.definition.nodes[producer_def_idx].id.as_str();

        let mut chain: Vec<usize> = Vec::new();
        let mut current_id = producer_node_id.to_string();
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
            let consumer_def_idx = self.definition.nodes.iter().position(|n| n.id == edge.to);
            let Some(consumer_pos) = consumer_def_idx else {
                break;
            };
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
        // The inline loop-region back edge closes a cycle in the graph; it is
        // excluded from the toposort so Kahn does not reject the flow. Region
        // semantics (the actual repeat) are handled by the executor, not the
        // topological order.
        if edge.is_loop_back() {
            continue;
        }
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

/// Resolves inline loop regions from `FlowNode.region` ids and the `loop_back`
/// edges. Structural integrity (single entry/exit, members of one region, no
/// boundary-crossing forward edges) is the responsibility of R11 in
/// `validation.rs`, which runs before compile; this function only reads the
/// already-validated shape, so any inconsistency here is a compiler invariant
/// breach and surfaces as `CompileError::Validation` via the `validate` call.
fn build_regions(
    def: &FlowDefinition,
    run_idx_by_id: &HashMap<String, usize>,
    execution_order: &[usize],
) -> Result<Vec<LoopRegion>, CompileError> {
    // `execution_order` maps run-index → def-index; used to fetch the entry
    // node's config (its run-index is its topological rank).
    use std::collections::BTreeMap;

    // Group member positions per region id (deterministic order via BTreeMap).
    let mut members_by_region: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for node in &def.nodes {
        if let Some(region_id) = node.region.as_deref() {
            if let Some(&pos) = run_idx_by_id.get(node.id.as_str()) {
                members_by_region.entry(region_id).or_default().push(pos);
            }
        }
    }
    if members_by_region.is_empty() {
        return Ok(Vec::new());
    }

    let mut regions = Vec::with_capacity(members_by_region.len());
    for (region_id, mut member_pos) in members_by_region {
        // The single back edge of this region pins entry (its `to`) and exit
        // (its `from`). R11 guarantees exactly one such edge per region.
        let (back_edge_idx, back_edge) = def
            .edges
            .iter()
            .enumerate()
            .find(|(_, e)| {
                e.is_loop_back()
                    && def
                        .nodes
                        .iter()
                        .any(|n| n.id == e.from && n.region.as_deref() == Some(region_id))
            })
            .ok_or_else(|| {
                CompileError::Json(format!("region '{region_id}' has no loop_back edge"))
            })?;
        let entry_pos = *run_idx_by_id.get(back_edge.to.as_str()).ok_or_else(|| {
            CompileError::Json(format!("region '{region_id}' back edge target missing"))
        })?;
        let exit_pos = *run_idx_by_id.get(back_edge.from.as_str()).ok_or_else(|| {
            CompileError::Json(format!("region '{region_id}' back edge source missing"))
        })?;

        // Internal member order: a run-index position IS its topological rank
        // (the global `execution_order`, built with the back edge excluded,
        // already topo-sorts the whole acyclic graph), so sorting member
        // positions ascending yields a valid internal order with `entry_pos`
        // first.
        member_pos.sort_unstable();
        debug_assert_eq!(member_pos.first().copied(), Some(entry_pos));

        let entry_def_idx = execution_order[entry_pos];
        let entry_node = &def.nodes[entry_def_idx];
        let max_iterations = entry_node
            .config
            .get("loop_max_iterations")
            .and_then(|v| v.as_i64())
            .filter(|n| *n > 0)
            .map(|n| n as u32)
            .unwrap_or(LOOP_REGION_DEFAULT_MAX_ITERATIONS)
            .min(LOOP_REGION_MAX_ITERATIONS_CAP);
        let final_pass = entry_node
            .config
            .get("loop_final_pass")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        regions.push(LoopRegion {
            id: region_id.to_string(),
            member_pos,
            entry_pos,
            exit_pos,
            back_edge_idx,
            max_iterations,
            final_pass,
        });
    }
    Ok(regions)
}

// =============================================================================
// Cache
// =============================================================================

pub struct CachedFlow {
    pub flow: DbFlow,
    pub compiled: Arc<CompiledFlow>,
}

pub struct FlowCache {
    entries: RwLock<HashMap<String, CacheEntry>>,
    ttl: Duration,
}

struct CacheEntry {
    flow: Option<Arc<CachedFlow>>,
    inserted_at: Instant,
}

impl FlowCache {
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            ttl: Duration::from_secs(ttl_secs),
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
            "edges": [{"from":"t","to":"o","from_port":"text","to_port":"text"}]
        }"#;
        let cf = CompiledFlow::from_json("1", json, &registry()).unwrap();
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
                {"from":"t","to":"l","from_port":"text"},
                {"from":"l","to":"o","from_port":"stream","to_port":"text"}
            ]
        }"#;
        // R7 streaming end-shape: llm.stream → output(stream).
        let reg = registry();
        let cf = CompiledFlow::from_json("1", json, &reg).unwrap();
        assert!(cf.is_streaming);
        assert_eq!(cf.stream_producer_run_idx(&reg), Some(1));
        // Stage 3d Krok 2c: chain pusty dla direct LLM → output (output
        // jest sink'iem, NIE w chain'ie).
        assert!(cf.streaming_chain_run_idxs(&reg).is_empty());
    }

    /// §3.11 B — producent strumienia jest wykrywany przez slot
    /// `StreamProducerAdapter`, nie po node_type=="llm". Non-LLM producent
    /// (zarejestrowany przez `register_stream_producer`) terminujący w
    /// `output(stream)` jest poprawnie wskazany.
    #[test]
    fn compile_detects_non_llm_stream_producer() {
        use crate::flow_engine::node_adapter::test_support::TestStreamProducer;
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register_stream_producer(Arc::new(TestStreamProducer::new("test_producer")));

        let json = r#"{
            "nodes": [
                {"id":"t","type":"trigger","config":{}},
                {"id":"p","type":"test_producer","config":{}},
                {"id":"o","type":"output","config":{"mode":"stream"}}
            ],
            "edges": [
                {"from":"t","to":"p","from_port":"text","to_port":"in"},
                {"from":"p","to":"o","from_port":"stream","to_port":"text"}
            ]
        }"#;
        let cf = CompiledFlow::from_json("1", json, &r).unwrap();
        assert!(cf.is_streaming);
        let producer = cf.stream_producer_run_idx(&r).expect("producer detected");
        let def_idx = cf.execution_order[producer];
        assert_eq!(cf.definition.nodes[def_idx].node_type, "test_producer");
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
                {"from":"t","to":"l","from_port":"text"},
                {"from":"l","to":"p","from_port":"stream"},
                {"from":"p","to":"o","from_port":"stream","to_port":"text"}
            ]
        }"#;
        let cf = CompiledFlow::from_json("1", json, &r).unwrap();
        let chain = cf.streaming_chain_run_idxs(&r);
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
        let err = CompiledFlow::from_json("1", json, &registry()).unwrap_err();
        assert!(matches!(err, CompileError::Cycle { .. }));
    }

    #[test]
    fn compile_rejects_empty_flow() {
        let json = r#"{"nodes":[],"edges":[]}"#;
        let err = CompiledFlow::from_json("1", json, &registry()).unwrap_err();
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
}
