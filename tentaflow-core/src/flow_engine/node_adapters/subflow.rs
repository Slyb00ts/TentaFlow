// ===== File: flow_engine/node_adapters/subflow.rs — SubflowNodeAdapter
// (node_type "subflow", category logic). General-purpose flow composition: runs
// another flow as the body of this one via the shared SubflowRunner. The current
// envelope is the child's trigger input; the child's final envelope is returned
// as this block's output, with the child artifacts re-exported under the
// `subflow.{node_id}.` prefix (artifacts are add-only — the prefix avoids
// colliding with the parent's keys). Recursion is bounded by a depth cap and a
// visited-flow cycle guard that live in ExecutionContext, not envelope.meta, so
// a WASM addon node that rewrites the whole envelope cannot zero them out.
// (Harness §3.5 block 8, §3.5.0, §3.10.) =====

use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{ArtifactProvenance, FlowEnvelope, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::subflow_runner::{SubflowRunnerSlot, MAX_SUBFLOW_DEPTH};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "subflow";

pub struct SubflowNodeAdapter {
    runner: SubflowRunnerSlot,
}

impl SubflowNodeAdapter {
    pub fn new(runner: SubflowRunnerSlot) -> Self {
        Self { runner }
    }

    /// Reads the target flow id from node config. Required — a subflow block
    /// with no `flow_id` is a misconfiguration, so this is a node error rather
    /// than a silent passthrough.
    fn flow_id(node: &FlowNode) -> Result<String> {
        node.config
            .get("flow_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("subflow: missing config 'flow_id'"))
    }

    /// Clamps the configured `timeout_ms` against the parent deadline: a subflow
    /// can shorten its run but never extend the flow's overall deadline (same
    /// contract as the addon block). Absent config leaves the parent deadline
    /// untouched.
    fn clamp_deadline(node: &FlowNode, parent: Option<Instant>) -> Option<Instant> {
        let configured = node
            .config
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .filter(|n| *n > 0)
            .map(|ms| Instant::now() + Duration::from_millis(ms));
        match (parent, configured) {
            (Some(p), Some(c)) => Some(p.min(c)),
            (Some(p), None) => Some(p),
            (None, c) => c,
        }
    }
}

#[async_trait]
impl NodeAdapter for SubflowNodeAdapter {
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
        let flow_id = Self::flow_id(node)?;

        // Depth guard (§3.10): the child would run at depth+1, so a parent
        // already at the cap cannot descend further.
        if ctx.subflow_depth >= MAX_SUBFLOW_DEPTH {
            return Err(anyhow!(
                "subflow: max nesting depth {MAX_SUBFLOW_DEPTH} reached (flow '{flow_id}')"
            ));
        }

        // Cycle guard (§3.10): the flow id is on the visited stack path — this
        // also covers a block referencing its own enclosing flow (self-ref),
        // since the enclosing flow id was pushed by the level above. The UI
        // dynamic_enum cannot yet exclude the current flow id, so the runtime
        // guard is the authoritative self-exclusion (UI gap).
        if ctx.subflow_visited.iter().any(|v| v == &flow_id) {
            return Err(anyhow!(
                "subflow: cycle detected — flow '{flow_id}' already on the call path"
            ));
        }

        let runner = self
            .runner
            .read()
            .clone()
            .ok_or_else(|| anyhow!("subflow: SubflowRunner slot not wired"))?;

        // The current envelope is the child's trigger input. Build a child ctx
        // with the clamped deadline; the runner clones it and rewrites the
        // execution_id / usage_sink / depth / visited.
        let input_envelope: FlowEnvelope = inputs
            .first()
            .map(|i| (*i.envelope).clone())
            .unwrap_or_else(|| (*ctx.initial_envelope).clone());

        let mut child_ctx = ctx.clone();
        child_ctx.deadline = Self::clamp_deadline(node, ctx.deadline);

        let child_final = runner
            .run(&flow_id, input_envelope.clone(), &child_ctx, 1, false)
            .await?;

        // Output = parent input envelope as the base (preserves the parent's
        // artifacts + provenance) with the child's payload / context / variables
        // / meta overlaid, and the child's artifacts re-exported under the
        // `subflow.{node_id}.` prefix so they never collide with parent keys
        // (artifacts are add-only).
        let mut out = input_envelope;
        out.payload = child_final.payload;
        out.context = child_final.context;
        out.variables = child_final.variables;
        out.meta = child_final.meta;

        let now_ms = ctx.clock.now_ms();
        for (key, value) in child_final.artifacts {
            let prefixed = format!("subflow.{}.{}", node.id, key);
            // put_artifact rejects duplicates; the prefix makes a collision
            // impossible in practice, but propagate the error rather than panic
            // if a parent already produced the same prefixed key.
            out.put_artifact(
                prefixed,
                value,
                ArtifactProvenance {
                    producer_node_id: node.id.clone(),
                    producer_node_type: NODE_TYPE.to_string(),
                    timestamp_ms: now_ms,
                },
            )
            .map_err(|e| anyhow!("subflow '{}': re-export artifact: {e}", node.id))?;
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{migrations, DbPool};
    use crate::flow_engine::dispatcher::build_registry_for_test;
    use crate::flow_engine::envelope::FlowValue;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use crate::flow_engine::node_adapter::AdapterRegistry;
    use crate::flow_engine::subflow_runner::SubflowRunner;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(Mutex::new(conn))
    }

    /// Inserts a flow with a fixed id (create_flow mints random UUIDs; tests
    /// need a known id to reference as a subflow body).
    fn insert_flow(pool: &DbPool, id: &str, name: &str, flow_json: &str, status: &str) {
        let conn = pool.lock().unwrap();
        conn.execute(
            "INSERT INTO flows (id, name, service_type, flow_json, status, is_default) \
             VALUES (?1, ?2, NULL, ?3, ?4, 0)",
            rusqlite::params![id, name, flow_json, status],
        )
        .expect("insert flow");
    }

    /// trigger → output passthrough body: the trigger emits the initial
    /// envelope, output echoes it. The Text payload travels trigger.text →
    /// output.text (the typed port names the adapters expose). We assert the
    /// payload survives the round-trip.
    fn passthrough_flow_json() -> String {
        json!({
            "nodes": [
                {"id": "t", "type": "trigger", "config": {}},
                {"id": "o", "type": "output", "config": {"format": "text"}}
            ],
            "edges": [
                {"from": "t", "from_port": "text", "to": "o", "to_port": "text"}
            ]
        })
        .to_string()
    }

    fn registry_and_runner(pool: DbPool) -> (Arc<AdapterRegistry>, SubflowRunnerSlot) {
        let registry = Arc::new(build_registry_for_test());
        let slot: SubflowRunnerSlot = Arc::new(parking_lot::RwLock::new(None));
        let runner = Arc::new(SubflowRunner::new(pool, Arc::downgrade(&registry)));
        *slot.write() = Some(runner);
        (registry, slot)
    }

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "sf1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
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
    async fn runs_child_flow_and_returns_its_result() {
        let pool = db();
        insert_flow(
            &pool,
            "aaaaaaaa-0000-0000-0000-000000000001",
            "child",
            &passthrough_flow_json(),
            "active",
        );
        // keep the registry Arc alive for the duration of the run
        let (_registry, slot) = registry_and_runner(pool.clone());

        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("hello-subflow".into());
        let ctx = stub_ctx();

        let out = SubflowNodeAdapter::new(slot)
            .execute(
                &node(json!({"flow_id": "aaaaaaaa-0000-0000-0000-000000000001"})),
                &[input(env)],
                &ctx,
            )
            .await
            .expect("execute");
        assert_eq!(out.payload.as_text(), Some("hello-subflow"));
    }

    #[tokio::test]
    async fn parent_execution_id_recorded() {
        let pool = db();
        // The parent run row must exist for the child FK to be satisfiable;
        // we only need the parent id value, so insert a real flow + execution.
        insert_flow(
            &pool,
            "bbbbbbbb-0000-0000-0000-000000000001",
            "child",
            &passthrough_flow_json(),
            "active",
        );
        let parent_exec_id = {
            let conn = pool.lock().unwrap();
            conn.execute(
                "INSERT INTO flow_executions (flow_id, status) VALUES \
                 ('bbbbbbbb-0000-0000-0000-000000000001', 'running')",
                [],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        let (_registry, slot) = registry_and_runner(pool.clone());

        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("x".into());
        let mut ctx = stub_ctx();
        // A non-zero parent execution id flows into the child as
        // parent_execution_id.
        ctx.execution_id = parent_exec_id;

        SubflowNodeAdapter::new(slot)
            .execute(
                &node(json!({"flow_id": "bbbbbbbb-0000-0000-0000-000000000001"})),
                &[input(env)],
                &ctx,
            )
            .await
            .expect("execute");

        let conn = pool.lock().unwrap();
        let recorded: Option<i64> = conn
            .query_row(
                "SELECT parent_execution_id FROM flow_executions \
                 WHERE id != ?1 ORDER BY id DESC LIMIT 1",
                rusqlite::params![parent_exec_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(recorded, Some(parent_exec_id));
    }

    #[tokio::test]
    async fn depth_guard_fires_at_cap() {
        let pool = db();
        insert_flow(
            &pool,
            "cccccccc-0000-0000-0000-000000000001",
            "child",
            &passthrough_flow_json(),
            "active",
        );
        let (_registry, slot) = registry_and_runner(pool.clone());

        let mut ctx = stub_ctx();
        ctx.subflow_depth = MAX_SUBFLOW_DEPTH;

        let err = SubflowNodeAdapter::new(slot)
            .execute(
                &node(json!({"flow_id": "cccccccc-0000-0000-0000-000000000001"})),
                &[input(FlowEnvelope::empty())],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("max nesting depth"), "{err}");
    }

    #[tokio::test]
    async fn cycle_guard_fires_on_self_reference() {
        let pool = db();
        insert_flow(
            &pool,
            "dddddddd-0000-0000-0000-000000000001",
            "child",
            &passthrough_flow_json(),
            "active",
        );
        let (_registry, slot) = registry_and_runner(pool.clone());

        let mut ctx = stub_ctx();
        // The flow id is already on the visited path (as if an outer level
        // already entered it) → self/loop reference must be rejected.
        ctx.subflow_visited = Arc::new(vec!["dddddddd-0000-0000-0000-000000000001".into()]);

        let err = SubflowNodeAdapter::new(slot)
            .execute(
                &node(json!({"flow_id": "dddddddd-0000-0000-0000-000000000001"})),
                &[input(FlowEnvelope::empty())],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cycle detected"), "{err}");
    }

    #[tokio::test]
    async fn usage_sink_isolation() {
        let pool = db();
        insert_flow(
            &pool,
            "eeeeeeee-0000-0000-0000-000000000001",
            "child",
            &passthrough_flow_json(),
            "active",
        );
        let (_registry, slot) = registry_and_runner(pool.clone());

        // Record some usage on the parent sink, then run the subflow. The child
        // runs on a fresh sink, so the parent's recorded usage is untouched
        // (not drained/stolen by the nested run).
        let ctx = stub_ctx();
        ctx.usage_sink.record(
            "parent_node",
            crate::flow_engine::envelope::TokenUsage {
                prompt_tokens: 11,
                completion_tokens: 7,
                total_tokens: 18,
            },
        );

        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("x".into());
        SubflowNodeAdapter::new(slot)
            .execute(
                &node(json!({"flow_id": "eeeeeeee-0000-0000-0000-000000000001"})),
                &[input(env)],
                &ctx,
            )
            .await
            .expect("execute");

        let parent_usage = ctx.usage_sink.aggregate();
        assert_eq!(parent_usage.total_tokens, 18);
        assert_eq!(parent_usage.prompt_tokens, 11);
        assert_eq!(parent_usage.completion_tokens, 7);
    }

    #[tokio::test]
    async fn unwired_slot_is_error() {
        let slot: SubflowRunnerSlot = Arc::new(parking_lot::RwLock::new(None));
        let ctx = stub_ctx();
        let err = SubflowNodeAdapter::new(slot)
            .execute(
                &node(json!({"flow_id": "x"})),
                &[input(FlowEnvelope::empty())],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("slot not wired"), "{err}");
    }
}
