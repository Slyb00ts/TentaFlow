// ===== File: flow_engine/node_adapters/task_gate.rs — TaskGateNodeAdapter
// (node_type "task_gate", category logic, 1-in/1-out). The block that refuses to
// call the work finished while the plan still has open tasks.
//
// A critic's approval is an opinion, and an opinion can be wrong about facts.
// "Everything is done" is exactly such a fact, and it is the one a model is most
// likely to get wrong about its own work — the plan lives in the conversation,
// the conversation gets compacted, and a task written twenty messages ago
// quietly stops existing.
//
// So the plan is kept as ROWS (`session_tasks`, written by `core.task_plan` and
// moved by `core.task_update`), and this block asks the database rather than the
// model. It can only ever VETO: it clears the loop's exit signal while tasks are
// open and leaves it untouched otherwise, so a critic that still has objections
// is not overruled into finishing.
//
// It is an ordinary block: put it after the critic gate to make the plan binding,
// delete it to go back to trusting the critic alone. A task in `blocked` counts
// as open on purpose — work that hit a wall is unfinished work. =====

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::code_studio::{repository, tools, workspace_db};
use crate::flow_engine::cache::LOOP_SHOULD_EXIT_META;
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::agents::AgentServiceSlot;

const NODE_TYPE: &str = "task_gate";
const DEFAULT_OUTPUT_VARIABLE: &str = "open_tasks";

pub struct TaskGateNodeAdapter {
    service: AgentServiceSlot,
}

impl TaskGateNodeAdapter {
    pub fn new(service: AgentServiceSlot) -> Self {
        Self { service }
    }

    fn output_variable(node: &FlowNode) -> String {
        node.config
            .get("output_variable")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_OUTPUT_VARIABLE)
            .to_string()
    }
}

#[async_trait]
impl NodeAdapter for TaskGateNodeAdapter {
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
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("task_gate: missing input edge"))?;
        let envelope = &input.envelope;

        let binding = tools::binding_from_meta(&envelope.meta).ok_or_else(|| {
            anyhow!(
                "task_gate: this run carries no Code Studio session binding \
                 (meta.code_session); open the run from a Code Studio session"
            )
        })?;
        let service = self
            .service
            .read()
            .clone()
            .ok_or_else(|| anyhow!("task_gate: AgentService slot not wired"))?;
        let main_db = service.db().clone();
        let workspace_id = binding.workspace_id.clone();
        let session_id = binding.session_id.clone();

        let (open, tasks) = tokio::task::spawn_blocking(move || -> Result<(i64, Vec<Value>)> {
            repository::get_workspace(&main_db, &workspace_id)?
                .ok_or_else(|| anyhow!("task_gate: workspace of this session no longer exists"))?;
            let pool = workspace_db::open(&workspace_id)?;
            let open = tools::open_task_count(&pool, &session_id)?;
            let tasks = tools::session_tasks(&pool, &session_id)?;
            Ok((open, tasks))
        })
        .await
        .map_err(|e| anyhow!("task_gate: task failed: {e}"))??;

        let mut out: FlowEnvelope = (**envelope).clone();
        // A veto, never a grant. Clearing the flag keeps the loop turning; NOT
        // setting it when the plan is clear leaves whatever the critic decided,
        // so an unhappy critic is not overruled into finishing.
        if open > 0 {
            out.meta
                .insert(LOOP_SHOULD_EXIT_META.into(), Value::Bool(false));
        }
        out.variables.insert(
            Self::output_variable(node),
            FlowValue::Json(json!({ "open": open, "tasks": tasks })),
        );
        Ok(out)
    }
}
