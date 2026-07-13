// ===== File: flow_engine/node_adapters/ask_user.rs — AskUserNodeAdapter
// (node_type "ask_user", category service, 1-in/1-out). The Flow Builder
// equivalent of a BPMN User Task (§3.13 C): pause the flow, ask the operator a
// question, write their answer to a flow variable, continue. Same delivery
// mechanic as the `core.ask_user` tool — it raises a question interaction, parks
// the owning run in `waiting_user` (releasing its permit + pausing its
// deadline), and awaits the reply with a configurable timeout. On timeout the
// answer is the no-response sentinel so a downstream condition can branch. The
// answer is written to `output_variable` (the variables channel, §3.12). In v1
// it is bounded by the flow deadline (minutes, not days — durable human tasks
// need persistent instances, §6). =====

use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::expr::{evaluate, ExprScope};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "ask_user";
const DEFAULT_OUTPUT_VARIABLE: &str = "user_response";
/// Shared default human-wait budget (§3.13). Mirrors the ask_user tool.
const DEFAULT_TIMEOUT_SECS: u64 = crate::agents::DEFAULT_INTERACTION_TIMEOUT_SECS;

pub struct AskUserNodeAdapter;

impl AskUserNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Resolves the question text. `question` is CEL-interpolable (§3.13 C): when
    /// it parses + evaluates to a value over the envelope scope, that string is
    /// used; otherwise the raw config string is taken verbatim (a plain question
    /// that is not a CEL expression must still work). An empty question is a
    /// node error — there is nothing to ask.
    fn resolve_question(node: &FlowNode, envelope: &FlowEnvelope) -> Result<String> {
        let raw = node
            .config
            .get("question")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("ask_user node '{}': 'question' is required", node.id))?;

        let extras: [(&str, Value); 0] = [];
        let scope = ExprScope {
            vars: &envelope.variables,
            payload: &envelope.payload,
            artifacts: &envelope.artifacts,
            meta: &envelope.meta,
            extras: &extras,
        };
        // Interpolate via CEL; a non-string result is stringified, and any parse/
        // eval error falls back to the literal config (a plain question is not a
        // valid CEL expression and must still be asked as-is).
        match evaluate(raw, &scope, None) {
            Ok(Value::String(s)) => Ok(s),
            Ok(other) => Ok(other.to_string()),
            Err(_) => Ok(raw.to_string()),
        }
    }

    /// Up to 4 choices from config (§3.13 C); the dashboard appends its own
    /// "other" option. Non-string entries are ignored.
    fn resolve_choices(node: &FlowNode) -> Vec<String> {
        node.config
            .get("choices")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .take(4)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn resolve_timeout(node: &FlowNode) -> Duration {
        let secs = node
            .config
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .min(3600);
        Duration::from_secs(secs)
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

impl Default for AskUserNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for AskUserNodeAdapter {
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
            .ok_or_else(|| anyhow!("ask_user: missing input edge"))?;
        let envelope = &input.envelope;

        let question = Self::resolve_question(node, envelope)?;
        let choices = Self::resolve_choices(node);
        let timeout = Self::resolve_timeout(node);
        let output_variable = Self::output_variable(node);

        // The owning run (if any) — its question parks it in waiting_user. A flow
        // without a run (a bare ask_user flow) still works: the manager calls
        // become no-ops, the question is delivered by scope, and the timeout
        // still applies. `parent_run_id` is left unset here: the ask_user BLOCK
        // is a top-level User Task, not a sub-agent ask (the `core.ask_user`
        // tool path resolves the parent chain for bubbling).
        let run_id = envelope
            .meta
            .get("agent_run_id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let manager = crate::agents::agent_run_manager_global();

        let (answer, waited) = crate::agents::run_ask_user(
            &crate::agents::interaction_registry_global(),
            manager.as_deref(),
            ctx.progress.as_ref(),
            &ctx.progress_scope,
            &run_id,
            None,
            &question,
            &choices,
            timeout,
        )
        .await;
        // Human think-time must not consume the run's deadline (§3.13).
        ctx.extend_deadline(waited);

        let mut out: FlowEnvelope = (**envelope).clone();
        // Write the operator's answer to the configured flow variable so
        // downstream blocks read it (the variables channel, §3.12).
        out.variables
            .insert(output_variable, FlowValue::Text(answer));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{interaction_registry_global, InteractionReply, QuestionReply};
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration as StdDuration;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "au1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    fn input(env: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "prev".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    #[tokio::test]
    async fn reply_writes_answer_to_output_variable() {
        let env = FlowEnvelope::empty();
        let ctx = stub_ctx();
        let adapter = AskUserNodeAdapter::new();

        // Spawn the block, then resolve the single pending interaction by id.
        let exec = tokio::spawn(async move {
            adapter
                .execute(
                    &node(json!({"question": "pick one", "choices": ["a", "b"], "output_variable": "choice"})),
                    &[input(env)],
                    &ctx,
                )
                .await
        });

        // Wait for the interaction to register, then answer it.
        let reg = interaction_registry_global();
        let id = loop {
            let pending = reg.list_for(true, &[]);
            if let Some(p) = pending.iter().find(|p| p.prompt == "pick one") {
                break p.id.clone();
            }
            tokio::time::sleep(StdDuration::from_millis(5)).await;
        };
        assert!(reg.reply(
            &id,
            InteractionReply::Question(QuestionReply { answer: "a".into() })
        ));

        let out = exec.await.expect("join").expect("execute");
        let written = out.variables.get("choice").expect("variable written");
        match written {
            FlowValue::Text(t) => {
                assert!(t.contains("trusted user channel"));
                assert!(t.contains('a'));
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn timeout_writes_sentinel() {
        let env = FlowEnvelope::empty();
        let ctx = stub_ctx();
        let out = AskUserNodeAdapter::new()
            .execute(
                &node(json!({"question": "anyone there?", "timeout_secs": 1, "output_variable": "ans"})),
                &[input(env)],
                &ctx,
            )
            .await
            .expect("execute");
        // No reply within the (clamped) budget → the sentinel lands in the var.
        let written = out.variables.get("ans").expect("variable written");
        match written {
            FlowValue::Text(t) => assert!(t.contains("did not respond")),
            other => panic!("expected text, got {other:?}"),
        }
    }
}
