// ===== File: flow_engine/node_adapters/map_block.rs — MapNodeAdapter
// (node_type "map", category logic). Dynamic parallelism ("50 tasks at once"):
// evaluates `items` to an array, runs the body flow once per element via the
// shared SubflowRunner with a concurrency cap (JoinSet + semaphore), and
// assembles the per-element results into a `FlowValue::Json` array in input
// order. A static DAG cannot express "N branches known only at runtime", so
// `map` is the engine's equivalent of Airflow/Temporal dynamic task mapping;
// together with `loop` it forms the family of control blocks whose body is
// another flow. Element body runs are light (no per-element flow_executions
// row) and inherit the same depth + visited recursion guard as `subflow`.
// Each element's `item` and `index` are exposed to the body through
// `envelope.meta` so the body can read them with CEL (`meta.item` / `meta.index`).
// Element output variables are merged in element order through the SAME
// per-key conflict policy as combine (error on conflict by default, or a
// declared `variable_merge_policy`), so a variable two elements both write
// resolves deterministically rather than by completion order. (Harness §3.5
// block 2, §3.5.0, §3.10, §3.11 C, §3.12.) =====

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::flow_engine::dispatchers::ProgressEvent;
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::expr::{evaluate, flow_value_to_json, ExprScope};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::node_adapters::variable_merge::{merge_ordered, MergeSource};
use crate::flow_engine::subflow_runner::{SubflowRunner, SubflowRunnerSlot, MAX_SUBFLOW_DEPTH};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "map";

/// Default concurrency when config omits it.
const DEFAULT_CONCURRENCY: usize = 4;

/// Hard cap on concurrency regardless of config — bounds resource use when a
/// model-author sets an unreasonable value.
const MAX_CONCURRENCY: usize = 16;

/// Default `items` expression: the whole payload, expected to be a JSON array.
const DEFAULT_ITEMS: &str = "payload";

/// How a failing element body is handled (§3.5 block 2).
#[derive(Clone, Copy, PartialEq)]
enum ErrorPolicy {
    /// First failure aborts the map and propagates as a node error.
    FailFast,
    /// Failures are collected as `{"error": "..."}` entries in the result.
    Collect,
}

pub struct MapNodeAdapter {
    runner: SubflowRunnerSlot,
}

impl MapNodeAdapter {
    pub fn new(runner: SubflowRunnerSlot) -> Self {
        Self { runner }
    }

    fn body_flow_id(node: &FlowNode) -> Result<String> {
        node.config
            .get("body_flow_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("map: missing config 'body_flow_id'"))
    }

    fn items_expr(node: &FlowNode) -> String {
        node.config
            .get("items")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_ITEMS)
            .to_string()
    }

    fn concurrency(node: &FlowNode) -> usize {
        node.config
            .get("concurrency")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_CONCURRENCY)
            .min(MAX_CONCURRENCY)
    }

    fn error_policy(node: &FlowNode) -> ErrorPolicy {
        match node.config.get("error_policy").and_then(|v| v.as_str()) {
            Some("collect") => ErrorPolicy::Collect,
            _ => ErrorPolicy::FailFast,
        }
    }

    /// Evaluates `items` to a JSON array. A non-array result is a node error —
    /// `map` over a scalar is a misconfiguration the author should see, not a
    /// silent single-element run.
    fn resolve_items(expr: &str, envelope: &FlowEnvelope) -> Result<Vec<Value>> {
        let extras: [(&str, Value); 0] = [];
        let scope = ExprScope {
            vars: &envelope.variables,
            payload: &envelope.payload,
            artifacts: &envelope.artifacts,
            meta: &envelope.meta,
            extras: &extras,
        };
        let value = evaluate(expr, &scope, None).map_err(|e| anyhow!("map items: {e}"))?;
        match value {
            Value::Array(items) => Ok(items),
            other => Err(anyhow!(
                "map items expression must resolve to an array, got {}",
                json_type_name(&other)
            )),
        }
    }
}

/// One element body result paired with its input index, so out-of-order
/// completions reassemble into input order.
struct ElementOutcome {
    index: usize,
    /// The body's final payload as JSON (or an `{"error": ...}` object under
    /// the collect policy).
    payload: Value,
    /// The body's output variables. Indexed by element order (not completion
    /// order) and merged through the same conflict policy as `combine`, so a
    /// variable two elements both write resolves deterministically (error by
    /// default) rather than by whichever task finished first.
    variables: BTreeMap<String, FlowValue>,
    /// `Some` only under fail_fast when the element failed — propagated as the
    /// node error after the JoinSet drains.
    error: Option<String>,
}

/// Runs one element body and projects its result. Owns everything it needs so
/// it can run on a JoinSet task. `base` is the seed envelope cloned per element
/// (carries the parent context / variables); the element value and its index go
/// into `meta` for the body to read via CEL.
async fn run_element(
    runner: Arc<SubflowRunner>,
    body_flow_id: String,
    base: FlowEnvelope,
    item: Value,
    index: usize,
    ctx: ExecutionContext,
    policy: ErrorPolicy,
) -> ElementOutcome {
    let mut element_env = base;
    element_env.payload = FlowValue::Json(item.clone());
    element_env.meta.insert("item".into(), item);
    element_env
        .meta
        .insert("index".into(), Value::from(index as u64));

    // Light-mode body run (§3.5 block 2): the runner skips the per-element
    // flow_executions row.
    match runner
        .run(&body_flow_id, element_env, &ctx, 1, true)
        .await
    {
        Ok(final_env) => {
            let payload = flow_value_to_json(&final_env.payload);
            ElementOutcome {
                index,
                payload,
                variables: final_env.variables,
                error: None,
            }
        }
        Err(e) => match policy {
            ErrorPolicy::Collect => ElementOutcome {
                index,
                payload: serde_json::json!({ "error": e.to_string() }),
                variables: BTreeMap::new(),
                error: None,
            },
            ErrorPolicy::FailFast => ElementOutcome {
                index,
                payload: Value::Null,
                variables: BTreeMap::new(),
                error: Some(e.to_string()),
            },
        },
    }
}

#[async_trait]
impl NodeAdapter for MapNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Any)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let body_flow_id = Self::body_flow_id(node)?;

        // Same recursion guards as `subflow` / `loop` (§3.10).
        if ctx.subflow_depth >= MAX_SUBFLOW_DEPTH {
            return Err(anyhow!(
                "map: max nesting depth {MAX_SUBFLOW_DEPTH} reached (body flow '{body_flow_id}')"
            ));
        }
        if ctx.subflow_visited.iter().any(|v| v == &body_flow_id) {
            return Err(anyhow!(
                "map: cycle detected — body flow '{body_flow_id}' already on the call path"
            ));
        }

        let runner = self
            .runner
            .read()
            .clone()
            .ok_or_else(|| anyhow!("map: SubflowRunner slot not wired"))?;

        let base: FlowEnvelope = inputs
            .first()
            .map(|i| (*i.envelope).clone())
            .unwrap_or_else(|| (*ctx.initial_envelope).clone());

        let items = Self::resolve_items(&Self::items_expr(node), &base)?;
        let total = items.len() as u32;
        let concurrency = Self::concurrency(node);
        let policy = Self::error_policy(node);

        let semaphore = Arc::new(Semaphore::new(concurrency));
        let mut join_set: JoinSet<ElementOutcome> = JoinSet::new();

        for (index, item) in items.into_iter().enumerate() {
            // Honour cancel / deadline before scheduling more elements — an
            // already-cancelled map must not keep spawning bodies. Uses
            // `effective_deadline` so a body that parked in `waiting_user`
            // (ask_user / permission grant) and extended the shared deadline Arc
            // is not aborted by the human-wait time it just added back (§3.13).
            if ctx.cancel_token.is_cancelled()
                || ctx.effective_deadline().is_some_and(|d| Instant::now() >= d)
            {
                join_set.abort_all();
                return Err(anyhow!("map '{}': cancelled before completion", node.id));
            }

            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| anyhow!("map '{}': semaphore closed: {e}", node.id))?;
            let runner = runner.clone();
            let body = body_flow_id.clone();
            let element_base = base.clone();
            let element_ctx = ctx.clone();
            ctx.progress.emit(
                &ctx.progress_scope,
                ProgressEvent::MapElement {
                    node_id: node.id.clone(),
                    index: index as u32,
                    total,
                    status: "started".into(),
                },
            );
            join_set.spawn(async move {
                let outcome =
                    run_element(runner, body, element_base, item, index, element_ctx, policy).await;
                drop(permit);
                outcome
            });
        }

        // Sized to total so out-of-order completions slot back into input order.
        let mut results: Vec<Option<Value>> = vec![None; total as usize];
        // Element variables indexed by element order (not completion order) so
        // the merge is deterministic regardless of which task finishes first.
        let mut element_vars: Vec<Option<BTreeMap<String, FlowValue>>> =
            (0..total as usize).map(|_| None).collect();
        let mut fail_fast_error: Option<String> = None;

        while let Some(joined) = join_set.join_next().await {
            // Cancel / deadline aborts the rest in flight (effective deadline —
            // see the pre-spawn gate above).
            if ctx.cancel_token.is_cancelled()
                || ctx.effective_deadline().is_some_and(|d| Instant::now() >= d)
            {
                join_set.abort_all();
                return Err(anyhow!("map '{}': cancelled mid-flight", node.id));
            }
            let outcome =
                joined.map_err(|e| anyhow!("map '{}': element task join: {e}", node.id))?;
            let status = if outcome.error.is_some() {
                "error"
            } else {
                "ok"
            };
            ctx.progress.emit(
                &ctx.progress_scope,
                ProgressEvent::MapElement {
                    node_id: node.id.clone(),
                    index: outcome.index as u32,
                    total,
                    status: status.into(),
                },
            );
            if let Some(err) = outcome.error {
                // fail_fast: keep the first failure, abort the rest.
                if fail_fast_error.is_none() {
                    fail_fast_error = Some(err);
                }
                join_set.abort_all();
                break;
            }
            element_vars[outcome.index] = Some(outcome.variables);
            results[outcome.index] = Some(outcome.payload);
        }

        if let Some(err) = fail_fast_error {
            return Err(anyhow!("map '{}': element failed: {err}", node.id));
        }

        // Every slot is filled under collect (failures became error objects) and
        // under fail_fast that returned early; here all succeeded.
        let assembled: Vec<Value> = results
            .into_iter()
            .map(|r| r.unwrap_or(Value::Null))
            .collect();

        // Element output variables merged through the SAME conflict policy as
        // combine (§3.12 / §3.5 block 2), in element order: a key two elements
        // both write errors by default, or honours `variable_merge_policy` when
        // the map node declares one. The block's own seed variables come first
        // so an element can override a seed default under last_wins. Scoped so
        // the borrows of `base`/`element_vars` end before `base` is moved out.
        let merged = {
            let mut sources: Vec<MergeSource<'_>> = Vec::with_capacity(total as usize + 1);
            sources.push(MergeSource {
                port: None,
                variables: &base.variables,
            });
            for slot in element_vars.iter().flatten() {
                sources.push(MergeSource {
                    port: None,
                    variables: slot,
                });
            }
            merge_ordered(node, &format!("map node '{}'", node.id), &sources)?
        };

        // Output = the seed envelope with the assembled array as payload and the
        // deterministically merged element variables.
        let mut out = base;
        out.payload = FlowValue::Json(Value::Array(assembled));
        out.variables = merged;
        Ok(out)
    }
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{migrations, DbPool};
    use crate::flow_engine::dispatcher::build_registry_for_test;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use crate::flow_engine::node_adapter::AdapterRegistry;
    use serde_json::json;
    use std::sync::Arc;

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn insert_flow(pool: &DbPool, id: &str, name: &str, flow_json: &str, status: &str) {
        let conn = pool.write().unwrap();
        conn.execute(
            "INSERT INTO flows (id, name, service_type, flow_json, status, is_default) \
             VALUES (?1, ?2, NULL, ?3, ?4, 0)",
            rusqlite::params![id, name, flow_json, status],
        )
        .expect("insert flow");
    }

    /// Body adapter: doubles meta.index if item is a number, or fails when item
    /// equals the configured `fail_on` value. Produces a Json payload
    /// `{"index": i, "doubled": item*2}` so order/concurrency are observable.
    struct DoublerBodyAdapter {
        fail_on: Option<i64>,
    }

    #[async_trait]
    impl NodeAdapter for DoublerBodyAdapter {
        fn node_type(&self) -> &str {
            "map_test_body"
        }
        fn input_ports(&self) -> Vec<PortSpec> {
            vec![PortSpec::new("text", FlowDataType::Text)]
        }
        fn output_ports(&self) -> Vec<PortSpec> {
            vec![PortSpec::new("text", FlowDataType::Text)]
        }
        async fn execute(
            &self,
            _node: &FlowNode,
            inputs: &[NodeInput],
            ctx: &ExecutionContext,
        ) -> Result<FlowEnvelope> {
            let mut env = inputs
                .first()
                .map(|i| (*i.envelope).clone())
                .unwrap_or_else(|| (*ctx.initial_envelope).clone());
            let item = env.meta.get("item").and_then(|v| v.as_i64()).unwrap_or(0);
            let index = env.meta.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
            if Some(item) == self.fail_on {
                return Err(anyhow!("body failed on item {item}"));
            }
            env.payload = FlowValue::Json(json!({"index": index, "doubled": item * 2}));
            Ok(env)
        }
    }

    fn registry_and_runner(
        pool: DbPool,
        fail_on: Option<i64>,
    ) -> (Arc<AdapterRegistry>, SubflowRunnerSlot) {
        let mut registry = build_registry_for_test();
        registry.register(Arc::new(DoublerBodyAdapter { fail_on }));
        let registry = Arc::new(registry);
        let slot: SubflowRunnerSlot = Arc::new(parking_lot::RwLock::new(None));
        let runner = Arc::new(SubflowRunner::new(pool, Arc::downgrade(&registry)));
        *slot.write() = Some(runner);
        (registry, slot)
    }

    /// Body adapter that writes a single output variable `winner` set to the
    /// element value as text — every element writes the SAME key with a
    /// DIFFERENT value, so the map-level variable merge must apply the conflict
    /// policy (error by default, deterministic under last_wins).
    struct VarWriterBodyAdapter;

    #[async_trait]
    impl NodeAdapter for VarWriterBodyAdapter {
        fn node_type(&self) -> &str {
            "map_var_body"
        }
        fn input_ports(&self) -> Vec<PortSpec> {
            vec![PortSpec::new("text", FlowDataType::Text)]
        }
        fn output_ports(&self) -> Vec<PortSpec> {
            vec![PortSpec::new("text", FlowDataType::Text)]
        }
        async fn execute(
            &self,
            _node: &FlowNode,
            inputs: &[NodeInput],
            ctx: &ExecutionContext,
        ) -> Result<FlowEnvelope> {
            let mut env = inputs
                .first()
                .map(|i| (*i.envelope).clone())
                .unwrap_or_else(|| (*ctx.initial_envelope).clone());
            let item = env.meta.get("item").and_then(|v| v.as_i64()).unwrap_or(0);
            env.variables
                .insert("winner".into(), FlowValue::Text(item.to_string()));
            env.payload = FlowValue::Json(json!({ "item": item }));
            Ok(env)
        }
    }

    fn var_registry_and_runner(pool: DbPool) -> (Arc<AdapterRegistry>, SubflowRunnerSlot) {
        let mut registry = build_registry_for_test();
        registry.register(Arc::new(VarWriterBodyAdapter));
        let registry = Arc::new(registry);
        let slot: SubflowRunnerSlot = Arc::new(parking_lot::RwLock::new(None));
        let runner = Arc::new(SubflowRunner::new(pool, Arc::downgrade(&registry)));
        *slot.write() = Some(runner);
        (registry, slot)
    }

    fn var_body_json() -> String {
        json!({
            "nodes": [
                {"id": "t", "type": "trigger", "config": {}},
                {"id": "b", "type": "map_var_body", "config": {}},
                {"id": "o", "type": "output", "config": {"format": "text"}}
            ],
            "edges": [
                {"from": "t", "from_port": "text", "to": "b", "to_port": "text"},
                {"from": "b", "from_port": "text", "to": "o", "to_port": "text"}
            ]
        })
        .to_string()
    }

    fn body_json() -> String {
        json!({
            "nodes": [
                {"id": "t", "type": "trigger", "config": {}},
                {"id": "b", "type": "map_test_body", "config": {}},
                {"id": "o", "type": "output", "config": {"format": "text"}}
            ],
            "edges": [
                {"from": "t", "from_port": "text", "to": "b", "to_port": "text"},
                {"from": "b", "from_port": "text", "to": "o", "to_port": "text"}
            ]
        })
        .to_string()
    }

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "map1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    fn input(env: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "trigger".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    #[tokio::test]
    async fn maps_array_preserving_input_order_with_concurrency() {
        let pool = db();
        let body_id = "aaaa1111-map0-0000-0000-000000000001";
        insert_flow(&pool, body_id, "doubler", &body_json(), "active");
        let (_registry, slot) = registry_and_runner(pool.clone(), None);

        // 50-element array, concurrency 4 → order must still be preserved.
        let items: Vec<i64> = (0..50).collect();
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Json(json!(items));

        let out = MapNodeAdapter::new(slot)
            .execute(
                &node(json!({"body_flow_id": body_id, "concurrency": 4})),
                &[input(env)],
                &stub_ctx(),
            )
            .await
            .expect("execute");

        let arr = match &out.payload {
            FlowValue::Json(Value::Array(a)) => a.clone(),
            other => panic!("expected json array, got {other:?}"),
        };
        assert_eq!(arr.len(), 50);
        for (i, v) in arr.iter().enumerate() {
            assert_eq!(v["index"].as_i64(), Some(i as i64));
            assert_eq!(v["doubled"].as_i64(), Some(i as i64 * 2));
        }
    }

    #[tokio::test]
    async fn fail_fast_propagates_first_error() {
        let pool = db();
        let body_id = "bbbb1111-map0-0000-0000-000000000001";
        insert_flow(&pool, body_id, "doubler", &body_json(), "active");
        // Element with item == 3 fails.
        let (_registry, slot) = registry_and_runner(pool.clone(), Some(3));

        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Json(json!([0, 1, 2, 3, 4]));

        let err = MapNodeAdapter::new(slot)
            .execute(
                &node(json!({"body_flow_id": body_id, "error_policy": "fail_fast"})),
                &[input(env)],
                &stub_ctx(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("element failed"), "{err}");
    }

    #[tokio::test]
    async fn collect_policy_keeps_error_entries_in_order() {
        let pool = db();
        let body_id = "cccc1111-map0-0000-0000-000000000001";
        insert_flow(&pool, body_id, "doubler", &body_json(), "active");
        let (_registry, slot) = registry_and_runner(pool.clone(), Some(2));

        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Json(json!([0, 1, 2, 3]));

        let out = MapNodeAdapter::new(slot)
            .execute(
                &node(json!({"body_flow_id": body_id, "error_policy": "collect"})),
                &[input(env)],
                &stub_ctx(),
            )
            .await
            .expect("execute");

        let arr = match &out.payload {
            FlowValue::Json(Value::Array(a)) => a.clone(),
            other => panic!("expected array, got {other:?}"),
        };
        assert_eq!(arr.len(), 4);
        assert_eq!(arr[0]["doubled"].as_i64(), Some(0));
        assert_eq!(arr[1]["doubled"].as_i64(), Some(2));
        // index 2 failed → error object preserved at its slot.
        assert!(arr[2].get("error").is_some(), "{:?}", arr[2]);
        assert_eq!(arr[3]["doubled"].as_i64(), Some(6));
    }

    #[tokio::test]
    async fn non_array_items_is_node_error() {
        let pool = db();
        let body_id = "dddd1111-map0-0000-0000-000000000001";
        insert_flow(&pool, body_id, "doubler", &body_json(), "active");
        let (_registry, slot) = registry_and_runner(pool.clone(), None);

        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("not-an-array".into());

        let err = MapNodeAdapter::new(slot)
            .execute(
                &node(json!({"body_flow_id": body_id})),
                &[input(env)],
                &stub_ctx(),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("must resolve to an array"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn cancel_aborts_before_completion() {
        let pool = db();
        let body_id = "eeee1111-map0-0000-0000-000000000001";
        insert_flow(&pool, body_id, "doubler", &body_json(), "active");
        let (_registry, slot) = registry_and_runner(pool.clone(), None);

        let ctx = stub_ctx();
        ctx.cancel_token.cancel();

        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Json(json!([0, 1, 2]));

        let err = MapNodeAdapter::new(slot)
            .execute(&node(json!({"body_flow_id": body_id})), &[input(env)], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cancelled"), "{err}");
    }

    #[tokio::test]
    async fn items_expression_selects_nested_array() {
        let pool = db();
        let body_id = "ffff1111-map0-0000-0000-000000000001";
        insert_flow(&pool, body_id, "doubler", &body_json(), "active");
        let (_registry, slot) = registry_and_runner(pool.clone(), None);

        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Json(json!({"batch": [10, 20]}));

        let out = MapNodeAdapter::new(slot)
            .execute(
                &node(json!({"body_flow_id": body_id, "items": "payload.batch"})),
                &[input(env)],
                &stub_ctx(),
            )
            .await
            .expect("execute");
        let arr = match &out.payload {
            FlowValue::Json(Value::Array(a)) => a.clone(),
            other => panic!("expected array, got {other:?}"),
        };
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["doubled"].as_i64(), Some(20));
        assert_eq!(arr[1]["doubled"].as_i64(), Some(40));
    }

    #[tokio::test]
    async fn depth_guard_fires_at_cap() {
        let pool = db();
        let body_id = "0000aaaa-map0-0000-0000-000000000001";
        insert_flow(&pool, body_id, "doubler", &body_json(), "active");
        let (_registry, slot) = registry_and_runner(pool.clone(), None);

        let mut ctx = stub_ctx();
        ctx.subflow_depth = MAX_SUBFLOW_DEPTH;
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Json(json!([1]));
        let err = MapNodeAdapter::new(slot)
            .execute(&node(json!({"body_flow_id": body_id})), &[input(env)], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("max nesting depth"), "{err}");
    }

    #[tokio::test]
    async fn unwired_slot_is_error() {
        let slot: SubflowRunnerSlot = Arc::new(parking_lot::RwLock::new(None));
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Json(json!([1]));
        let err = MapNodeAdapter::new(slot)
            .execute(
                &node(json!({"body_flow_id": "x"})),
                &[input(env)],
                &stub_ctx(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("slot not wired"), "{err}");
    }

    #[tokio::test]
    async fn conflicting_element_variables_without_policy_is_error() {
        let pool = db();
        let body_id = "1111bbbb-map0-0000-0000-000000000001";
        insert_flow(&pool, body_id, "var-writer", &var_body_json(), "active");
        let (_registry, slot) = var_registry_and_runner(pool.clone());

        // Two elements both write `winner` with different values → no policy →
        // deterministic conflict error (not an arbitrary completion-order pick).
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Json(json!([1, 2]));
        let err = MapNodeAdapter::new(slot)
            .execute(
                &node(json!({"body_flow_id": body_id, "concurrency": 4})),
                &[input(env)],
                &stub_ctx(),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("conflicting values for variable 'winner'"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn last_wins_policy_resolves_element_variable_in_element_order() {
        let pool = db();
        let body_id = "2222bbbb-map0-0000-0000-000000000001";
        insert_flow(&pool, body_id, "var-writer", &var_body_json(), "active");
        let (_registry, slot) = var_registry_and_runner(pool.clone());

        // last_wins is keyed to ELEMENT order (not completion order), so the last
        // element's value wins regardless of task scheduling — deterministic.
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Json(json!([10, 20, 30]));
        let out = MapNodeAdapter::new(slot)
            .execute(
                &node(json!({
                    "body_flow_id": body_id,
                    "concurrency": 8,
                    "variable_merge_policy": {"winner": "last_wins"}
                })),
                &[input(env)],
                &stub_ctx(),
            )
            .await
            .expect("execute");
        assert_eq!(
            out.variables.get("winner"),
            Some(&FlowValue::Text("30".into()))
        );
    }
}
