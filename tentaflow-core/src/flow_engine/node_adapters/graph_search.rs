// =============================================================================
// Plik: flow_engine/node_adapters/graph_search.rs
// Opis: GraphSearchNodeAdapter (NODE_TYPE="graph_search") — węzeł flow czytający
//       kontekst z grafu instancji (`ctx.graph` = GraphManager), scoped do
//       (org, addon_instance, collection). Paralela do `vector.rs` (RAG E1.0):
//       tożsamość instancji z `ctx.addon_id` (None → czytelny błąd, zero
//       operacji), org z `ctx.org_id` (fallback DEFAULT_ORG_ID tylko gdy None).
//       Operacje read-only: neighbors / pagerank / ppr. Parametry (iteracje,
//       seedy, limit, top_n) są CLAMPOWANE host-side jak w host_functions/graph.rs
//       — nie ufamy wejściu. Tombstone/alive wyklucza GraphManager (backend), więc
//       nie duplikujemy tu filtrowania. Współbieżność ciężkich ops jest ograniczona
//       TYM SAMYM `GraphComputeGuard` co host-fn (services/graph/compute_guard),
//       żeby flow nie obchodził capa DoS host-fn.
// Przykład: node config {"op":"neighbors"}, payload Json {collection,node_id}.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::services::graph::{GraphComputeGuard, NeighborDir};
use crate::services::org::DEFAULT_ORG_ID;

const NODE_TYPE: &str = "graph_search";

/// Twardy cap iteracji PageRank/PPR — lustro `MAX_RANK_ITERATIONS` z host-fn
/// grafowych. Wejście ponad cap jest CLAMPOWANE (nie odrzucane): graf-retrieval
/// w flow ma dać wynik mimo zbyt agresywnej konfiguracji, a koszt obliczenia i
/// tak jest ograniczony tym capem.
const MAX_RANK_ITERATIONS: u32 = 100;

/// Twardy cap liczby seedów PPR — lustro `MAX_PPR_SEEDS`. Nadmiarowe seedy są
/// obcinane (`take`), bo każdy seed to dodatkowa masa personalizacji.
const MAX_PPR_SEEDS: usize = 64;

/// Twardy cap liczby zwracanych wierszy (neighbors `limit`, pagerank/ppr
/// `top_n`) — lustro `MAX_RESULT_ROWS`. Jedno wywołanie nie wyciągnie
/// nieograniczonego zbioru.
const MAX_RESULT_ROWS: u32 = 2_000;

/// Domyślny damping PageRank/PPR, jak w host-fn (Cozo default 0.85).
const DEFAULT_DAMPING: f64 = 0.85;

/// Domyślna liczba iteracji, jak w host-fn.
const DEFAULT_ITERATIONS: u32 = 20;

/// Domyślny limit/top_n, gdy wejście go nie poda.
const DEFAULT_RESULT_ROWS: u32 = 50;

pub struct GraphSearchNodeAdapter;

impl GraphSearchNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Org-scope: `ctx.org_id` gdy `Some`, w p.p. `DEFAULT_ORG_ID`. Fallback
    /// tylko dla wywołań bez org (lustro vector node).
    fn org_scope(ctx: &ExecutionContext) -> String {
        ctx.org_id
            .clone()
            .unwrap_or_else(|| DEFAULT_ORG_ID.to_string())
    }

    /// Tożsamość instancji addona z kontekstu. `None` → błąd: węzeł grafu nie
    /// wie z której instancji czytać, więc odmawia (zero operacji), zamiast
    /// trafiać w cudzą/domyślną kolekcję.
    fn addon_scope(ctx: &ExecutionContext) -> Result<&str> {
        ctx.addon_id.as_deref().ok_or_else(|| {
            anyhow!(
                "graph_search adapter: brak tożsamości addona (ctx.addon_id=None) — \
                 węzeł graph_search wymaga wywołania flow JAKO MODEL przez addon (RAG E1.0)"
            )
        })
    }

    /// Zajmuje slot współbieżności ciężkiego obliczenia grafowego (RAII). To TEN
    /// SAM guard co host-fn (`services::graph::compute_guard`) — saturacja → błąd
    /// „graph compute busy" (fail-closed). Slot zwalnia się po `drop` guarda
    /// (koniec operacji), także przy panice/błędzie.
    fn acquire_compute(addon: &str) -> Result<GraphComputeGuard> {
        GraphComputeGuard::acquire(addon).map_err(|e| anyhow!("graph_search adapter: {e}"))
    }

    /// Wybiera `op` z node.config (wymagane). `neighbors`|`pagerank`|`ppr`.
    fn pick_op(node: &FlowNode) -> Result<&str> {
        node.config
            .get("op")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow!("graph_search adapter: brak wymaganego 'op' (neighbors|pagerank|ppr) w node.config")
            })
    }

    /// Wyciąga payload Json z envelope. Inne warianty (Text/Embedding/...) są
    /// błędem — graph_search potrzebuje strukturalnego wejścia (collection itd.).
    fn payload_json(envelope: &FlowEnvelope) -> Result<&serde_json::Value> {
        match &envelope.payload {
            FlowValue::Json(obj) => Ok(obj),
            other => Err(anyhow!(
                "graph_search adapter: payload musi być Json, dostał {}",
                other.kind()
            )),
        }
    }

    /// Nazwa kolekcji z payload Json (wymagana). Kolekcja jest częścią wejścia
    /// (nie node.config), bo ten sam węzeł retrievalu obsługuje wiele kolekcji
    /// instancji w jednym flow — lustro host-fn, gdzie collection jest w inpucie.
    fn pick_collection(payload: &serde_json::Value) -> Result<String> {
        payload
            .get("collection")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("graph_search adapter: brak wymaganego 'collection' w payload Json"))
    }

    /// Bezpieczne odczytanie `u32` z pola JSON z CLAMPEM do `1..=max`. Wartość
    /// > u32::MAX nie panikuje (serde_json zwróci u64 — `try_from`), tylko jest
    /// traktowana jako "powyżej capa" i clampowana do `max`. Brak pola → default.
    /// Wartość 0 podnoszona do 1 (clamp dolny). To lustro `.clamp(1, MAX)` z
    /// host-fn, ale na ścieżce u64→u32 bez zawijania (walidacja-przed-rzutowaniem).
    fn clamped_u32(payload: &serde_json::Value, field: &str, default: u32, max: u32) -> u32 {
        match payload.get(field).and_then(|v| v.as_u64()) {
            None => default.clamp(1, max),
            Some(raw) => {
                // u64 ponad u32::MAX → host-side cap (max), bez `as` (anti-wrap).
                let n = u32::try_from(raw).unwrap_or(u32::MAX);
                n.clamp(1, max)
            }
        }
    }

    /// Damping z pola JSON, clamp do `0.0..=1.0`. Brak → default. NaN/inf z
    /// `clamp` byłby pułapką, więc nie-skończone wartości zastępujemy defaultem.
    fn clamped_damping(payload: &serde_json::Value) -> f64 {
        match payload.get("damping").and_then(|v| v.as_f64()) {
            Some(d) if d.is_finite() => d.clamp(0.0, 1.0),
            _ => DEFAULT_DAMPING,
        }
    }

    /// op=neighbors: `{collection, node_id, (rel?, direction?, limit?)}` →
    /// `{op, collection, node_id, neighbors:[{id,rel,weight}]}`.
    fn op_neighbors(
        envelope: &FlowEnvelope,
        ctx: &ExecutionContext,
        out: &mut FlowEnvelope,
    ) -> Result<()> {
        let org = Self::org_scope(ctx);
        let addon = Self::addon_scope(ctx)?;
        let payload = Self::payload_json(envelope)?;
        let collection = Self::pick_collection(payload)?;

        let node_id = payload
            .get("node_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("graph_search adapter: neighbors: brak 'node_id' w payload Json"))?;

        let direction = match payload.get("direction").and_then(|v| v.as_str()) {
            None | Some("out") => NeighborDir::Out,
            Some("in") => NeighborDir::In,
            Some("both") => NeighborDir::Both,
            Some(other) => {
                return Err(anyhow!(
                    "graph_search adapter: neighbors: nieznany 'direction'='{other}' (out|in|both)"
                ))
            }
        };
        let rel = payload
            .get("rel")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());
        let limit = Self::clamped_u32(payload, "limit", DEFAULT_RESULT_ROWS, MAX_RESULT_ROWS);

        // Cap współbieżności: ten sam guard co host-fn — addon nie obejdzie DoS,
        // wołając ciężki trawers przez flow zamiast host-fn (slot zwalnia Drop).
        let _compute = Self::acquire_compute(addon)?;

        let neighbors = ctx
            .graph
            .neighbors(&org, addon, &collection, node_id, direction, rel, limit)
            .map_err(|e| anyhow!("graph_search adapter: neighbors: {e}"))?;

        let neighbors_json: Vec<serde_json::Value> = neighbors
            .into_iter()
            .map(|(id, rel, weight)| {
                serde_json::json!({ "id": id, "rel": rel, "weight": weight })
            })
            .collect();

        out.payload = FlowValue::Json(serde_json::json!({
            "op": "neighbors",
            "collection": collection,
            "node_id": node_id,
            "neighbors": neighbors_json,
        }));
        Ok(())
    }

    /// op=pagerank: `{collection, (top_n?, damping?, iterations?)}` →
    /// `{op, collection, ranked:[{id,score}]}`.
    fn op_pagerank(
        envelope: &FlowEnvelope,
        ctx: &ExecutionContext,
        out: &mut FlowEnvelope,
    ) -> Result<()> {
        let org = Self::org_scope(ctx);
        let addon = Self::addon_scope(ctx)?;
        let payload = Self::payload_json(envelope)?;
        let collection = Self::pick_collection(payload)?;

        let top_n = Self::clamped_u32(payload, "top_n", DEFAULT_RESULT_ROWS, MAX_RESULT_ROWS);
        let damping = Self::clamped_damping(payload);
        let iterations =
            Self::clamped_u32(payload, "iterations", DEFAULT_ITERATIONS, MAX_RANK_ITERATIONS);

        // Cap współbieżności: ten sam guard co host-fn (slot zwalnia Drop).
        let _compute = Self::acquire_compute(addon)?;

        let ranked = ctx
            .graph
            .pagerank(&org, addon, &collection, top_n, damping, iterations)
            .map_err(|e| anyhow!("graph_search adapter: pagerank: {e}"))?;

        out.payload = FlowValue::Json(ranked_to_json("pagerank", &collection, ranked));
        Ok(())
    }

    /// op=ppr: `{collection, seeds:[{id,weight?}], (top_n?, damping?, iterations?)}`
    /// → `{op, collection, ranked:[{id,score}]}`. Klucz seeda to `id` (zgodnie z
    /// SDK/host-fn `GraphSeed`), `node_id` akceptowany jako alias. Seedy obcinane
    /// do MAX_PPR_SEEDS.
    /// Wagi seedów (`weight`, brak => 1.0) PŁYNĄ do `GraphManager::ppr` jako wektor
    /// personalizacji P_init (R6) — sterują rankingiem. Kształt wejścia zgodny z
    /// host-fn `GraphPprInput`.
    fn op_ppr(
        envelope: &FlowEnvelope,
        ctx: &ExecutionContext,
        out: &mut FlowEnvelope,
    ) -> Result<()> {
        let org = Self::org_scope(ctx);
        let addon = Self::addon_scope(ctx)?;
        let payload = Self::payload_json(envelope)?;
        let collection = Self::pick_collection(payload)?;

        let seeds_raw = payload
            .get("seeds")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("graph_search adapter: ppr: brak 'seeds' (tablica) w payload Json"))?;
        if seeds_raw.is_empty() {
            return Err(anyhow!("graph_search adapter: ppr: pusta lista 'seeds'"));
        }

        // Cap liczby seedów host-side (`take`) — nie ufamy wejściu co do rozmiaru
        // wektora personalizacji (lustro MAX_PPR_SEEDS z host-fn).
        let mut seeds: Vec<(String, f64)> = Vec::with_capacity(seeds_raw.len().min(MAX_PPR_SEEDS));
        for (i, item) in seeds_raw.iter().take(MAX_PPR_SEEDS).enumerate() {
            // Klucz seeda to `id` — zgodnie z SDK/host-fn `GraphSeed { id, weight }`
            // (tentaflow-sdk-spec). `node_id` przyjmujemy jako alias, żeby caller
            // kopiujący kształt host-fn nie dostał błędu, ale `id` jest kanoniczne.
            let seed_id = item
                .get("id")
                .or_else(|| item.get("node_id"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("graph_search adapter: ppr: seed[{i}] brak 'id'"))?;
            // Waga seeda steruje personalizacją PPR (R6); brak `weight` => 1.0
            // (kompatybilnie — dawniej wszystkie seedy ważyły tak samo).
            let weight = item.get("weight").and_then(|v| v.as_f64()).unwrap_or(1.0);
            seeds.push((seed_id.to_string(), weight));
        }

        let top_n = Self::clamped_u32(payload, "top_n", DEFAULT_RESULT_ROWS, MAX_RESULT_ROWS);
        let damping = Self::clamped_damping(payload);
        let iterations =
            Self::clamped_u32(payload, "iterations", DEFAULT_ITERATIONS, MAX_RANK_ITERATIONS);

        // Cap współbieżności: ten sam guard co host-fn (slot zwalnia Drop).
        let _compute = Self::acquire_compute(addon)?;

        let ranked = ctx
            .graph
            .ppr(&org, addon, &collection, &seeds, top_n, damping, iterations)
            .map_err(|e| anyhow!("graph_search adapter: ppr: {e}"))?;

        out.payload = FlowValue::Json(ranked_to_json("ppr", &collection, ranked));
        Ok(())
    }
}

impl Default for GraphSearchNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for GraphSearchNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Json)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("graph_search adapter: brak krawędzi wejściowej"))?;
        let envelope = &input.envelope;
        let mut out: FlowEnvelope = (**envelope).clone();

        match Self::pick_op(node)? {
            "neighbors" => Self::op_neighbors(envelope, ctx, &mut out)?,
            "pagerank" => Self::op_pagerank(envelope, ctx, &mut out)?,
            "ppr" => Self::op_ppr(envelope, ctx, &mut out)?,
            other => {
                return Err(anyhow!(
                    "graph_search adapter: nieznane 'op'='{other}' (neighbors|pagerank|ppr)"
                ))
            }
        }
        Ok(out)
    }
}

/// Serializuje ranking `(id, score)` do `Json{op, collection, ranked:[{id,score}]}`.
/// Backend zwraca posortowane malejąco po score (top-N).
fn ranked_to_json(op: &str, collection: &str, ranked: Vec<(String, f64)>) -> serde_json::Value {
    let ranked_json: Vec<serde_json::Value> = ranked
        .into_iter()
        .map(|(id, score)| serde_json::json!({ "id": id, "score": score }))
        .collect();
    serde_json::json!({
        "op": op,
        "collection": collection,
        "ranked": ranked_json,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::{stub_ctx, stub_graph};
    use serde_json::json;
    use std::sync::Arc;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "g1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    fn input(payload: serde_json::Value) -> NodeInput {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Json(payload);
        NodeInput {
            from_node_id: "trigger".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    /// Kontekst addona — `addon_id`/`org_id` Some, wspólny `graph` manager żeby
    /// seed (upsert) i retrieval trafiały w tę samą kolekcję instancji.
    fn addon_ctx(
        addon: &str,
        org: &str,
        graph: Arc<crate::services::graph::GraphManager>,
    ) -> ExecutionContext {
        let mut ctx = stub_ctx();
        ctx.addon_id = Some(addon.to_string());
        ctx.org_id = Some(org.to_string());
        ctx.graph = graph;
        ctx
    }

    /// Seeduje mały graf: a→b, a→c, b→c (rel "links") w danej (org, addon, kolekcja).
    fn seed_triangle(
        graph: &crate::services::graph::GraphManager,
        org: &str,
        addon: &str,
        collection: &str,
    ) {
        for id in ["a", "b", "c"] {
            graph
                .upsert_node_with_quota(org, addon, collection, id, "node", "{}", "null")
                .unwrap();
        }
        for (s, d) in [("a", "b"), ("a", "c"), ("b", "c")] {
            graph
                .upsert_edge_with_quota(org, addon, collection, s, "links", d, 1.0, "{}", "null")
                .unwrap();
        }
    }

    #[tokio::test]
    async fn neighbors_returns_adjacency() {
        let g = stub_graph();
        let ctx = addon_ctx("inst-a", "org-1", g.clone());
        seed_triangle(&g, "org-1", "inst-a", "kg");

        let out = GraphSearchNodeAdapter::new()
            .execute(
                &node(json!({"op": "neighbors"})),
                &[input(json!({"collection": "kg", "node_id": "a", "direction": "out"}))],
                &ctx,
            )
            .await
            .unwrap();

        let neighbors = match &out.payload {
            FlowValue::Json(v) => v.get("neighbors").and_then(|n| n.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        // a→b, a→c (dwóch sąsiadów out).
        let ids: Vec<&str> = neighbors
            .iter()
            .filter_map(|n| n.get("id").and_then(|v| v.as_str()))
            .collect();
        assert_eq!(neighbors.len(), 2, "a ma 2 sąsiadów out, było: {ids:?}");
        assert!(ids.contains(&"b") && ids.contains(&"c"), "było: {ids:?}");
    }

    #[tokio::test]
    async fn ppr_seed_ranks_high() {
        let g = stub_graph();
        let ctx = addon_ctx("inst-a", "org-1", g.clone());
        seed_triangle(&g, "org-1", "inst-a", "kg");

        // Kanoniczny klucz seeda to `id` (lustro SDK `GraphSeed { id, weight }`).
        let out = GraphSearchNodeAdapter::new()
            .execute(
                &node(json!({"op": "ppr"})),
                &[input(json!({
                    "collection": "kg",
                    "seeds": [{"id": "a", "weight": 1.0}],
                    "top_n": 10
                }))],
                &ctx,
            )
            .await
            .unwrap();

        let ranked = match &out.payload {
            FlowValue::Json(v) => v.get("ranked").and_then(|r| r.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        assert!(!ranked.is_empty(), "PPR zwrócił pusty ranking");
        // Seed 'a' musi być w wyniku (PPR personalizowany na 'a' daje mu masę).
        let ids: Vec<&str> = ranked
            .iter()
            .filter_map(|r| r.get("id").and_then(|v| v.as_str()))
            .collect();
        assert!(ids.contains(&"a"), "seed 'a' ma być w rankingu, było: {ids:?}");
    }

    #[tokio::test]
    async fn ppr_all_unknown_seeds_returns_empty_not_global() {
        // Regresja bug 1: op=ppr z JAWNYMI seedami, ktorych ZADEN nie istnieje w
        // grafie -> PUSTY ranking (NIE globalny PageRank). Graf jest niepusty
        // (trojkat a/b/c z krawedziami), wiec gdyby PPR degenerowal do uniform,
        // ranking zwrocilby globalne encje (szum). Personalized PageRank z
        // zerowymi kotwicami = brak wyniku.
        let g = stub_graph();
        let ctx = addon_ctx("inst-a", "org-1", g.clone());
        seed_triangle(&g, "org-1", "inst-a", "kg");

        let out = GraphSearchNodeAdapter::new()
            .execute(
                &node(json!({"op": "ppr"})),
                &[input(json!({
                    "collection": "kg",
                    "seeds": [{"id": "nieistniejaca"}, {"id": "tez-nie-ma"}],
                    "top_n": 10
                }))],
                &ctx,
            )
            .await
            .unwrap();

        let ranked = match &out.payload {
            FlowValue::Json(v) => v.get("ranked").and_then(|r| r.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        assert!(
            ranked.is_empty(),
            "same nieznane seedy -> pusty ranking (nie globalny), bylo: {ranked:?}"
        );
    }

    #[tokio::test]
    async fn pagerank_returns_ranking() {
        let g = stub_graph();
        let ctx = addon_ctx("inst-a", "org-1", g.clone());
        seed_triangle(&g, "org-1", "inst-a", "kg");

        let out = GraphSearchNodeAdapter::new()
            .execute(
                &node(json!({"op": "pagerank"})),
                &[input(json!({"collection": "kg", "top_n": 10}))],
                &ctx,
            )
            .await
            .unwrap();

        let ranked = match &out.payload {
            FlowValue::Json(v) => v.get("ranked").and_then(|r| r.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        assert!(!ranked.is_empty(), "PageRank zwrócił pusty ranking");
        // 'c' ma 2 wchodzące krawędzie (a→c, b→c), więc najwyższy rank.
        let top = ranked[0].get("id").and_then(|v| v.as_str()).unwrap();
        assert_eq!(top, "c", "c (2 in-edges) ma być na szczycie, był: {top}");
    }

    #[tokio::test]
    async fn missing_addon_id_is_error_not_operation() {
        let g = stub_graph();
        seed_triangle(&g, DEFAULT_ORG_ID, "inst-a", "kg");
        let mut ctx = stub_ctx();
        ctx.graph = g;
        // addon_id None (wywołanie nie-addonowe).
        let err = GraphSearchNodeAdapter::new()
            .execute(
                &node(json!({"op": "neighbors"})),
                &[input(json!({"collection": "kg", "node_id": "a"}))],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("brak tożsamości addona"),
            "błąd ma wskazywać brak addon_id, był: {err}"
        );
    }

    #[tokio::test]
    async fn isolation_per_org_addon_collection() {
        // Ten sam manager, trzy konteksty. Izolacja po (org, addon, collection).
        let g = stub_graph();
        let ctx_a = addon_ctx("inst-a", "org-1", g.clone());
        let ctx_b = addon_ctx("inst-b", "org-1", g.clone());
        let ctx_c = addon_ctx("inst-a", "org-2", g.clone());

        // Tylko inst-a/org-1 dostaje trójkąt; pozostałe konteksty mają pustą
        // (nieistniejącą) kolekcję 'kg'.
        seed_triangle(&g, "org-1", "inst-a", "kg");

        let oa = GraphSearchNodeAdapter::new()
            .execute(
                &node(json!({"op": "neighbors"})),
                &[input(json!({"collection": "kg", "node_id": "a", "direction": "out"}))],
                &ctx_a,
            )
            .await
            .unwrap();
        let na = match &oa.payload {
            FlowValue::Json(v) => v.get("neighbors").and_then(|n| n.as_array()).map(|a| a.len()).unwrap(),
            _ => panic!("expected Json"),
        };
        assert_eq!(na, 2, "inst-a/org-1 widzi swój graf");

        // inst-b/org-1 oraz inst-a/org-2 nie mają kolekcji 'kg' → CollectionNotFound.
        for (ctx, label) in [(&ctx_b, "inst-b/org-1"), (&ctx_c, "inst-a/org-2")] {
            let err = GraphSearchNodeAdapter::new()
                .execute(
                    &node(json!({"op": "neighbors"})),
                    &[input(json!({"collection": "kg", "node_id": "a"}))],
                    ctx,
                )
                .await
                .unwrap_err();
            let s = err.to_string().to_lowercase();
            assert!(
                s.contains("not") || s.contains("nie"),
                "{label} nie powinien widzieć cudzej kolekcji, był: {err}"
            );
        }
    }

    #[tokio::test]
    async fn iterations_above_cap_clamped_not_overflow() {
        // Iteracje > MAX_RANK_ITERATIONS (i > u32::MAX) są clampowane, nie
        // panikują / nie zawijają — pagerank dalej zwraca wynik.
        let g = stub_graph();
        let ctx = addon_ctx("inst-a", "org-1", g.clone());
        seed_triangle(&g, "org-1", "inst-a", "kg");

        let out = GraphSearchNodeAdapter::new()
            .execute(
                &node(json!({"op": "pagerank"})),
                &[input(json!({
                    "collection": "kg",
                    "top_n": 99999u64,
                    "iterations": 5_000_000_000u64
                }))],
                &ctx,
            )
            .await
            .unwrap();
        let ranked = match &out.payload {
            FlowValue::Json(v) => v.get("ranked").and_then(|r| r.as_array()).cloned().unwrap(),
            _ => panic!("expected Json"),
        };
        assert!(!ranked.is_empty(), "clampowane parametry dają wynik");
    }

    #[tokio::test]
    async fn ppr_seeds_above_cap_clamped() {
        // Więcej niż MAX_PPR_SEEDS (64) seedów — host-side `take(64)` obcina, brak
        // paniki, PPR zwraca wynik.
        let g = stub_graph();
        let ctx = addon_ctx("inst-a", "org-1", g.clone());
        seed_triangle(&g, "org-1", "inst-a", "kg");

        // 100 seedów (część nieznana — i tak pomijane przez backend).
        let seeds: Vec<serde_json::Value> = (0..100)
            .map(|i| json!({"node_id": format!("seed-{i}")}))
            .chain(std::iter::once(json!({"node_id": "a"})))
            .collect();

        let out = GraphSearchNodeAdapter::new()
            .execute(
                &node(json!({"op": "ppr"})),
                &[input(json!({"collection": "kg", "seeds": seeds, "top_n": 10}))],
                &ctx,
            )
            .await
            .unwrap();
        // Bez paniki/overflow; ranking obecny (seed 'a' znany w grafie).
        match &out.payload {
            FlowValue::Json(v) => assert!(v.get("ranked").and_then(|r| r.as_array()).is_some()),
            _ => panic!("expected Json"),
        }
    }

    #[tokio::test]
    async fn tombstoned_node_excluded_from_neighbors() {
        // Tombstone 'c' → krawędzie do 'c' znikają z retrievalu (filtruje backend).
        let g = stub_graph();
        let ctx = addon_ctx("inst-a", "org-1", g.clone());
        seed_triangle(&g, "org-1", "inst-a", "kg");
        g.tombstone_node_in("org-1", "inst-a", "kg", "c").unwrap();

        let out = GraphSearchNodeAdapter::new()
            .execute(
                &node(json!({"op": "neighbors"})),
                &[input(json!({"collection": "kg", "node_id": "a", "direction": "out"}))],
                &ctx,
            )
            .await
            .unwrap();
        let ids: Vec<String> = match &out.payload {
            FlowValue::Json(v) => v
                .get("neighbors")
                .and_then(|n| n.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.get("id").and_then(|v| v.as_str()).map(str::to_string))
                        .collect()
                })
                .unwrap(),
            _ => panic!("expected Json"),
        };
        // Po tombstone 'c' zostaje tylko a→b.
        assert!(!ids.contains(&"c".to_string()), "tombstone 'c' nie może być w wyniku, było: {ids:?}");
        assert!(ids.contains(&"b".to_string()), "a→b zostaje, było: {ids:?}");
    }

    #[tokio::test]
    async fn ppr_seed_node_id_alias_works() {
        // `node_id` to alias kanonicznego `id` — caller kopiujący stary kształt
        // dalej działa (Bug 2: wcześniej węzeł czytał TYLKO `node_id`).
        let g = stub_graph();
        let ctx = addon_ctx("inst-a", "org-1", g.clone());
        seed_triangle(&g, "org-1", "inst-a", "kg");

        let out = GraphSearchNodeAdapter::new()
            .execute(
                &node(json!({"op": "ppr"})),
                &[input(json!({
                    "collection": "kg",
                    "seeds": [{"node_id": "a"}],
                    "top_n": 10
                }))],
                &ctx,
            )
            .await
            .unwrap();
        let ids: Vec<String> = match &out.payload {
            FlowValue::Json(v) => v
                .get("ranked")
                .and_then(|r| r.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.get("id").and_then(|v| v.as_str()).map(str::to_string))
                        .collect()
                })
                .unwrap(),
            _ => panic!("expected Json"),
        };
        assert!(ids.contains(&"a".to_string()), "seed 'a' (alias node_id) ma być w rankingu, było: {ids:?}");
    }

    #[tokio::test]
    async fn compute_concurrency_cap_fails_closed() {
        // Cap współbieżności: węzeł flow bierze TEN SAM `GraphComputeGuard` co
        // host-fn — dowód, że ciężka op przez flow nie obchodzi capa DoS. Test
        // wysyca PER-ADDON cap (a NIE globalny), bo licznik globalny to static
        // dzielony przez wszystkie równoległe testy w tym binarze — saturacja
        // globalnego głodziłaby siostrzane testy i odwrotnie. Per-addon cap jest
        // lokalny dla `addon_id` własnego węzła, więc deterministyczny i izolowany.
        use crate::services::graph::MAX_PER_ADDON_GRAPH_COMPUTE;

        // Unikalny addon_id dla tego testu — własny licznik per-addon, zero kolizji
        // z addon_id innych testów.
        let addon = "inst-cap-node";
        let g = stub_graph();
        let ctx = addon_ctx(addon, "org-1", g.clone());
        seed_triangle(&g, "org-1", addon, "kg");

        // Zajmij cały per-addon cap tego addona (te same liczniki co host-fn).
        let mut held = Vec::new();
        for _ in 0..MAX_PER_ADDON_GRAPH_COMPUTE {
            held.push(GraphComputeGuard::acquire(addon).expect("slot w obrębie per-addon capa"));
        }

        // Per-addon cap wysycony → ciężka op przez węzeł (ten sam addon) odmawia.
        let err = GraphSearchNodeAdapter::new()
            .execute(
                &node(json!({"op": "ppr"})),
                &[input(json!({"collection": "kg", "seeds": [{"id": "a"}], "top_n": 10}))],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("compute capacity exhausted"),
            "węzeł przy wysyconym per-addon capie ma odmówić (graph compute busy), był: {err}"
        );

        // Zwolnij jeden slot (Drop) → ta sama op przez węzeł znów przechodzi.
        held.pop();
        let out = GraphSearchNodeAdapter::new()
            .execute(
                &node(json!({"op": "ppr"})),
                &[input(json!({"collection": "kg", "seeds": [{"id": "a"}], "top_n": 10}))],
                &ctx,
            )
            .await
            .expect("po zwolnieniu slotu op znów przechodzi");
        match &out.payload {
            FlowValue::Json(v) => assert!(v.get("ranked").and_then(|r| r.as_array()).is_some()),
            _ => panic!("expected Json"),
        }
        drop(held);
    }
}
