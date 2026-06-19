// ===== File: flow_engine/node_adapters/agent_block.rs — AgentNodeAdapter
// (node_type "agent", category service). The agent as a Flow Builder block (§0
// requirement). THIN by design: it contains NO loop — the loop is a Flow
// Builder flow. The block sets meta.agent_id and runs the agent's harness flow
// (agents.flow_id, default the seeded "Agent Run" flow) via the shared
// SubflowRunner — exactly what a `subflow` block prefilled with that flow id
// would do. Only the summary returns to the parent (Codex-review pattern): the
// inner loop's full conversation (context.messages, the harness control signals)
// is dropped, the parent envelope keeps its own context, and the result surfaces
// as payload=Text(final) plus meta.agent_run_id / meta.agent_exit_reason. When
// a flow wires the block's `stream` output port, the agent block becomes the
// parent's stream producer and forwards the agent harness flow's stream
// (the harness flow's loop is the producer) token-by-token (§3.11 B). Recursion
// is bounded by the same depth + visited guard as `subflow`, living in
// ExecutionContext. (Harness §3.5 block 6, §3.5.0, §3.10, §3.11 B.) =====

use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::stream::BoxStream;
use serde_json::Value;

use crate::flow_engine::envelope::{EnvelopeDelta, FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{
    ExecutionContext, NodeAdapter, PortSpec, StreamProducerAdapter,
};
use crate::flow_engine::subflow_runner::{SubflowRunnerSlot, MAX_SUBFLOW_DEPTH};
use crate::flow_engine::types::{FlowDataType, FlowNode};

use crate::agents::AgentServiceSlot;

const NODE_TYPE: &str = "agent";

/// Stable id of the seeded "Agent Run" harness flow (§3.8). An agent with no
/// `agents.flow_id` of its own falls back to this flow. Stage D seeds the row
/// with this exact UUID (random-per-node ids would diverge across the fleet,
/// like Default Chat). The runner errors loudly if the row is missing, so a
/// build without the seed cannot silently no-op.
pub const AGENT_RUN_FLOW_ID: &str = "00000000-0000-4000-8000-000000000012";

pub struct AgentNodeAdapter {
    service: AgentServiceSlot,
    runner: SubflowRunnerSlot,
}

impl AgentNodeAdapter {
    pub fn new(service: AgentServiceSlot, runner: SubflowRunnerSlot) -> Self {
        Self { service, runner }
    }

    /// Reads the agent id from node config. Required — an agent block with no
    /// `agent_id` is a misconfiguration.
    fn agent_id(node: &FlowNode) -> Result<String> {
        node.config
            .get("agent_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("agent: missing config 'agent_id'"))
    }

    /// Clamps the harness flow's deadline to `min(agent.timeout_secs, remaining
    /// flow deadline)` (§3.7) — a misbehaving harness must not run past the
    /// agent's configured budget, mirroring how `subflow` clamps to `timeout_ms`.
    /// A non-positive `timeout_secs` leaves the parent deadline untouched.
    fn clamp_deadline(timeout_secs: i64, parent: Option<Instant>) -> Option<Instant> {
        let configured = (timeout_secs > 0)
            .then(|| Instant::now() + Duration::from_secs(timeout_secs as u64));
        match (parent, configured) {
            (Some(p), Some(c)) => Some(p.min(c)),
            (Some(p), None) => Some(p),
            (None, c) => c,
        }
    }

    /// Resolves the agent + runner, runs the recursion guards, and builds the
    /// harness flow's trigger envelope (with `meta.agent_id` stamped) + the
    /// budget-clamped child context — the setup shared by `execute` (blocking)
    /// and `produce_stream` (streaming forward). Returns the resolved harness
    /// flow id, the child input envelope, the runner, and the child ctx.
    async fn prepare(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<(
        String,
        FlowEnvelope,
        std::sync::Arc<crate::flow_engine::subflow_runner::SubflowRunner>,
        ExecutionContext,
    )> {
        let agent_id = Self::agent_id(node)?;

        let service = self
            .service
            .read()
            .clone()
            .ok_or_else(|| anyhow!("agent: AgentService slot not wired"))?;
        let runner = self
            .runner
            .read()
            .clone()
            .ok_or_else(|| anyhow!("agent: SubflowRunner slot not wired"))?;

        let agent = service
            .get_agent(&agent_id)?
            .ok_or_else(|| anyhow!("agent: agent '{agent_id}' not found"))?;
        if !agent.is_enabled {
            return Err(anyhow!("agent: agent '{agent_id}' is disabled"));
        }

        // The agent's own harness flow, falling back to the seeded "Agent Run".
        let flow_id = agent
            .flow_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(AGENT_RUN_FLOW_ID)
            .to_string();

        // Same recursion guards as `subflow` (§3.10): the harness flow runs at
        // depth+1, so a parent already at the cap cannot descend, and the flow
        // id must not be on the call path (covers an agent flow re-entering
        // itself).
        if ctx.subflow_depth >= MAX_SUBFLOW_DEPTH {
            return Err(anyhow!(
                "agent: max nesting depth {MAX_SUBFLOW_DEPTH} reached (agent '{agent_id}')"
            ));
        }
        if ctx.subflow_visited.iter().any(|v| v == &flow_id) {
            return Err(anyhow!(
                "agent: cycle detected — agent flow '{flow_id}' already on the call path"
            ));
        }

        // The incoming envelope seeds the harness flow's trigger. We stamp
        // meta.agent_id (agent_context inside the flow reads it when configured
        // with from_vars) but otherwise let the harness flow own its context.
        let input_envelope: FlowEnvelope = inputs
            .first()
            .map(|i| (*i.envelope).clone())
            .unwrap_or_else(|| (*ctx.initial_envelope).clone());

        let mut child_input = input_envelope;
        child_input
            .meta
            .insert("agent_id".into(), Value::String(agent.id.clone()));

        // Clamp the harness flow's deadline to the agent's own budget (§3.7).
        let mut child_ctx = ctx.clone();
        child_ctx.deadline = Self::clamp_deadline(agent.timeout_secs, ctx.deadline);

        Ok((flow_id, child_input, runner, child_ctx))
    }
}

#[async_trait]
impl NodeAdapter for AgentNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        // `text` is the blocking final answer; `stream` (§3.11 B) forwards the
        // agent harness flow's stream (the harness loop is the producer). A flow
        // wiring `stream` makes this block the parent's stream producer (R7).
        vec![
            PortSpec::new("text", FlowDataType::Text),
            PortSpec::new("stream", FlowDataType::Text),
        ]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let (flow_id, child_input, runner, child_ctx) = self.prepare(node, inputs, ctx).await?;

        let child_final = runner.run(&flow_id, child_input, &child_ctx, 1, false).await?;

        // Codex-review pattern: only the summary returns. The parent keeps its
        // OWN envelope as the base (its context, variables, artifacts) and the
        // agent block surfaces just the final answer as a fresh Text payload —
        // the harness loop's full conversation (context.messages) and internal
        // control signals (harness_done, loop_*) do NOT leak upward.
        let final_text = child_final.payload.as_text().unwrap_or("").to_string();
        let agent_run_id = child_final
            .meta
            .get("agent_run_id")
            .cloned()
            .unwrap_or(Value::Null);
        let exit_reason = child_final
            .meta
            .get("harness_exit_reason")
            .or_else(|| child_final.meta.get("loop_exit_reason"))
            .cloned()
            .unwrap_or_else(|| Value::String("final_response".into()));

        let mut out = inputs
            .first()
            .map(|i| (*i.envelope).clone())
            .unwrap_or_else(|| (*ctx.initial_envelope).clone());
        out.payload = FlowValue::Text(final_text);
        out.meta.insert("agent_run_id".into(), agent_run_id);
        out.meta.insert("agent_exit_reason".into(), exit_reason);

        Ok(out)
    }
}

/// §3.11 B — when a flow wires this block's `stream` output port, the agent
/// block is the parent's stream producer: it forwards the agent harness flow's
/// stream (the harness flow's `loop` is the producer) token-by-token. The
/// harness flow runs via `SubflowRunner::run_streaming`; a harness flow without
/// a streaming end-shape falls back to one terminal delta. The executor's
/// producer finalizer aggregates the forwarded deltas into the parent outcome,
/// so the harness flow's own outcome receiver is dropped here. Unlike the
/// blocking `execute`, the streaming path forwards the harness deltas directly —
/// the per-iteration conversation never enters the parent envelope because the
/// deltas only carry the final answer text the harness loop streams out.
#[async_trait]
impl StreamProducerAdapter for AgentNodeAdapter {
    async fn produce_stream(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<BoxStream<'static, Result<EnvelopeDelta>>> {
        let (flow_id, child_input, runner, child_ctx) = self.prepare(node, inputs, ctx).await?;
        let exec = runner
            .run_streaming(&flow_id, child_input, &child_ctx, 1, false)
            .await?;
        Ok(exec.stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentService;
    use crate::db::models::AgentParams;
    use crate::db::{migrations, DbPool};
    use crate::flow_engine::dispatcher::build_registry_for_test;
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

    fn insert_flow(pool: &DbPool, id: &str, name: &str, flow_json: &str, status: &str) {
        let conn = pool.write().unwrap();
        conn.execute(
            "INSERT INTO flows (id, name, service_type, flow_json, status, is_default) \
             VALUES (?1, ?2, NULL, ?3, ?4, 0)",
            rusqlite::params![id, name, flow_json, status],
        )
        .expect("insert flow");
    }

    #[allow(clippy::too_many_arguments)]
    fn seed_agent(pool: &DbPool, id: &str, name: &str, flow_id: Option<&str>, enabled: bool) {
        repository_upsert_agent(pool, id, name, flow_id, enabled);
    }

    fn repository_upsert_agent(
        pool: &DbPool,
        id: &str,
        name: &str,
        flow_id: Option<&str>,
        enabled: bool,
    ) {
        crate::db::repository::upsert_agent(
            pool,
            &AgentParams {
                id,
                name,
                display_name: None,
                description: "test agent",
                system_prompt: Some("sp"),
                model: Some("test-model"),
                tools_json: "[]",
                skills_json: "{}",
                params_json: "{}",
                max_iterations: 5,
                timeout_secs: 600,
                max_subagents: 0,
                max_spawn_depth: 1,
                flow_id,
                routable: true,
                is_enabled: enabled,
                on_child_complete: "notify",
                actor_user_id: None,
            },
        )
        .expect("seed agent");
    }

    /// The harness flow stand-in: a body that drops a synthetic conversation
    /// message and produces a final text answer + agent_run_id in meta. Modeled
    /// with a tiny custom adapter so we exercise the subflow→summary contract
    /// without seeding the real three-flow harness.
    struct FakeHarnessAdapter;

    #[async_trait]
    impl NodeAdapter for FakeHarnessAdapter {
        fn node_type(&self) -> &str {
            "fake_harness"
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
            // Simulate the inner loop's leaked conversation + control signals.
            env.context
                .messages
                .push(crate::flow_engine::envelope::ChatMessage::assistant(
                    "internal turn",
                ));
            env.meta.insert("harness_done".into(), Value::Bool(true));
            env.meta
                .insert("agent_run_id".into(), Value::String("run-xyz".into()));
            env.meta.insert(
                "harness_exit_reason".into(),
                Value::String("final_response".into()),
            );
            env.payload = FlowValue::Text("the final answer".into());
            Ok(env)
        }
    }

    fn harness_flow_json() -> String {
        json!({
            "nodes": [
                {"id": "t", "type": "trigger", "config": {}},
                {"id": "h", "type": "fake_harness", "config": {}},
                {"id": "o", "type": "output", "config": {"format": "text"}}
            ],
            "edges": [
                {"from": "t", "from_port": "text", "to": "h", "to_port": "text"},
                {"from": "h", "from_port": "text", "to": "o", "to_port": "text"}
            ]
        })
        .to_string()
    }

    fn service(pool: DbPool) -> AgentServiceSlot {
        let cipher = Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]));
        let addon_manager =
            Arc::new(crate::addon::AddonManager::new(pool.clone(), cipher).expect("addon manager"));
        let svc = Arc::new(AgentService::new(pool, addon_manager));
        Arc::new(parking_lot::RwLock::new(Some(svc)))
    }

    fn registry_and_runner(pool: DbPool) -> (Arc<AdapterRegistry>, SubflowRunnerSlot) {
        let mut registry = build_registry_for_test();
        registry.register(Arc::new(FakeHarnessAdapter));
        let registry = Arc::new(registry);
        let slot: SubflowRunnerSlot = Arc::new(parking_lot::RwLock::new(None));
        let runner = Arc::new(SubflowRunner::new(pool, Arc::downgrade(&registry)));
        *slot.write() = Some(runner);
        (registry, slot)
    }

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "ag1".into(),
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
    async fn runs_agent_flow_and_returns_summary_only() {
        let pool = db();
        let flow_id = "aaaaaaaa-agnt-0000-0000-000000000001";
        insert_flow(&pool, flow_id, "agent-run", &harness_flow_json(), "active");
        seed_agent(&pool, "agent-1", "researcher", Some(flow_id), true);
        let svc = service(pool.clone());
        let (_registry, runner) = registry_and_runner(pool.clone());

        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("do the task".into());
        // The parent has its own conversation message that must survive.
        env.context
            .messages
            .push(crate::flow_engine::envelope::ChatMessage::user(
                "parent msg",
            ));
        let ctx = stub_ctx();

        let out = AgentNodeAdapter::new(svc, runner)
            .execute(&node(json!({"agent_id": "agent-1"})), &[input(env)], &ctx)
            .await
            .expect("execute");

        // Only the summary returns: the final answer as a Text payload.
        assert_eq!(out.payload.as_text(), Some("the final answer"));
        assert_eq!(
            out.meta.get("agent_run_id").and_then(|v| v.as_str()),
            Some("run-xyz")
        );
        assert_eq!(
            out.meta.get("agent_exit_reason").and_then(|v| v.as_str()),
            Some("final_response")
        );
        // The inner loop's conversation must NOT leak into the parent envelope.
        assert_eq!(out.context.messages.len(), 1);
        assert_eq!(out.context.messages[0].text(), Some("parent msg"));
        // Internal control signals must not leak either.
        assert!(out.meta.get("harness_done").is_none());
    }

    #[tokio::test]
    async fn falls_back_to_default_agent_run_flow_when_agent_flow_null() {
        let pool = db();
        // Seed the default Agent Run flow under the stable id.
        insert_flow(
            &pool,
            AGENT_RUN_FLOW_ID,
            "Agent Run",
            &harness_flow_json(),
            "active",
        );
        seed_agent(&pool, "agent-2", "general", None, true);
        let svc = service(pool.clone());
        let (_registry, runner) = registry_and_runner(pool.clone());

        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("x".into());
        let ctx = stub_ctx();

        let out = AgentNodeAdapter::new(svc, runner)
            .execute(&node(json!({"agent_id": "agent-2"})), &[input(env)], &ctx)
            .await
            .expect("execute");
        assert_eq!(out.payload.as_text(), Some("the final answer"));
    }

    #[tokio::test]
    async fn disabled_agent_is_error() {
        let pool = db();
        seed_agent(&pool, "agent-3", "off", None, false);
        let svc = service(pool.clone());
        let (_registry, runner) = registry_and_runner(pool.clone());
        let ctx = stub_ctx();

        let err = AgentNodeAdapter::new(svc, runner)
            .execute(
                &node(json!({"agent_id": "agent-3"})),
                &[input(FlowEnvelope::empty())],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("disabled"), "{err}");
    }

    #[tokio::test]
    async fn missing_agent_is_error() {
        let pool = db();
        let svc = service(pool.clone());
        let (_registry, runner) = registry_and_runner(pool.clone());
        let ctx = stub_ctx();

        let err = AgentNodeAdapter::new(svc, runner)
            .execute(
                &node(json!({"agent_id": "nope"})),
                &[input(FlowEnvelope::empty())],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    fn clamp_deadline_picks_the_earlier_bound() {
        let now = Instant::now();
        // No parent deadline → the agent budget alone applies.
        let only_agent = AgentNodeAdapter::clamp_deadline(600, None).expect("some");
        assert!(only_agent > now);
        // A short agent budget wins over a far parent deadline.
        let far_parent = now + Duration::from_secs(3600);
        let clamped = AgentNodeAdapter::clamp_deadline(5, Some(far_parent)).expect("some");
        assert!(clamped < far_parent);
        // A long agent budget never extends a nearer parent deadline.
        let near_parent = now + Duration::from_secs(1);
        let kept = AgentNodeAdapter::clamp_deadline(3600, Some(near_parent)).expect("some");
        assert_eq!(kept, near_parent);
        // A non-positive budget leaves the parent deadline untouched.
        assert_eq!(
            AgentNodeAdapter::clamp_deadline(0, Some(near_parent)),
            Some(near_parent)
        );
    }

    #[tokio::test]
    async fn missing_config_agent_id_is_error() {
        let pool = db();
        let svc = service(pool.clone());
        let (_registry, runner) = registry_and_runner(pool.clone());
        let ctx = stub_ctx();
        let err = AgentNodeAdapter::new(svc, runner)
            .execute(&node(json!({})), &[input(FlowEnvelope::empty())], &ctx)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("missing config 'agent_id'"),
            "{err}"
        );
    }

    /// Streaming harness stand-in: a node that is the producer of a streaming
    /// harness flow. Models the harness loop's streamed final answer.
    struct StreamingHarnessAdapter;

    #[async_trait]
    impl NodeAdapter for StreamingHarnessAdapter {
        fn node_type(&self) -> &str {
            "fake_harness_stream"
        }
        fn input_ports(&self) -> Vec<PortSpec> {
            vec![PortSpec::new("in", FlowDataType::Text)]
        }
        fn output_ports(&self) -> Vec<PortSpec> {
            vec![
                PortSpec::new("stream", FlowDataType::Text),
                PortSpec::new("full", FlowDataType::Text),
            ]
        }
        async fn execute(
            &self,
            _node: &FlowNode,
            inputs: &[NodeInput],
            ctx: &ExecutionContext,
        ) -> Result<FlowEnvelope> {
            Ok(inputs
                .first()
                .map(|i| (*i.envelope).clone())
                .unwrap_or_else(|| (*ctx.initial_envelope).clone()))
        }
    }

    #[async_trait]
    impl StreamProducerAdapter for StreamingHarnessAdapter {
        async fn produce_stream(
            &self,
            _node: &FlowNode,
            inputs: &[NodeInput],
            _ctx: &ExecutionContext,
        ) -> Result<BoxStream<'static, Result<EnvelopeDelta>>> {
            use crate::flow_engine::envelope::{FinishReason, LlmStreamChunk};
            use futures::StreamExt;
            // Assert the agent block stamped meta.agent_id into the trigger seed.
            let agent_id = inputs
                .first()
                .and_then(|i| i.envelope.meta.get("agent_id").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();
            let first = LlmStreamChunk {
                text_delta: format!("streamed answer for {agent_id}"),
                ..Default::default()
            };
            let last = LlmStreamChunk {
                finish_reason: Some(FinishReason::Stop),
                ..Default::default()
            };
            Ok(futures::stream::iter(vec![
                Ok(EnvelopeDelta::Llm(first)),
                Ok(EnvelopeDelta::Llm(last)),
            ])
            .boxed())
        }
    }

    fn streaming_harness_flow_json() -> String {
        json!({
            "nodes": [
                {"id": "t", "type": "trigger", "config": {}},
                {"id": "h", "type": "fake_harness_stream", "config": {}},
                {"id": "o", "type": "output", "config": {"mode": "stream"}}
            ],
            "edges": [
                {"from": "t", "from_port": "text", "to": "h", "to_port": "in"},
                {"from": "h", "from_port": "stream", "to": "o", "to_port": "text"}
            ]
        })
        .to_string()
    }

    /// §3.11 B — `agent.produce_stream` forwards the agent harness flow's stream.
    /// The harness flow's producer streams the final answer; the agent block
    /// pipes those deltas straight out.
    #[tokio::test]
    async fn produce_stream_forwards_harness_flow_stream() {
        use crate::flow_engine::envelope::FinishReason;
        use futures::StreamExt;

        let pool = db();
        let flow_id = "eeee0000-agnt-strm-0000-000000000001";
        insert_flow(&pool, flow_id, "agent-run", &streaming_harness_flow_json(), "active");
        seed_agent(&pool, "agent-stream", "streamer", Some(flow_id), true);
        let svc = service(pool.clone());

        let mut registry = build_registry_for_test();
        registry.register_stream_producer(Arc::new(StreamingHarnessAdapter));
        let registry = Arc::new(registry);
        let slot: SubflowRunnerSlot = Arc::new(parking_lot::RwLock::new(None));
        let runner = Arc::new(SubflowRunner::new(pool.clone(), Arc::downgrade(&registry)));
        *slot.write() = Some(runner);

        let stream = AgentNodeAdapter::new(svc, slot)
            .produce_stream(
                &node(json!({"agent_id": "agent-stream"})),
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
        // The harness flow producer saw the agent id the block stamped.
        assert!(text.contains("streamed answer for agent-stream"), "stream text: {text:?}");
        assert!(saw_finish, "client never got finish_reason=Stop");
    }

    /// A disabled agent never streams — `produce_stream` errors at the guard.
    #[tokio::test]
    async fn produce_stream_disabled_agent_is_error() {
        let pool = db();
        seed_agent(&pool, "agent-off-stream", "off", None, false);
        let svc = service(pool.clone());
        let (_registry, runner) = registry_and_runner(pool.clone());

        let result = AgentNodeAdapter::new(svc, runner)
            .produce_stream(
                &node(json!({"agent_id": "agent-off-stream"})),
                &[input(FlowEnvelope::empty())],
                &stub_ctx(),
            )
            .await;
        let err = match result {
            Ok(_) => panic!("disabled agent must not produce a stream"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("disabled"), "{err}");
    }
}
