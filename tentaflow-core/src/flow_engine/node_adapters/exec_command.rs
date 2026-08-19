// ===== File: flow_engine/node_adapters/exec_command.rs —
// ExecCommandNodeAdapter (node_type "exec_command", category service,
// 1-in/1-out). The deterministic counterpart of `core.exec` (§16.4): a command
// the GRAPH decides to run, not the model — a lint gate, a build step, a smoke
// test that must happen whatever the agent concluded.
//
// The block chooses a profile, but it cannot widen one: the PEP is consulted
// exactly as it is for the tool, and the configured mount/network access is
// intersected with what the decision allows. A flow author asking for `rw` in a
// session whose autonomy mode forbids writing gets the mode's answer, not the
// config's.
//
// Nor can the block widen the agent's SURFACE. `core.exec` inside a graph is
// still `core.exec`: it passes `agents.tools_json` first (§10), so dropping
// this block into the harness of a reviewer that holds no exec verb refuses
// instead of running the command.
// =====

use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agents::AgentServiceSlot;
use crate::code_studio::tools::{self, ToolCallCtx};
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

use super::patch_review::InteractionGate;

const NODE_TYPE: &str = "exec_command";
const DEFAULT_OUTPUT_VARIABLE: &str = "exec_result";
const DEFAULT_TIMEOUT_SECS: u64 = 300;

pub struct ExecCommandNodeAdapter {
    service: AgentServiceSlot,
}

impl ExecCommandNodeAdapter {
    pub fn new(service: AgentServiceSlot) -> Self {
        Self { service }
    }

    fn argv(node: &FlowNode) -> Result<Vec<String>> {
        let argv: Vec<String> = node
            .config
            .get("argv")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if argv.is_empty() {
            return Err(anyhow!(
                "exec_command node '{}': 'argv' is required and must be a non-empty array",
                node.id
            ));
        }
        Ok(argv)
    }

    fn output_variable(node: &FlowNode) -> String {
        node.config
            .get("output_variable")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_OUTPUT_VARIABLE)
            .to_string()
    }
}

#[async_trait]
impl NodeAdapter for ExecCommandNodeAdapter {
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
            .ok_or_else(|| anyhow!("exec_command: missing input edge"))?;
        let envelope = &input.envelope;

        // §10 sieve one, before the binding and before the PEP: the running
        // agent's allowlist. A block is a caller like any other.
        let service = self
            .service
            .read()
            .clone()
            .ok_or_else(|| anyhow!("exec_command: AgentService slot not wired"))?;
        service.require_core_tool(
            envelope.meta.get("agent_id").and_then(|v| v.as_str()),
            crate::agents::CoreToolName::Exec,
        )?;

        let binding = tools::binding_from_meta(&envelope.meta).ok_or_else(|| {
            anyhow!(
                "exec_command: this run carries no Code Studio session binding \
                 (meta.code_session)"
            )
        })?;
        let user_id = ctx
            .user_id
            .clone()
            .ok_or_else(|| anyhow!("exec_command: running a command needs a user identity"))?;
        let run_id = envelope
            .meta
            .get("agent_run_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let argv = Self::argv(node)?;
        let timeout_secs = node
            .config
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        let fail_on_nonzero = node
            .config
            .get("fail_on_nonzero")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let mut args = json!({
            "argv": argv,
            "timeout_secs": timeout_secs,
            "purpose": node
                .label
                .clone()
                .unwrap_or_else(|| format!("flow block '{}'", node.id)),
        });
        if let Some(cwd) = node.config.get("cwd").and_then(|v| v.as_str()) {
            args["cwd"] = Value::String(cwd.to_string());
        }
        // `mount_access`, `network_access` and `ephemeral` are the author's
        // REQUEST. They travel as meta on the call so the sandbox layer can
        // narrow the lease; they never widen the PEP's decision.
        for key in ["mount_access", "network_access", "ephemeral"] {
            if let Some(value) = node.config.get(key) {
                args[key] = value.clone();
            }
        }

        let main_db = crate::db::global_pool()
            .ok_or_else(|| anyhow!("exec_command: the core database is not available"))?;
        let registry = crate::agents::interaction_registry_global();
        let manager = crate::agents::agent_run_manager_global();
        let extend = |waited: Duration| ctx.extend_deadline(waited);
        let gate = InteractionGate::new(
            &registry,
            manager.as_deref(),
            ctx.progress.as_ref(),
            &ctx.progress_scope,
            &run_id,
            None,
            &extend,
        );
        let call_id = format!("{}:{}", node.id, ctx.execution_id);
        let call_ctx = ToolCallCtx {
            main_db: &main_db,
            user_id: &user_id,
            run_id: (!run_id.is_empty()).then_some(run_id.as_str()),
            tool_call_id: &call_id,
            binding: &binding,
            gate: &gate,
        };

        let result = tools::execute(&call_ctx, crate::agents::CoreToolName::Exec, &args).await?;
        let exit_code = result.get("exit_code").and_then(|v| v.as_i64());
        if fail_on_nonzero && exit_code != Some(0) {
            // The graph author asked for a gate, so a red command stops the run
            // instead of flowing on as data nobody looks at.
            return Err(anyhow!(
                "exec_command '{}': {} exited with {}",
                node.id,
                argv.join(" "),
                exit_code.map(|c| c.to_string()).unwrap_or_else(|| result
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("no exit code")
                    .to_string())
            ));
        }

        let mut out: FlowEnvelope = (**envelope).clone();
        out.variables
            .insert(Self::output_variable(node), FlowValue::Json(result.clone()));
        out.payload = FlowValue::Text(format!(
            "{} -> {}",
            argv.join(" "),
            result
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        ));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::envelope::FlowEnvelope;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use std::sync::Arc;

    /// A slot holding a service over a freshly seeded database — so the agents
    /// the sieve is tested against are the REAL seeded roster (§15), not a
    /// fixture written to agree with the test.
    fn seeded_slot() -> (crate::db::DbPool, crate::agents::AgentServiceSlot) {
        let pool = crate::db::init(std::path::Path::new(":memory:")).expect("init db");
        let cipher = Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]));
        let addons =
            Arc::new(crate::addon::AddonManager::new(pool.clone(), cipher).expect("addon mgr"));
        let service = Arc::new(crate::agents::AgentService::new(pool.clone(), addons));
        (pool, Arc::new(parking_lot::RwLock::new(Some(service))))
    }

    fn agent_id(pool: &crate::db::DbPool, name: &str) -> String {
        crate::db::repository::get_agent_by_name(pool, name)
            .expect("query agent")
            .unwrap_or_else(|| panic!("seeded agent '{name}' is missing"))
            .id
    }

    fn inputs_for(agent_id: Option<&str>) -> Vec<NodeInput> {
        let mut envelope = FlowEnvelope::empty();
        if let Some(id) = agent_id {
            envelope.meta.insert("agent_id".into(), json!(id));
        }
        // A real binding: the sieve must refuse BEFORE the session is touched,
        // so its presence proves the refusal is about the allowlist and not
        // about a missing workspace.
        envelope.meta.insert(
            crate::code_studio::tools::SESSION_META_KEY.to_string(),
            crate::code_studio::tools::binding_meta_value("wsexec", "sessexec"),
        );
        vec![NodeInput {
            from_node_id: "upstream".to_string(),
            from_port: "full".to_string(),
            envelope: Arc::new(envelope),
        }]
    }

    /// A9 — `agents.tools_json` is the FIRST sieve and it applies to the graph
    /// too. `code-reviewer` holds no `core.exec`; dropping this block into its
    /// harness must refuse, not run the command. Before the fix the block called
    /// `tools::execute(CoreToolName::Exec)` with no allowlist check at all, so
    /// the graph out-ranked the agent definition.
    #[tokio::test]
    async fn an_agent_without_core_exec_cannot_run_the_block() {
        let (pool, slot) = seeded_slot();
        let reviewer = agent_id(&pool, "code-reviewer");
        let err = ExecCommandNodeAdapter::new(slot)
            .execute(
                &node(json!({"argv": ["rm", "-rf", "/"]})),
                &inputs_for(Some(&reviewer)),
                &stub_ctx(),
            )
            .await
            .expect_err("a reviewer must not be able to exec through a flow block");
        let message = format!("{err:#}");
        assert!(message.contains("core.exec"), "{message}");
        assert!(message.contains("allowlist"), "{message}");
    }

    /// A run with no agent pinned has NO surface — the same answer `tool_exec`
    /// gives a model-issued call on a misconfigured flow.
    #[tokio::test]
    async fn a_run_without_an_agent_has_no_surface_at_all() {
        let (_pool, slot) = seeded_slot();
        let err = ExecCommandNodeAdapter::new(slot)
            .execute(
                &node(json!({"argv": ["true"]})),
                &inputs_for(None),
                &stub_ctx(),
            )
            .await
            .expect_err("no agent means no allowlist");
        assert!(format!("{err:#}").contains("core.exec"));
    }

    /// The other side of the sieve: `code-tester` DOES hold `core.exec`, so the
    /// block gets past the allowlist and fails at the next gate instead. Without
    /// this half, a check that always refused would pass the test above.
    #[tokio::test]
    async fn an_agent_holding_core_exec_passes_the_sieve_and_stops_at_the_next_gate() {
        let (pool, slot) = seeded_slot();
        let tester = agent_id(&pool, "code-tester");
        let err = ExecCommandNodeAdapter::new(slot)
            .execute(
                &node(json!({"argv": ["cargo", "test"]})),
                &inputs_for(Some(&tester)),
                &stub_ctx(),
            )
            .await
            .expect_err("no user identity in the stub context");
        let message = format!("{err:#}");
        assert!(
            !message.contains("allowlist"),
            "the allowlist must not be what stopped this run: {message}"
        );
        assert!(message.contains("user identity"), "{message}");
    }

    fn node(config: Value) -> FlowNode {
        FlowNode {
            id: "x1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    #[test]
    fn argv_is_required_and_must_be_a_list() {
        assert!(ExecCommandNodeAdapter::argv(&node(json!({}))).is_err());
        assert!(ExecCommandNodeAdapter::argv(&node(json!({"argv": []}))).is_err());
        // A shell string is not argv: there is no shell in the sandbox path, so
        // accepting one would produce a command nobody can audit.
        assert!(ExecCommandNodeAdapter::argv(&node(json!({"argv": "cargo test"}))).is_err());
        assert_eq!(
            ExecCommandNodeAdapter::argv(&node(json!({"argv": ["cargo", "test"]}))).unwrap(),
            vec!["cargo".to_string(), "test".to_string()]
        );
    }

    #[test]
    fn output_variable_defaults_but_is_overridable() {
        assert_eq!(
            ExecCommandNodeAdapter::output_variable(&node(json!({}))),
            DEFAULT_OUTPUT_VARIABLE
        );
        assert_eq!(
            ExecCommandNodeAdapter::output_variable(&node(json!({"output_variable": "lint"}))),
            "lint"
        );
    }
}
