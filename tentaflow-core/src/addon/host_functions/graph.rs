// ============ File: addon/host_functions/graph.rs — Graph storage host functions (RAG 0.2) ============
//
// Six host functions exposing the embedded CozoDB graph engine (services/graph)
// to addons — ONLY host-shaped, capped primitives (NO raw Datalog surface):
//
//   * graph_upsert_node_v1(collection, node)            — insert/replace a node    (graph.write)
//   * graph_upsert_edge_v1(collection, src,rel,dst,...) — insert/replace an edge   (graph.write)
//   * graph_neighbors_v1(collection, node, dir, rel)    — adjacency traversal      (graph.read)
//   * graph_pagerank_v1(collection, top_n, ...)         — built-in Cozo PageRank   (graph.read)
//   * graph_ppr_v1(collection, seeds, top_n, ...)       — Personalized PageRank    (graph.read)
//   * graph_delete_v1(collection, target)               — node/edge/tombstone      (graph.write)
//
// Every call follows the vector.rs order: get_memory -> check_permission (BEFORE
// decoding) -> read_input_cbor -> validate -> dispatch through `graph_manager(db)`
// -> audit (RiskClass::B) -> write_cbor_capped (PayloadKind::GraphItem, ~256 KiB).
//
// org_id from caller.data().org_id (fallback DEFAULT_ORG_ID); addon_id is the
// caller's instance_id. Every collection MUST be declared in the addon manifest
// under `[[graph_collection]]` — ad-hoc collections are rejected.
//
// DECYZJA (finalny rework B1+B2): surowy `graph_query_v1` (Datalog od addona)
// został USUNIĘTY. Niezaufany Datalog przecieka za każdym razem (compute DoS,
// obejście gramatyki regułami-pomocniczymi, błędny filtr tombstone, alive-leak).
// Addon NIE kształtuje zapytań — dostaje tylko host-budowane prymitywy poniżej.
//
// BUDŻET OBLICZENIOWY ciężkich prymitywów (neighbors/pagerank/ppr — trzymają lock
// i liczą): parametry są CLAMPOWANE do twardych capów hosta (głębokość/iteracje/
// seedy — addon nie kontroluje rozmiaru pracy), a współbieżność jest ograniczona
// globalnym + per-addon licznikiem in-flight (`GraphComputeGuard`): saturacja →
// fail-closed `GraphError::ComputeBusy` (audyt jako odrzucenie), więc addon nie
// odpali N ciężkich obliczeń równolegle i nie wysyci CPU.

#![allow(clippy::too_many_arguments)]

use tentaflow_sdk_spec::{
    FieldValue, GraphDeleteInput, GraphDeleteOutput, GraphDeleteTarget, GraphDirection,
    GraphNeighbor, GraphNeighborsInput, GraphNeighborsOutput, GraphPagerankInput,
    GraphPagerankOutput, GraphPprInput, GraphPprOutput, GraphProp, GraphRankedNode,
    GraphUpsertEdgeInput, GraphUpsertEdgeOutput, GraphUpsertNodeInput, GraphUpsertNodeOutput,
};
use tentaflow_sdk_spec::protocol::graph::GraphNode;

use super::abi_helpers::PayloadKind;
use super::cbor_io::{read_input_cbor, write_cbor_capped};
use super::{audit_log_with_risk, check_permission, get_memory, AddonState, WasmCaller};
use crate::addon::errors::AbiError;
use crate::addon::manifest::GraphCollectionSpec;
use crate::audit::RiskClass;
use crate::services::graph::{GraphComputeGuard, GraphError, GraphManager, NeighborDir};

// =============================================================================
// Permission constants
// =============================================================================

const PERM_GRAPH_READ: &str = "graph.read";
const PERM_GRAPH_WRITE: &str = "graph.write";

// =============================================================================
// Host caps for the compute primitives (addon never controls the work size)
// =============================================================================

/// Twardy cap liczby zwracanych wierszy ciężkiego prymitywu (neighbors/pagerank/
/// ppr `top_n`/`limit`) — addon nie wyciągnie nieograniczonego zbioru jednym
/// wywołaniem.
pub const MAX_RESULT_ROWS: u32 = 2_000;

/// Twardy cap iteracji PageRank/PPR. Addon podaje wartość, host ją clampuje do
/// `1..=MAX_RANK_ITERATIONS` — bez tego addon zażądałby np. 1e9 iteracji i wysycił
/// CPU mimo capa współbieżności.
pub const MAX_RANK_ITERATIONS: u32 = 100;

/// Twardy cap liczby seedów PPR. Każdy seed to dodatkowa masa personalizacji do
/// rozprowadzenia; cap trzyma koszt jednego PPR ograniczonym niezależnie od tego,
/// ile id addon wrzuci.
pub const MAX_PPR_SEEDS: usize = 64;

// Cap współbieżności ciężkich obliczeń grafowych (neighbors/pagerank/ppr) żyje w
// `services::graph::compute_guard` — JEDEN mechanizm współdzielony z węzłem flow
// `graph_search`, żeby addon nie obchodził kontroli DoS, wołając przez flow.
// `GraphComputeGuard` jest importowany na górze; caps (`MAX_*_GRAPH_COMPUTE`)
// re-eksportuje `test_api` wprost z `services::graph`.

// =============================================================================
// Shared helpers
// =============================================================================

fn audit(
    state: &AddonState,
    action: &str,
    collection: Option<&str>,
    result: &str,
    reason: Option<&str>,
) {
    audit_log_with_risk(
        state,
        action,
        Some("graph_collection"),
        collection,
        RiskClass::B,
        None,
        None,
        result,
        reason,
    );
}

fn manager(state: &AddonState) -> &'static std::sync::Arc<GraphManager> {
    crate::services::graph_manager(&state.db)
}

fn org_of(caller: &WasmCaller<'_, AddonState>) -> String {
    caller
        .data()
        .org_id
        .clone()
        .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string())
}

/// Looks up the `[[graph_collection]]` spec by name. Addons MUST declare every
/// collection they touch — this blocks ad-hoc collections at runtime.
fn lookup_collection_spec<'a>(
    state: &'a AddonState,
    collection: &str,
) -> Option<&'a GraphCollectionSpec> {
    state
        .manifest
        .graph_collections
        .iter()
        .find(|c| c.name == collection)
}

/// Structural gate check (mirror of vector's): a gated collection requires a
/// non-empty `gate_claim_id` from the caller. Full policy verification is the
/// vector gate engine's job; graph reuses the structural check here and the
/// query/mutation only proceeds when the gate is satisfied. graph_* host
/// functions take no claim id in their wire shape yet, so a gated collection is
/// hard-denied until the claim plumbing lands — fail-closed by design.
pub fn check_gate(spec: &GraphCollectionSpec) -> Result<(), &'static str> {
    if spec.gate.as_deref().map(|g| !g.is_empty()).unwrap_or(false) {
        return Err("gate_not_satisfied");
    }
    Ok(())
}

/// Translates a `GraphError` into the (AbiError, audit_reason) pair surfaced to
/// the addon. Quotas get dedicated codes so addons can react programmatically.
pub fn map_graph_error(e: &GraphError) -> (AbiError, &'static str) {
    match e {
        GraphError::CollectionNotFound { .. } => (AbiError::NotFound, "collection_not_found"),
        GraphError::CollectionExists { .. } => (AbiError::Conflict, "collection_exists"),
        GraphError::CollectionQuotaExceeded { .. } => {
            (AbiError::QuotaExceeded, "collection_quota_exceeded")
        }
        GraphError::NodeQuotaExceeded { .. } => (AbiError::QuotaExceeded, "node_quota_exceeded"),
        GraphError::EdgeQuotaExceeded { .. } => (AbiError::QuotaExceeded, "edge_quota_exceeded"),
        GraphError::InvalidCollectionName(_) => (AbiError::Operation, "invalid_collection_name"),
        GraphError::Datalog(_) => (AbiError::Operation, "datalog_error"),
        GraphError::ComputeBusy { .. } => (AbiError::QuotaExceeded, "graph_compute_busy"),
        GraphError::Backend(_) => (AbiError::Operation, "graph_backend_error"),
        GraphError::Db(_) => (AbiError::Operation, "graph_db_error"),
        GraphError::Io { .. } => (AbiError::Operation, "graph_io_error"),
    }
}

/// Serializes a property bag into a JSON object string for the backend
/// (`props_json`). Empty bag → `"{}"`.
pub fn props_to_json(props: &[GraphProp]) -> String {
    let map: serde_json::Map<String, serde_json::Value> = props
        .iter()
        .map(|p| (p.name.clone(), field_value_to_json(&p.value)))
        .collect();
    serde_json::Value::Object(map).to_string()
}

/// Serializes optional provenance into a JSON string (`"null"` when absent).
fn provenance_to_json(prov: &Option<tentaflow_sdk_spec::Provenance>) -> String {
    match prov {
        None => "null".to_string(),
        Some(p) => serde_json::to_string(&ProvenanceJson::from(p)).unwrap_or_else(|_| "null".into()),
    }
}

#[derive(serde::Serialize)]
struct ProvenanceJson {
    chunk_id: Option<String>,
    doc_id: Option<String>,
    page: Option<u32>,
    span: Option<(u32, u32)>,
    confidence: Option<f32>,
    extractor_version: Option<String>,
}

impl From<&tentaflow_sdk_spec::Provenance> for ProvenanceJson {
    fn from(p: &tentaflow_sdk_spec::Provenance) -> Self {
        ProvenanceJson {
            chunk_id: p.chunk_id.clone(),
            doc_id: p.doc_id.clone(),
            page: p.page,
            span: p.span,
            confidence: p.confidence,
            extractor_version: p.extractor_version.clone(),
        }
    }
}

fn field_value_to_json(v: &FieldValue) -> serde_json::Value {
    match v {
        FieldValue::Str(s) => serde_json::Value::String(s.clone()),
        FieldValue::Int(i) => serde_json::Value::from(*i),
        FieldValue::Float(f) => serde_json::Value::from(*f),
        FieldValue::Bool(b) => serde_json::Value::Bool(*b),
    }
}

/// Shared preamble for every graph host fn: memory + permission + decode. On any
/// failure it audits and returns the ABI error code (the caller short-circuits).
macro_rules! graph_preamble {
    ($caller:ident, $perm:expr, $action:expr, $ty:ty, $ptr:expr, $len:expr) => {{
        let memory = match get_memory(&mut $caller) {
            Some(m) => m,
            None => return AbiError::Operation.as_i32(),
        };
        if !check_permission($caller.data(), $perm, None) {
            audit($caller.data(), $action, None, "denied", Some("missing_permission"));
            return AbiError::Permission.as_i32();
        }
        let input: $ty =
            match read_input_cbor(&memory, &$caller, $ptr, $len, PayloadKind::GraphItem) {
                Ok(v) => v,
                Err(e) => {
                    audit(
                        $caller.data(),
                        $action,
                        None,
                        "denied",
                        Some(if e == AbiError::PayloadTooLarge {
                            "payload_too_large"
                        } else {
                            "invalid_payload"
                        }),
                    );
                    return e.as_i32();
                }
            };
        (memory, input)
    }};
}

/// Validates the collection name + manifest declaration + gate, returning the
/// spec clone or an audited ABI error.
fn resolve_collection(
    caller: &WasmCaller<'_, AddonState>,
    action: &str,
    collection: &str,
) -> Result<GraphCollectionSpec, i32> {
    if !crate::addon::manifest::graph_collection_name_ok(collection) {
        audit(caller.data(), action, Some(collection), "denied", Some("invalid_collection_name"));
        return Err(AbiError::Operation.as_i32());
    }
    let spec = match lookup_collection_spec(caller.data(), collection) {
        Some(s) => s.clone(),
        None => {
            audit(
                caller.data(),
                action,
                Some(collection),
                "denied",
                Some("collection_not_declared_in_manifest"),
            );
            return Err(AbiError::NotFound.as_i32());
        }
    };
    if let Err(reason) = check_gate(&spec) {
        audit(caller.data(), action, Some(collection), "gate_denied", Some(reason));
        return Err(AbiError::GateNotSatisfied.as_i32());
    }
    Ok(spec)
}

// =============================================================================
// Host function: graph_upsert_node_v1
// =============================================================================

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32. graph.write.
pub fn graph_upsert_node_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let action = "graph.upsert_node";
    let (memory, input) = graph_preamble!(
        caller,
        PERM_GRAPH_WRITE,
        action,
        GraphUpsertNodeInput,
        input_ptr,
        input_len
    );

    if let Err(code) = resolve_collection(&caller, action, &input.collection) {
        return code;
    }
    let node: GraphNode = input.node;
    if node.id.is_empty() {
        audit(caller.data(), action, Some(&input.collection), "denied", Some("empty_node_id"));
        return AbiError::Operation.as_i32();
    }

    let props_json = props_to_json(&node.props);
    let prov_json = provenance_to_json(&node.provenance);
    let addon_id = caller.data().addon_id.clone();
    let org_id = org_of(&caller);
    let mgr = manager(caller.data()).clone();

    let count = match mgr.upsert_node_with_quota(
        &org_id,
        &addon_id,
        &input.collection,
        &node.id,
        &node.label,
        &props_json,
        &prov_json,
    ) {
        Ok(c) => c,
        Err(e) => {
            let (abi, reason) = map_graph_error(&e);
            audit(caller.data(), action, Some(&input.collection), "denied", Some(reason));
            return abi.as_i32();
        }
    };

    audit(caller.data(), action, Some(&input.collection), "ok", None);
    let out = GraphUpsertNodeOutput {
        collection: input.collection,
        id: node.id,
        count,
    };
    write_cbor_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr, PayloadKind::GraphItem)
}

// =============================================================================
// Host function: graph_upsert_edge_v1
// =============================================================================

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32. graph.write.
pub fn graph_upsert_edge_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let action = "graph.upsert_edge";
    let (memory, input) = graph_preamble!(
        caller,
        PERM_GRAPH_WRITE,
        action,
        GraphUpsertEdgeInput,
        input_ptr,
        input_len
    );

    if let Err(code) = resolve_collection(&caller, action, &input.collection) {
        return code;
    }
    if input.src.is_empty() || input.rel.is_empty() || input.dst.is_empty() {
        audit(caller.data(), action, Some(&input.collection), "denied", Some("empty_edge_endpoint"));
        return AbiError::Operation.as_i32();
    }

    let weight = input.weight.unwrap_or(1.0);
    let props_json = props_to_json(&input.props);
    let prov_json = provenance_to_json(&input.provenance);
    let addon_id = caller.data().addon_id.clone();
    let org_id = org_of(&caller);
    let mgr = manager(caller.data()).clone();

    let count = match mgr.upsert_edge_with_quota(
        &org_id,
        &addon_id,
        &input.collection,
        &input.src,
        &input.rel,
        &input.dst,
        weight,
        &props_json,
        &prov_json,
    ) {
        Ok(c) => c,
        Err(e) => {
            let (abi, reason) = map_graph_error(&e);
            audit(caller.data(), action, Some(&input.collection), "denied", Some(reason));
            return abi.as_i32();
        }
    };

    audit(caller.data(), action, Some(&input.collection), "ok", None);
    let out = GraphUpsertEdgeOutput {
        collection: input.collection,
        count,
    };
    write_cbor_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr, PayloadKind::GraphItem)
}

// =============================================================================
// Host function: graph_neighbors_v1
// =============================================================================

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32. graph.read.
pub fn graph_neighbors_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let action = "graph.neighbors";
    let (memory, input) = graph_preamble!(
        caller,
        PERM_GRAPH_READ,
        action,
        GraphNeighborsInput,
        input_ptr,
        input_len
    );

    if let Err(code) = resolve_collection(&caller, action, &input.collection) {
        return code;
    }
    if input.node.is_empty() {
        audit(caller.data(), action, Some(&input.collection), "denied", Some("empty_node"));
        return AbiError::Operation.as_i32();
    }
    // Host-capped limit so a single call can't pull an unbounded adjacency set.
    let limit = input.limit.clamp(1, MAX_RESULT_ROWS);
    let dir = match input.direction {
        GraphDirection::Out => NeighborDir::Out,
        GraphDirection::In => NeighborDir::In,
        GraphDirection::Both => NeighborDir::Both,
    };

    let addon_id = caller.data().addon_id.clone();
    let org_id = org_of(&caller);
    let mgr = manager(caller.data()).clone();

    // Cap współbieżności: neighbors trzyma read-lock kolekcji i liczy trawers —
    // zajmij slot PRZED pracą, fail-closed przy saturacji (guard zwalnia w Drop).
    let _compute = match GraphComputeGuard::acquire(&addon_id) {
        Ok(g) => g,
        Err(e) => {
            let (abi, reason) = map_graph_error(&e);
            audit(caller.data(), action, Some(&input.collection), "denied", Some(reason));
            return abi.as_i32();
        }
    };

    let neighbors = match mgr.neighbors(
        &org_id,
        &addon_id,
        &input.collection,
        &input.node,
        dir,
        input.rel.as_deref(),
        limit,
    ) {
        Ok(n) => n,
        Err(e) => {
            let (abi, reason) = map_graph_error(&e);
            audit(caller.data(), action, Some(&input.collection), "denied", Some(reason));
            return abi.as_i32();
        }
    };

    audit(caller.data(), action, Some(&input.collection), "ok", None);
    let out = GraphNeighborsOutput {
        collection: input.collection,
        neighbors: neighbors
            .into_iter()
            .map(|(id, rel, weight)| GraphNeighbor { id, rel, weight })
            .collect(),
    };
    write_cbor_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr, PayloadKind::GraphItem)
}

// =============================================================================
// Host function: graph_pagerank_v1
// =============================================================================

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32. graph.read.
pub fn graph_pagerank_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let action = "graph.pagerank";
    let (memory, input) = graph_preamble!(
        caller,
        PERM_GRAPH_READ,
        action,
        GraphPagerankInput,
        input_ptr,
        input_len
    );

    if let Err(code) = resolve_collection(&caller, action, &input.collection) {
        return code;
    }
    let top_n = input.top_n.clamp(1, MAX_RESULT_ROWS);
    let damping = input.damping.unwrap_or(0.85).clamp(0.0, 1.0);
    let iterations = input.iterations.unwrap_or(20).clamp(1, MAX_RANK_ITERATIONS);

    let addon_id = caller.data().addon_id.clone();
    let org_id = org_of(&caller);
    let mgr = manager(caller.data()).clone();

    // Cap współbieżności: PageRank trzyma read-lock i liczy iteracyjnie — zajmij
    // slot PRZED pracą (fail-closed przy saturacji, Drop zwalnia).
    let _compute = match GraphComputeGuard::acquire(&addon_id) {
        Ok(g) => g,
        Err(e) => {
            let (abi, reason) = map_graph_error(&e);
            audit(caller.data(), action, Some(&input.collection), "denied", Some(reason));
            return abi.as_i32();
        }
    };

    let ranked = match mgr.pagerank(&org_id, &addon_id, &input.collection, top_n, damping, iterations)
    {
        Ok(r) => r,
        Err(e) => {
            let (abi, reason) = map_graph_error(&e);
            audit(caller.data(), action, Some(&input.collection), "denied", Some(reason));
            return abi.as_i32();
        }
    };

    audit(caller.data(), action, Some(&input.collection), "ok", None);
    let out = GraphPagerankOutput {
        collection: input.collection,
        ranked: ranked
            .into_iter()
            .map(|(id, score)| GraphRankedNode { id, score })
            .collect(),
    };
    write_cbor_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr, PayloadKind::GraphItem)
}

// =============================================================================
// Host function: graph_ppr_v1 (Personalized PageRank over CSR, ppr.rs)
// =============================================================================

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32. graph.read.
pub fn graph_ppr_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let action = "graph.ppr";
    let (memory, input) =
        graph_preamble!(caller, PERM_GRAPH_READ, action, GraphPprInput, input_ptr, input_len);

    if let Err(code) = resolve_collection(&caller, action, &input.collection) {
        return code;
    }
    let top_n = input.top_n.clamp(1, MAX_RESULT_ROWS);
    let damping = input.damping.unwrap_or(0.85).clamp(0.0, 1.0);
    let iterations = input.iterations.unwrap_or(20).clamp(1, MAX_RANK_ITERATIONS);
    // Cap liczby seedów: nie ufamy addonowi co do rozmiaru wektora personalizacji.
    let seeds: Vec<String> = input
        .seeds
        .iter()
        .take(MAX_PPR_SEEDS)
        .map(|s| s.id.clone())
        .collect();

    let addon_id = caller.data().addon_id.clone();
    let org_id = org_of(&caller);
    let mgr = manager(caller.data()).clone();

    // Cap współbieżności: PPR eksportuje CSR pod read-lockiem i liczy iteracyjnie —
    // zajmij slot PRZED pracą (fail-closed przy saturacji, Drop zwalnia).
    let _compute = match GraphComputeGuard::acquire(&addon_id) {
        Ok(g) => g,
        Err(e) => {
            let (abi, reason) = map_graph_error(&e);
            audit(caller.data(), action, Some(&input.collection), "denied", Some(reason));
            return abi.as_i32();
        }
    };

    let ranked = match mgr.ppr(
        &org_id,
        &addon_id,
        &input.collection,
        &seeds,
        top_n,
        damping,
        iterations,
    ) {
        Ok(r) => r,
        Err(e) => {
            let (abi, reason) = map_graph_error(&e);
            audit(caller.data(), action, Some(&input.collection), "denied", Some(reason));
            return abi.as_i32();
        }
    };

    audit(caller.data(), action, Some(&input.collection), "ok", None);
    let out = GraphPprOutput {
        collection: input.collection,
        ranked: ranked
            .into_iter()
            .map(|(id, score)| GraphRankedNode { id, score })
            .collect(),
    };
    write_cbor_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr, PayloadKind::GraphItem)
}

// =============================================================================
// Host function: graph_delete_v1
// =============================================================================

/// ABI: (input_ptr, input_len, out_ptr, out_cap, out_len_ptr) -> i32. graph.write.
pub fn graph_delete_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let action = "graph.delete";
    let (memory, input) =
        graph_preamble!(caller, PERM_GRAPH_WRITE, action, GraphDeleteInput, input_ptr, input_len);

    if let Err(code) = resolve_collection(&caller, action, &input.collection) {
        return code;
    }

    let addon_id = caller.data().addon_id.clone();
    let org_id = org_of(&caller);
    let mgr = manager(caller.data()).clone();

    let result = match &input.target {
        GraphDeleteTarget::Node(id) if id.is_empty() => Err("empty_node_id"),
        GraphDeleteTarget::Tombstone(id) if id.is_empty() => Err("empty_node_id"),
        GraphDeleteTarget::Edge(s, r, d) if s.is_empty() || r.is_empty() || d.is_empty() => {
            Err("empty_edge_endpoint")
        }
        _ => Ok(()),
    };
    if let Err(reason) = result {
        audit(caller.data(), action, Some(&input.collection), "denied", Some(reason));
        return AbiError::Operation.as_i32();
    }

    let dispatched = match &input.target {
        GraphDeleteTarget::Node(id) => {
            mgr.delete_node_in(&org_id, &addon_id, &input.collection, id)
        }
        GraphDeleteTarget::Edge(s, r, d) => {
            mgr.delete_edge_in(&org_id, &addon_id, &input.collection, s, r, d)
        }
        GraphDeleteTarget::Tombstone(id) => {
            mgr.tombstone_node_in(&org_id, &addon_id, &input.collection, id)
        }
    };

    let (removed, node_count, edge_count) = match dispatched {
        Ok(t) => t,
        Err(e) => {
            let (abi, reason) = map_graph_error(&e);
            audit(caller.data(), action, Some(&input.collection), "denied", Some(reason));
            return abi.as_i32();
        }
    };

    audit(caller.data(), action, Some(&input.collection), "ok", None);
    let out = GraphDeleteOutput {
        collection: input.collection,
        removed,
        node_count,
        edge_count,
    };
    write_cbor_capped(&memory, &mut caller, &out, out_ptr, out_cap, out_len_ptr, PayloadKind::GraphItem)
}

// =============================================================================
// Public test surface — invoked from `tests/graph_host_functions.rs`
// =============================================================================

/// Re-exports helpers + caps so integration tests can exercise the host-fn logic
/// (error mapping, prop serialization, gate, compute concurrency cap) without
/// spinning up a wasmtime store.
#[doc(hidden)]
pub mod test_api {
    use super::{GraphComputeGuard, GraphError};

    pub use super::{check_gate, map_graph_error, props_to_json, MAX_PPR_SEEDS, MAX_RANK_ITERATIONS, MAX_RESULT_ROWS};
    pub use crate::services::graph::{MAX_GLOBAL_GRAPH_COMPUTE, MAX_PER_ADDON_GRAPH_COMPUTE};

    /// Nieprzezroczysty uchwyt slotu obliczeń — trzymanie go zajmuje slot, a
    /// `drop` go zwalnia (jak w prawdziwej ścieżce host-fn). Test trzyma N
    /// uchwytów, by zasymulować N równoległych ciężkich wywołań. Pole jest
    /// load-bearing wyłącznie przez `Drop` (zwolnienie slotu), nie odczytywane.
    #[allow(dead_code)]
    pub struct ComputeSlot(GraphComputeGuard);

    /// Próbuje zająć slot obliczeń dla `addon_id`. `Ok` = slot zajęty (trzymaj go
    /// żywym, by zajmował zasób), `Err` = saturacja (fail-closed, jak host-fn).
    pub fn try_acquire_compute(addon_id: &str) -> Result<ComputeSlot, GraphError> {
        GraphComputeGuard::acquire(addon_id).map(ComputeSlot)
    }

    /// Czy błąd to fail-closed odrzucenie z capa współbieżności.
    pub fn is_compute_busy(e: &GraphError) -> bool {
        matches!(e, GraphError::ComputeBusy { .. })
    }
}
