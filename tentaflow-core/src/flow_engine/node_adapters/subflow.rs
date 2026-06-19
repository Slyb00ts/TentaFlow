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

use futures::stream::BoxStream;

use crate::flow_engine::envelope::{ArtifactProvenance, EnvelopeDelta, FlowEnvelope, NodeInput};
use crate::flow_engine::node_adapter::{
    ExecutionContext, NodeAdapter, PortSpec, StreamProducerAdapter,
};
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

    /// Resolves the runner, runs the recursion guards, and builds the child
    /// trigger envelope + clamped child context — the setup shared by `execute`
    /// (blocking) and `produce_stream` (streaming forward). Returns the resolved
    /// flow id, the child input envelope, the runner handle, and the child ctx.
    fn prepare<'a>(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &'a ExecutionContext,
    ) -> Result<(
        String,
        FlowEnvelope,
        std::sync::Arc<crate::flow_engine::subflow_runner::SubflowRunner>,
        ExecutionContext,
    )> {
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
        // since the enclosing flow id was pushed by the level above.
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

        let input_envelope: FlowEnvelope = inputs
            .first()
            .map(|i| (*i.envelope).clone())
            .unwrap_or_else(|| (*ctx.initial_envelope).clone());

        let mut child_ctx = ctx.clone();
        child_ctx.deadline = Self::clamp_deadline(node, ctx.deadline);

        Ok((flow_id, input_envelope, runner, child_ctx))
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
        // `stream` (§3.11 B) forwards the child flow's stream producer output;
        // `full` is the blocking child result. A flow wiring `stream` makes this
        // block the parent's stream producer (R7); `full` keeps the blocking
        // composition path.
        vec![
            PortSpec::new("stream", FlowDataType::Any),
            PortSpec::new("full", FlowDataType::Any),
        ]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        // The current envelope is the child's trigger input. The runner clones
        // the child ctx and rewrites the execution_id / usage_sink / depth /
        // visited; guards + clamped deadline are applied in `prepare`.
        let (flow_id, input_envelope, runner, child_ctx) = self.prepare(node, inputs, ctx)?;

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

/// §3.11 B — when a flow wires this block's `stream` output port, the subflow is
/// the parent's stream producer: it forwards the child flow's own stream
/// producer output token-by-token. The child runs in streaming mode via
/// `SubflowRunner::run_streaming`; a child without a streaming end-shape falls
/// back to one terminal delta (the runner wraps the blocking result), so a
/// `stream`-wired subflow always streams. The executor's producer finalizer
/// aggregates the forwarded deltas into the parent outcome, so the child's own
/// outcome receiver is dropped here.
#[async_trait]
impl StreamProducerAdapter for SubflowNodeAdapter {
    async fn produce_stream(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<BoxStream<'static, Result<EnvelopeDelta>>> {
        let (flow_id, input_envelope, runner, child_ctx) = self.prepare(node, inputs, ctx)?;
        let exec = runner
            .run_streaming(&flow_id, input_envelope, &child_ctx, 1, false)
            .await?;
        Ok(exec.stream)
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
    use std::sync::Arc;

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    /// Inserts a flow with a fixed id (create_flow mints random UUIDs; tests
    /// need a known id to reference as a subflow body).
    fn insert_flow(pool: &DbPool, id: &str, name: &str, flow_json: &str, status: &str) {
        let conn = pool.write().unwrap();
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
            let conn = pool.write().unwrap();
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

        let conn = pool.read().unwrap();
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

    /// Streaming child flow: trigger → test_producer → output(stream). The
    /// `TestStreamProducer` (test_support) emits a fixed two-chunk delta stream.
    fn streaming_child_json() -> String {
        json!({
            "nodes": [
                {"id": "t", "type": "trigger", "config": {}},
                {"id": "p", "type": "test_producer", "config": {}},
                {"id": "o", "type": "output", "config": {"mode": "stream"}}
            ],
            "edges": [
                {"from": "t", "from_port": "text", "to": "p", "to_port": "in"},
                {"from": "p", "from_port": "stream", "to": "o", "to_port": "text"}
            ]
        })
        .to_string()
    }

    /// §3.11 B — `subflow.produce_stream` forwards the child flow's stream
    /// producer output token-by-token (the child runs in streaming mode).
    #[tokio::test]
    async fn produce_stream_forwards_child_stream() {
        use crate::flow_engine::envelope::{EnvelopeDelta, FinishReason};
        use crate::flow_engine::node_adapter::test_support::TestStreamProducer;
        use futures::StreamExt;

        let pool = db();
        let child_id = "ffff0000-subf-strm-0000-000000000001";
        insert_flow(&pool, child_id, "stream-child", &streaming_child_json(), "active");

        // Registry with the streaming TestStreamProducer registered so the child
        // flow's output(stream) end-shape validates and produces a stream.
        let mut registry = build_registry_for_test();
        registry.register_stream_producer(Arc::new(TestStreamProducer::new("test_producer")));
        let registry = Arc::new(registry);
        let slot: SubflowRunnerSlot = Arc::new(parking_lot::RwLock::new(None));
        let runner = Arc::new(SubflowRunner::new(pool.clone(), Arc::downgrade(&registry)));
        *slot.write() = Some(runner);

        let stream = SubflowNodeAdapter::new(slot)
            .produce_stream(
                &node(json!({"flow_id": child_id})),
                &[input(FlowEnvelope::empty())],
                &stub_ctx(),
            )
            .await
            .expect("produce_stream");

        let mut text = String::new();
        let mut saw_finish = false;
        let mut s = stream;
        while let Some(item) = s.next().await {
            if let EnvelopeDelta::Llm(c) = item.expect("delta ok") {
                text.push_str(&c.text_delta);
                if c.finish_reason == Some(FinishReason::Stop) {
                    saw_finish = true;
                }
            }
        }
        assert!(text.contains("hello from test producer"), "stream text: {text:?}");
        assert!(saw_finish, "client never got finish_reason=Stop");
    }

    /// A non-streaming child flow still produces a stream via `produce_stream` —
    /// the runner wraps the blocking result as one terminal delta.
    #[tokio::test]
    async fn produce_stream_wraps_non_streaming_child() {
        use crate::flow_engine::envelope::EnvelopeDelta;
        use futures::StreamExt;

        let pool = db();
        let child_id = "ffff0000-subf-strm-0000-000000000002";
        insert_flow(&pool, child_id, "blocking-child", &passthrough_flow_json(), "active");
        let (_registry, slot) = registry_and_runner(pool.clone());

        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("blocking-text".into());

        let stream = SubflowNodeAdapter::new(slot)
            .produce_stream(&node(json!({"flow_id": child_id})), &[input(env)], &stub_ctx())
            .await
            .expect("produce_stream");

        let mut text = String::new();
        let mut saw_finish = false;
        let mut s = stream;
        while let Some(item) = s.next().await {
            if let EnvelopeDelta::Llm(c) = item.expect("delta ok") {
                text.push_str(&c.text_delta);
                if c.finish_reason.is_some() {
                    saw_finish = true;
                }
            }
        }
        assert_eq!(text, "blocking-text");
        assert!(saw_finish, "wrapped blocking result must carry a terminal finish_reason");
    }
}
