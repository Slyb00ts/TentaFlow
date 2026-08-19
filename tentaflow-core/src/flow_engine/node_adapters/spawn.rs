// ===== File: flow_engine/node_adapters/spawn.rs — SpawnNodeAdapter
// (node_type "spawn", category logic, 1-in/1-out). Deterministic, graph-driven
// counterpart to the `core.agent_spawn` tool: a flow node delegates a sub-agent
// in the background without the model deciding to. It builds the same CallerRun
// the harness loop uses and calls AgentRunManager::handle_agent_spawn — the
// child run is detached (the manager owns a tokio task), so the handler returns
// the `run_ids` immediately. Those ids are written to a flow variable so a later
// `await_subagents` / `subagent_status` block can wait on or poll them. The
// payload passes through unchanged. (Harness §3.3.) =====

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agents::{AgentPrincipal, AgentServiceSlot, CallerRun};
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::expr::{evaluate, ExprScope};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "spawn";
const DEFAULT_OUTPUT_VARIABLE: &str = "spawned_run_ids";

pub struct SpawnNodeAdapter {
    service: AgentServiceSlot,
}

impl SpawnNodeAdapter {
    pub fn new(service: AgentServiceSlot) -> Self {
        Self { service }
    }

    /// Resolves the sub-agent's NAME (the key `handle_agent_spawn` resolves a
    /// run from). `agent_name` is taken verbatim; an `agent_id` is translated to
    /// its name through the agent service so the Flow Builder can pin an agent by
    /// id while the manager keeps resolving by name like the tool path.
    fn resolve_agent_name(&self, node: &FlowNode) -> Result<String> {
        if let Some(name) = node
            .config
            .get("agent_name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
        {
            return Ok(name.to_string());
        }
        let agent_id = node
            .config
            .get("agent_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "spawn node '{}': 'agent_id' or 'agent_name' is required",
                    node.id
                )
            })?;
        let service = self
            .service
            .read()
            .clone()
            .ok_or_else(|| anyhow!("spawn: AgentService slot not wired"))?;
        service
            .get_agent(agent_id)?
            .map(|a| a.name)
            .ok_or_else(|| anyhow!("spawn: agent '{agent_id}' not found"))
    }

    /// Resolves the task text. `task` is CEL-interpolable over the envelope
    /// (payload/vars/meta), mirroring `ask_user`'s question handling: a value
    /// expression is evaluated, a plain string is taken verbatim. An empty task
    /// is a node error — there is nothing to delegate.
    fn resolve_task(node: &FlowNode, envelope: &FlowEnvelope) -> Result<String> {
        let raw = node
            .config
            .get("task")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("spawn node '{}': 'task' is required", node.id))?;
        Ok(Self::interpolate(raw, envelope))
    }

    /// Optional extra context prepended to the task by the manager. CEL-interpolable
    /// like the task; absent/empty yields `None`.
    fn resolve_context(node: &FlowNode, envelope: &FlowEnvelope) -> Option<String> {
        node.config
            .get("context")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|raw| Self::interpolate(raw, envelope))
    }

    /// CEL interpolation over the envelope scope; a parse/eval error falls back to
    /// the literal config (a plain string is not a valid CEL expression and must
    /// still be used as-is).
    fn interpolate(raw: &str, envelope: &FlowEnvelope) -> String {
        let extras: [(&str, Value); 0] = [];
        let scope = ExprScope {
            vars: &envelope.variables,
            payload: &envelope.payload,
            artifacts: &envelope.artifacts,
            meta: &envelope.meta,
            extras: &extras,
        };
        match evaluate(raw, &scope, None) {
            Ok(Value::String(s)) => s,
            Ok(other) => other.to_string(),
            Err(_) => raw.to_string(),
        }
    }

    fn output_variable(node: &FlowNode) -> String {
        node.config
            .get("output_variable")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_OUTPUT_VARIABLE)
            .to_string()
    }

    /// Core spawn step against an explicit manager — the path `execute` takes
    /// once it has resolved the process-global manager. Split out so a test can
    /// drive it with a manager bound to its own in-memory db (the global is
    /// set-once and would otherwise leak the first test's pool across the binary).
    async fn spawn_with(
        &self,
        manager: &crate::agents::AgentRunManager,
        node: &FlowNode,
        envelope: &FlowEnvelope,
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let agent_name = self.resolve_agent_name(node)?;
        let task = Self::resolve_task(node, envelope)?;
        let context = Self::resolve_context(node, envelope);
        let output_variable = Self::output_variable(node);

        let principal = AgentPrincipal::new(ctx.user_id.clone(), None);
        let caller = CallerRun::from_envelope(envelope, principal, ctx.session_id.clone());
        if caller.run_id.is_empty() {
            return Err(anyhow!(
                "spawn: requires a managed run context (place after agent_context)"
            ));
        }

        let mut args = json!({ "agent_name": agent_name, "task": task });
        if let Some(context) = context {
            args["context"] = Value::String(context);
        }
        let outcome = manager.handle_agent_spawn(&caller, &args).await?;
        let run_ids = outcome.get("run_ids").cloned().unwrap_or_else(|| json!([]));

        let mut out: FlowEnvelope = envelope.clone();
        out.variables
            .insert(output_variable, FlowValue::Json(run_ids));
        Ok(out)
    }
}

#[async_trait]
impl NodeAdapter for SpawnNodeAdapter {
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
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("spawn: missing input edge"))?;
        let envelope = &input.envelope;

        // The manager owns the background tokio task — without it the deterministic
        // delegation cannot run (headless / unwired), which is a node error, not a
        // silent pass-through (the flow author asked to spawn work).
        let manager = crate::agents::agent_run_manager_global()
            .ok_or_else(|| anyhow!("spawn: agent run manager not available on this node"))?;

        self.spawn_with(&manager, node, envelope, ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentRunManager, BackgroundFlowRunner, RunStatus};
    use crate::db::migrations;
    use crate::db::models::AgentParams;
    use crate::db::repository;
    use crate::db::DbPool;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use crate::flow_engine::progress_broker::ProgressBroker;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio_util::sync::CancellationToken;

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn service(pool: DbPool) -> AgentServiceSlot {
        let cipher = Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]));
        let addon_manager =
            Arc::new(crate::addon::AddonManager::new(pool.clone(), cipher).expect("addon manager"));
        let svc = Arc::new(crate::agents::AgentService::new(pool, addon_manager));
        Arc::new(parking_lot::RwLock::new(Some(svc)))
    }

    /// Runner that completes instantly — a spawned child settles to `completed`
    /// without a live flow dispatcher.
    struct InstantRunner;
    #[async_trait]
    impl BackgroundFlowRunner for InstantRunner {
        async fn run_agent_flow(
            &self,
            _flow_id: String,
            _initial: FlowEnvelope,
            _principal: AgentPrincipal,
            _deadline: Option<Instant>,
            _cancel: CancellationToken,
            _progress: Arc<dyn crate::flow_engine::dispatchers::ProgressSink>,
            scope: String,
        ) -> Result<crate::agents::run_manager::AgentFlowOutcome> {
            Ok(crate::agents::run_manager::AgentFlowOutcome {
                text: format!("done-{scope}"),
                usage: crate::flow_engine::envelope::TokenUsage::default(),
                model: None,
            })
        }
    }

    fn manager(pool: DbPool) -> Arc<AgentRunManager> {
        let mgr = Arc::new(AgentRunManager::new(
            pool,
            Arc::new(InstantRunner),
            Arc::new(ProgressBroker::new()),
            8,
        ));
        mgr.attach_self();
        mgr
    }

    fn seed_agent(pool: &DbPool, id: &str, name: &str, max_subagents: i64) {
        repository::upsert_agent(
            pool,
            &AgentParams {
                id,
                name,
                display_name: None,
                description: "d",
                system_prompt: None,
                model: None,
                tools_json: "[]",
                skills_json: "{}",
                params_json: "{}",
                max_iterations: 5,
                timeout_secs: 600,
                max_subagents,
                max_spawn_depth: 2,
                flow_id: Some("11111111-0000-4000-8000-000000000099"),
                routable: true,
                is_enabled: true,
                on_child_complete: "notify",
                allowed_agents_json: None,
                actor_user_id: None,
            },
        )
        .expect("seed agent");
    }

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "sp1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    /// Creates a parent run row so the deterministic spawn block has a managed
    /// caller context (mirrors how agent_context primes a run). Returns the
    /// parent run id.
    async fn parent_run(mgr: &AgentRunManager, agent_id: &str) -> String {
        mgr.spawn(
            agent_id,
            "lead",
            None,
            &AgentPrincipal::user("u1"),
            &[],
            &[],
            None,
            None,
        )
        .await
        .expect("spawn parent")
    }

    #[tokio::test]
    async fn spawn_creates_child_and_writes_run_ids() {
        let pool = db();
        seed_agent(&pool, "parent", "boss", 4);
        seed_agent(&pool, "worker", "worker", 0);
        let mgr = manager(pool.clone());

        let parent = parent_run(&mgr, "parent").await;

        let mut env = FlowEnvelope::empty();
        env.meta.insert("agent_id".into(), json!("parent"));
        env.meta
            .insert("agent_run_id".into(), json!(parent.clone()));

        let out = SpawnNodeAdapter::new(service(pool.clone()))
            .spawn_with(
                &mgr,
                &node(json!({"agent_name": "worker", "task": "do a subtask"})),
                &env,
                &stub_ctx(),
            )
            .await
            .expect("execute");

        let run_ids = match out.variables.get("spawned_run_ids") {
            Some(FlowValue::Json(v)) => v.as_array().cloned().expect("array"),
            other => panic!("expected json array, got {other:?}"),
        };
        assert_eq!(run_ids.len(), 1, "one child spawned");
        let child_id = run_ids[0].as_str().expect("child id string");

        // The child run exists in the DB as a row parented to `parent`.
        let row = repository::get_agent_run(&pool, child_id)
            .expect("get")
            .expect("child row");
        assert_eq!(row.parent_run_id.as_deref(), Some(parent.as_str()));

        // The InstantRunner settles it to completed.
        for _ in 0..200 {
            let r = repository::get_agent_run(&pool, child_id).unwrap().unwrap();
            if RunStatus::Completed.as_str() == r.status {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("child run never completed");
    }

    #[tokio::test]
    async fn spawn_resolves_agent_by_id() {
        let pool = db();
        seed_agent(&pool, "parent2", "boss2", 4);
        seed_agent(&pool, "worker-id", "worker2", 0);
        let mgr = manager(pool.clone());
        let parent = parent_run(&mgr, "parent2").await;

        let mut env = FlowEnvelope::empty();
        env.meta.insert("agent_id".into(), json!("parent2"));
        env.meta.insert("agent_run_id".into(), json!(parent));

        let out = SpawnNodeAdapter::new(service(pool.clone()))
            .spawn_with(
                &mgr,
                // agent_id (not agent_name) must resolve to the worker's name.
                &node(json!({"agent_id": "worker-id", "task": "x"})),
                &env,
                &stub_ctx(),
            )
            .await
            .expect("execute");
        let run_ids = match out.variables.get("spawned_run_ids") {
            Some(FlowValue::Json(v)) => v.as_array().cloned().expect("array"),
            other => panic!("expected json array, got {other:?}"),
        };
        assert_eq!(run_ids.len(), 1);
    }

    #[tokio::test]
    async fn spawn_without_run_context_errors() {
        let pool = db();
        seed_agent(&pool, "worker3", "worker3", 0);
        let mgr = manager(pool.clone());

        // No agent_run_id in meta → not a managed run context.
        let env = FlowEnvelope::empty();
        let err = SpawnNodeAdapter::new(service(pool))
            .spawn_with(
                &mgr,
                &node(json!({"agent_name": "worker3", "task": "x"})),
                &env,
                &stub_ctx(),
            )
            .await
            .expect_err("must error without run context");
        assert!(
            err.to_string().contains("managed run context"),
            "got: {err}"
        );
    }
}
