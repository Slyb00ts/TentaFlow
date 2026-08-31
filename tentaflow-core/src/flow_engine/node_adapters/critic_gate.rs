// ===== File: flow_engine/node_adapters/critic_gate.rs — CriticGateNodeAdapter
// (node_type "critic_gate", category logic, 1-in/1-out). The block that ends a
// review loop.
//
// A loop region normally stops on "the last assistant turn carried no tool
// calls", which is the right rule for a tool loop and the WRONG one for a
// review loop: delegate → wait → judge produces no assistant tool calls at all,
// so such a region would run exactly once. This block is the alternative stop,
// and it is deliberately a BLOCK rather than a rule inside the engine — an
// author has to be able to see the reviewer in the Flow Builder, change what
// counts as approval, and delete the reviewer entirely if they do not want one.
//
// It reads the reviewer's answer out of a flow variable, decides whether that
// answer is an approval, and sets `meta.loop_should_exit` accordingly. The
// region runner honours that flag; the iteration budget on the region entry
// remains the ceiling, so a reviewer that never approves still terminates.
//
// The decision is written to a variable too, so the graph can branch on it and
// the operator can see WHY a loop ended. =====

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::flow_engine::cache::LOOP_SHOULD_EXIT_META;
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "critic_gate";
const DEFAULT_VERDICT_VAR: &str = "critic_verdict";
const DEFAULT_OUTPUT_VARIABLE: &str = "critic_gate_decision";
/// What an approving reviewer is told to write. Configurable, because the
/// wording belongs to the prompt the author writes for their own reviewer.
const DEFAULT_APPROVED_MARKER: &str = "BEZ UWAG";

pub struct CriticGateNodeAdapter;

impl CriticGateNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    fn config_str(node: &FlowNode, key: &str, fallback: &str) -> String {
        node.config
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(fallback)
            .to_string()
    }

    /// The reviewer's answer, whatever shape the wait block left it in. A
    /// `await_subagents` result is an array of run summaries, so the text of
    /// every entry is folded together rather than reaching for one field that
    /// a different producer would not have written.
    fn verdict_text(envelope: &FlowEnvelope, var: &str) -> String {
        let Some(value) = envelope.variables.get(var) else {
            return String::new();
        };
        match value {
            FlowValue::Text(text) => text.clone(),
            FlowValue::Json(json) => flatten_json_text(json),
            other => format!("{other:?}"),
        }
    }
}

/// Every string inside a JSON value, joined. A reviewer's answer may arrive as
/// `[{"run_id":…,"output":"…"}]`, as a bare string, or as an object with the
/// text under a key this block cannot know the name of.
fn flatten_json_text(value: &Value) -> String {
    let mut parts = Vec::new();
    collect_strings(value, &mut parts);
    parts.join("\n")
}

fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => {
            for item in items {
                collect_strings(item, out);
            }
        }
        Value::Object(map) => {
            for (_, item) in map {
                collect_strings(item, out);
            }
        }
        _ => {}
    }
}

impl Default for CriticGateNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for CriticGateNodeAdapter {
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
            .ok_or_else(|| anyhow!("critic_gate: missing input edge"))?;
        let envelope = &input.envelope;

        let verdict_var = Self::config_str(node, "verdict_var", DEFAULT_VERDICT_VAR);
        let marker = Self::config_str(node, "approved_marker", DEFAULT_APPROVED_MARKER);
        let output_variable = Self::config_str(node, "output_variable", DEFAULT_OUTPUT_VARIABLE);

        let text = Self::verdict_text(envelope, &verdict_var);
        // Case-insensitive so a reviewer that shouts or whispers its approval is
        // still understood; the marker is matched as a substring because models
        // wrap a verdict in a sentence far more often than they emit it bare.
        let approved = !text.is_empty() && text.to_lowercase().contains(&marker.to_lowercase());

        let mut out: FlowEnvelope = (**envelope).clone();
        out.meta
            .insert(LOOP_SHOULD_EXIT_META.into(), Value::Bool(approved));
        out.variables.insert(
            output_variable,
            FlowValue::Json(json!({
                "approved": approved,
                "marker": marker,
                "verdict_var": verdict_var,
                // Enough of the answer to explain the decision in the UI without
                // copying a whole review into the envelope.
                "excerpt": text.chars().take(400).collect::<String>(),
            })),
        );
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use std::sync::Arc;

    fn node(config: Value) -> FlowNode {
        FlowNode {
            id: "g1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: Some("review".into()),
        }
    }

    fn envelope_with(var: &str, value: FlowValue) -> FlowEnvelope {
        let mut env = FlowEnvelope::empty();
        env.variables.insert(var.to_string(), value);
        env
    }

    async fn run(node: &FlowNode, env: FlowEnvelope) -> FlowEnvelope {
        let ctx = stub_ctx();
        let inputs = vec![NodeInput {
            from_node_id: "prev".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }];
        CriticGateNodeAdapter::new()
            .execute(node, &inputs, &ctx)
            .await
            .expect("gate must not fail on a well-formed input")
    }

    fn exits(env: &FlowEnvelope) -> Option<bool> {
        env.meta
            .get(LOOP_SHOULD_EXIT_META)
            .and_then(|v| v.as_bool())
    }

    #[tokio::test]
    async fn an_approving_reviewer_ends_the_loop() {
        let env = envelope_with(
            "critic_verdict",
            FlowValue::Text("Przejrzalem plan. BEZ UWAG - mozna wdrazac.".into()),
        );
        assert_eq!(exits(&run(&node(json!({})), env).await), Some(true));
    }

    #[tokio::test]
    async fn a_reviewer_with_objections_keeps_the_loop_running() {
        let env = envelope_with(
            "critic_verdict",
            FlowValue::Text("Brakuje obslugi bledow w kroku 3 i testow formularza.".into()),
        );
        assert_eq!(exits(&run(&node(json!({})), env).await), Some(false));
    }

    /// The wait block hands over an ARRAY of run results, not a string. A gate
    /// that only understood `FlowValue::Text` would read every real delegation
    /// as "no verdict" and spin until the budget ran out.
    #[tokio::test]
    async fn a_verdict_buried_in_a_subagent_result_array_is_still_read() {
        let env = envelope_with(
            "critic_verdict",
            FlowValue::Json(json!([
                {"run_id": "abc", "status": "completed",
                 "output": "Sprawdzilem wzgledem wytycznych: BEZ UWAG."}
            ])),
        );
        assert_eq!(exits(&run(&node(json!({})), env).await), Some(true));
    }

    /// An absent variable is NOT an approval. Treating "nothing" as "no
    /// objections" would let a broken reviewer wave every plan through.
    #[tokio::test]
    async fn a_missing_verdict_is_not_an_approval() {
        let out = run(&node(json!({})), FlowEnvelope::empty()).await;
        assert_eq!(exits(&out), Some(false));
    }

    #[tokio::test]
    async fn the_approval_wording_is_the_authors_to_choose() {
        let node = node(json!({"approved_marker": "SHIP IT", "verdict_var": "review"}));
        let env = envelope_with("review", FlowValue::Text("Looks good, ship it.".into()));
        let out = run(&node, env).await;
        assert_eq!(exits(&out), Some(true));
        let Some(FlowValue::Json(decision)) = out.variables.get("critic_gate_decision") else {
            panic!("the gate must record WHY it decided");
        };
        assert_eq!(
            decision.get("approved").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    /// A config field left blank in the Flow Builder must fall back to the
    /// documented default rather than reading a variable named "".
    #[tokio::test]
    async fn a_blank_config_field_falls_back_to_the_default() {
        let node = node(json!({"verdict_var": "   ", "approved_marker": ""}));
        let env = envelope_with("critic_verdict", FlowValue::Text("BEZ UWAG".into()));
        assert_eq!(exits(&run(&node, env).await), Some(true));
    }
}
