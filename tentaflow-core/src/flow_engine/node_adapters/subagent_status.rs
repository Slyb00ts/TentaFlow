// ===== File: flow_engine/node_adapters/subagent_status.rs — SubagentStatusNodeAdapter
// (node_type "subagent_status", category logic, 1-in/1-out). Deterministic,
// graph-driven counterpart to the `core.agent_list` tool: a NON-blocking snapshot
// of the caller's child runs. It calls AgentRunManager::handle_agent_list and
// writes a `[{run_id, status}]` array to a flow variable; the payload passes
// through unchanged. Built to sit in a watch region with `interval` so a flow can
// poll "are the children done yet?" without blocking on any one of them.
// (Harness §3.4.) =====

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agents::{AgentPrincipal, CallerRun};
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "subagent_status";
const DEFAULT_OUTPUT_VARIABLE: &str = "subagent_status";

pub struct SubagentStatusNodeAdapter;

impl SubagentStatusNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    fn output_variable(node: &FlowNode) -> String {
        node.config
            .get("output_variable")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_OUTPUT_VARIABLE)
            .to_string()
    }

    /// Core snapshot step against an explicit manager. Split out so a test can
    /// drive it with a manager bound to its own in-memory db.
    fn snapshot_with(
        &self,
        manager: &crate::agents::AgentRunManager,
        node: &FlowNode,
        envelope: &FlowEnvelope,
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let output_variable = Self::output_variable(node);

        // §2.5 — the run inherits the flow context's provenance verbatim; nothing
        // here derives an actor from `user_id`.
        let principal = AgentPrincipal::new(
            ctx.user_id.clone(),
            ctx.org_id.clone(),
            ctx.origin,
            ctx.actor(),
        )
        .with_correlation_id(ctx.correlation_id.clone());
        let caller = CallerRun::from_envelope(envelope, principal, ctx.session_id.clone());
        if caller.run_id.is_empty() {
            return Err(anyhow!(
                "subagent_status: requires a managed run context (place after agent_context)"
            ));
        }

        // handle_agent_list returns ACTIVE children only; the watch region's stop
        // condition therefore fires once this array is empty (all children
        // terminal). The status array reduces each entry to {run_id, status}.
        let listing = manager.handle_agent_list(&caller)?;
        let statuses: Vec<Value> = listing
            .get("runs")
            .and_then(|v| v.as_array())
            .map(|rows| {
                rows.iter()
                    .map(|r| {
                        json!({
                            "run_id": r.get("run_id").cloned().unwrap_or(Value::Null),
                            "status": r.get("status").cloned().unwrap_or(Value::Null),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut out: FlowEnvelope = envelope.clone();
        out.variables
            .insert(output_variable, FlowValue::Json(Value::Array(statuses)));
        Ok(out)
    }
}

impl Default for SubagentStatusNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for SubagentStatusNodeAdapter {
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
            .ok_or_else(|| anyhow!("subagent_status: missing input edge"))?;
        let envelope = &input.envelope;

        let manager = crate::agents::agent_run_manager_global().ok_or_else(|| {
            anyhow!("subagent_status: agent run manager not available on this node")
        })?;

        self.snapshot_with(&manager, node, envelope, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentRunManager, BackgroundFlowRunner};
    use crate::db::models::AgentParams;
    use crate::db::repository;
    use crate::db::{migrations, DbPool};
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use crate::flow_engine::progress_broker::ProgressBroker;
    use std::sync::Arc;
    use std::time::Instant;
    use tokio::sync::watch;
    use tokio_util::sync::CancellationToken;

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    /// Runner gated on a watch flag so a spawned child stays active (queued/
    /// running) until the test releases it — `subagent_status` must see it listed.
    struct GatedRunner {
        rx: watch::Receiver<bool>,
    }
    #[async_trait]
    impl BackgroundFlowRunner for GatedRunner {
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
            let mut rx = self.rx.clone();
            while !*rx.borrow() {
                if rx.changed().await.is_err() {
                    break;
                }
            }
            Ok(crate::agents::run_manager::AgentFlowOutcome {
                text: format!("done-{scope}"),
                usage: crate::flow_engine::envelope::TokenUsage::default(),
                model: None,
            })
        }
    }

    fn manager(pool: DbPool, rx: watch::Receiver<bool>) -> Arc<AgentRunManager> {
        let mgr = Arc::new(AgentRunManager::new(
            pool,
            Arc::new(GatedRunner { rx }),
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
            id: "ss1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    #[tokio::test]
    async fn lists_active_children_with_status() {
        let pool = db();
        seed_agent(&pool, "parent", "boss", 4);
        seed_agent(&pool, "worker", "worker", 0);
        let (tx, rx) = watch::channel(false);
        let mgr = manager(pool.clone(), rx);

        let parent = mgr
            .spawn(
                "parent",
                "lead",
                None,
                &AgentPrincipal::user("u1"),
                &[],
                &[],
                None,
                None,
            )
            .await
            .expect("parent");
        let caller = CallerRun {
            run_id: parent.clone(),
            agent_id: "parent".into(),
            principal: AgentPrincipal::user("u1"),
            session_id: None,
            code_session: None,
        };
        let spawn_out = mgr
            .handle_agent_spawn(&caller, &json!({"agent_name": "worker", "task": "x"}))
            .await
            .expect("spawn child");
        let child_id = spawn_out["run_ids"][0].as_str().unwrap().to_string();

        let mut env = FlowEnvelope::empty();
        env.meta.insert("agent_id".into(), json!("parent"));
        env.meta.insert("agent_run_id".into(), json!(parent));

        // Wait until the gated child registers as active before snapshotting.
        for _ in 0..200 {
            let listing = mgr.handle_agent_list(&caller).unwrap();
            if !listing["runs"].as_array().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let out = SubagentStatusNodeAdapter::new()
            .snapshot_with(&mgr, &node(json!({})), &env, &stub_ctx())
            .expect("snapshot");

        let statuses = match out.variables.get("subagent_status") {
            Some(FlowValue::Json(Value::Array(a))) => a.clone(),
            other => panic!("expected json array, got {other:?}"),
        };
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0]["run_id"], json!(child_id));
        assert!(matches!(
            statuses[0]["status"].as_str(),
            Some("queued") | Some("running")
        ));

        // Release the child; the snapshot now reports an empty active set (the
        // stop condition for a watch region).
        let _ = tx.send(true);
        for _ in 0..200 {
            let out = SubagentStatusNodeAdapter::new()
                .snapshot_with(&mgr, &node(json!({})), &env, &stub_ctx())
                .unwrap();
            if let Some(FlowValue::Json(Value::Array(a))) = out.variables.get("subagent_status") {
                if a.is_empty() {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("child never left the active set");
    }
}
