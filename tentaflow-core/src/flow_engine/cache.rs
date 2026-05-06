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
    ) -> Result<Self, CompileError> {
        let definition: FlowDefinition = serde_json::from_str(flow_json)
            .map_err(|e| CompileError::Json(e.to_string()))?;
        Self::compile(flow_id, definition, registry)
    }

    pub fn compile(
        flow_id: i64,
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
            "edges": [{"from":"t","to":"o"}]
        }"#;
        let cf = CompiledFlow::from_json(1, json, &registry()).unwrap();
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
        let cf = CompiledFlow::from_json(1, json, &registry()).unwrap();
        assert!(cf.is_streaming);
        assert_eq!(cf.streaming_llm_run_idx(), Some(1));
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
        let err = CompiledFlow::from_json(1, json, &registry()).unwrap_err();
        assert!(matches!(err, CompileError::Cycle { .. }));
    }

    #[test]
    fn compile_rejects_empty_flow() {
        let json = r#"{"nodes":[],"edges":[]}"#;
        let err = CompiledFlow::from_json(1, json, &registry()).unwrap_err();
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
