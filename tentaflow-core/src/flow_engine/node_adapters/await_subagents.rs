// ===== File: flow_engine/node_adapters/await_subagents.rs — AwaitSubagentsNodeAdapter
// (node_type "await_subagents", category logic, 1-in/1-out). Deterministic,
// graph-driven counterpart to the `core.agent_wait` tool: a node blocks until
// the named child runs settle (or the timeout elapses). The run ids come from a
// flow variable a prior `spawn` block wrote (or an explicit `run_ids` config
// list). It calls AgentRunManager::handle_agent_wait, which releases the caller's
// concurrency permit while parked (anti-livelock) and reacquires it on wake. The
// per-run results land in a flow variable; the payload also gets a short summary.
// `mode` (all|any) decides whether it waits for every run or the first finisher.
// (Harness §3.5.) =====

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agents::{AgentPrincipal, CallerRun};
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "await_subagents";
const DEFAULT_RUN_IDS_VAR: &str = "spawned_run_ids";
const DEFAULT_OUTPUT_VARIABLE: &str = "subagent_results";
const DEFAULT_TIMEOUT_SECS: u64 = 600;
const MAX_TIMEOUT_SECS: u64 = 3600;

pub struct AwaitSubagentsNodeAdapter;

impl AwaitSubagentsNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Resolves the run ids to wait on. An explicit `run_ids` config array wins;
    /// otherwise the ids are read from the variable named by `run_ids_var`
    /// (default `spawned_run_ids`, the key `spawn` writes). Accepts a JSON array
    /// (FlowValue::Json) or a single text id. An empty set is a node error — there
    /// is nothing to wait on.
    fn resolve_run_ids(node: &FlowNode, envelope: &FlowEnvelope) -> Result<Vec<String>> {
        if let Some(arr) = node.config.get("run_ids").and_then(|v| v.as_array()) {
            let ids: Vec<String> = arr
                .iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect();
            if !ids.is_empty() {
                return Ok(ids);
            }
        }
        let var = node
            .config
            .get("run_ids_var")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_RUN_IDS_VAR);
        let ids = match envelope.variables.get(var) {
            Some(FlowValue::Json(Value::Array(a))) => a
                .iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect::<Vec<_>>(),
            Some(FlowValue::Text(t)) if !t.is_empty() => vec![t.clone()],
            _ => Vec::new(),
        };
        if ids.is_empty() {
            return Err(anyhow!(
                "await_subagents node '{}': no run ids in config or variable '{var}'",
                node.id
            ));
        }
        Ok(ids)
    }

    fn timeout_secs(node: &FlowNode) -> u64 {
        node.config
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(MAX_TIMEOUT_SECS)
    }

    fn mode(node: &FlowNode) -> &'static str {
        match node.config.get("mode").and_then(|v| v.as_str()) {
            Some("any") => "any",
            _ => "all",
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

    /// Core wait step against an explicit manager — the path `execute` takes once
    /// it has resolved the process-global manager. Split out so a test can drive
    /// it with a manager bound to its own in-memory db.
    async fn await_with(
        &self,
        manager: &crate::agents::AgentRunManager,
        node: &FlowNode,
        envelope: &FlowEnvelope,
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let run_ids = Self::resolve_run_ids(node, envelope)?;
        let timeout_secs = Self::timeout_secs(node);
        let mode = Self::mode(node);
        let output_variable = Self::output_variable(node);

        let principal = AgentPrincipal::new(ctx.user_id.clone(), None);
        let caller = CallerRun::from_envelope(envelope, principal, ctx.session_id.clone());
        if caller.run_id.is_empty() {
            return Err(anyhow!(
                "await_subagents: requires a managed run context (place after agent_context)"
            ));
        }

        let args = json!({
            "run_ids": run_ids,
            "timeout_secs": timeout_secs,
            "mode": mode,
        });
        let results = manager.handle_agent_wait(&caller, &args).await?;

        // A compact summary on the payload lets a downstream LLM see how the
        // delegated work settled without re-reading the full result blobs.
        let summary = Self::summarize(&results);

        let mut out: FlowEnvelope = envelope.clone();
        out.variables
            .insert(output_variable, FlowValue::Json(results));
        out.payload = FlowValue::Text(summary);
        Ok(out)
    }

    /// One-line-per-run status summary (no result bodies) for the payload.
    fn summarize(results: &Value) -> String {
        let Some(map) = results.as_object() else {
            return "no sub-agent results".to_string();
        };
        if map.is_empty() {
            return "no sub-agent results".to_string();
        }
        map.iter()
            .map(|(run_id, entry)| {
                let status = entry
                    .get("status")
                    .and_then(|s| s.as_str())
                    .unwrap_or("unknown");
                format!("{run_id}: {status}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Default for AwaitSubagentsNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for AwaitSubagentsNodeAdapter {
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
            .ok_or_else(|| anyhow!("await_subagents: missing input edge"))?;
        let envelope = &input.envelope;

        let manager = crate::agents::agent_run_manager_global().ok_or_else(|| {
            anyhow!("await_subagents: agent run manager not available on this node")
        })?;

        self.await_with(&manager, node, envelope, ctx).await
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
    use tokio_util::sync::CancellationToken;

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    /// Completes instantly with a fixed result so a waited child settles to
    /// `completed` without a live flow dispatcher.
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
                text: format!("result-of-{scope}"),
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
                actor_user_id: None,
            },
        )
        .expect("seed agent");
    }

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "aw1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    #[tokio::test]
    async fn waits_for_children_and_writes_results() {
        let pool = db();
        seed_agent(&pool, "parent", "boss", 4);
        seed_agent(&pool, "worker", "worker", 0);
        let mgr = manager(pool.clone());

        // Parent run + two spawned children (via the manager, as spawn would).
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
            .handle_agent_spawn(
                &caller,
                &json!({"tasks": [
                    {"agent_name": "worker", "task": "a"},
                    {"agent_name": "worker", "task": "b"}
                ]}),
            )
            .await
            .expect("spawn children");
        let run_ids = spawn_out["run_ids"].clone();

        let mut env = FlowEnvelope::empty();
        env.meta.insert("agent_id".into(), json!("parent"));
        env.meta.insert("agent_run_id".into(), json!(parent));
        env.variables
            .insert("spawned_run_ids".into(), FlowValue::Json(run_ids.clone()));

        let out = AwaitSubagentsNodeAdapter::new()
            .await_with(
                &mgr,
                &node(json!({"timeout_secs": 30, "mode": "all"})),
                &env,
                &stub_ctx(),
            )
            .await
            .expect("await");

        let results = match out.variables.get("subagent_results") {
            Some(FlowValue::Json(v)) => v.as_object().cloned().expect("object"),
            other => panic!("expected json object, got {other:?}"),
        };
        assert_eq!(results.len(), 2, "both children present");
        for id in run_ids.as_array().unwrap() {
            let entry = results.get(id.as_str().unwrap()).expect("entry");
            assert_eq!(entry["status"], "completed");
            assert!(entry["result"].as_str().unwrap().starts_with("result-of-"));
        }
        // Payload summary mentions both run ids.
        match out.payload {
            FlowValue::Text(t) => assert_eq!(t.lines().count(), 2),
            other => panic!("expected text payload, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_run_ids_errors() {
        let pool = db();
        seed_agent(&pool, "parent2", "boss2", 4);
        let mgr = manager(pool.clone());
        let parent = mgr
            .spawn(
                "parent2",
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

        let mut env = FlowEnvelope::empty();
        env.meta.insert("agent_id".into(), json!("parent2"));
        env.meta.insert("agent_run_id".into(), json!(parent));
        // No spawned_run_ids variable, no explicit run_ids.
        let err = AwaitSubagentsNodeAdapter::new()
            .await_with(&mgr, &node(json!({})), &env, &stub_ctx())
            .await
            .expect_err("must error");
        assert!(err.to_string().contains("no run ids"), "got: {err}");
    }
}
