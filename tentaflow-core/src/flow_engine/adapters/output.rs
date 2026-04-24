// =============================================================================
// Plik: flow_engine/adapters/output.rs
// Opis: Adapter wezla output — sink flow. Przekazuje tekst z poprzednika
//       jako finalny rezultat z polem type="flow_output".
// =============================================================================

use anyhow::Result;
use serde_json::Value;

use crate::flow_engine::adapters::NodeAdapter;
use crate::flow_engine::types::{FlowContext, FlowNode};

/// Wspolny resolver tekstu wejsciowego dla wezlow passthrough (output/router).
/// Szuka w konfiguracji `input_from`, potem w ostatnim wezle z polem `text`,
/// finalnie uzywa ctx.input.
pub fn resolve_passthrough_text(node: &FlowNode, ctx: &FlowContext) -> String {
    if let Some(input_from) = node.config.get("input_from").and_then(|v| v.as_str()) {
        if let Some(prev_result) = ctx.node_results.get(input_from) {
            if let Some(text) = prev_result.get("text").and_then(|v| v.as_str()) {
                return text.to_string();
            }
            return prev_result.to_string();
        }
    }

    for step in ctx.execution_log.iter().rev() {
        if let Some(prev_result) = ctx.node_results.get(&step.node_id) {
            if let Some(text) = prev_result.get("text").and_then(|v| v.as_str()) {
                return text.to_string();
            }
        }
    }

    ctx.input.clone()
}

/// Buduje output JSON dla wezla typu passthrough (output, router).
pub fn build_passthrough_output(node: &FlowNode, ctx: &FlowContext, type_name: &str) -> Value {
    let text = resolve_passthrough_text(node, ctx);
    serde_json::json!({
        "type": type_name,
        "text": text,
    })
}

pub struct OutputNodeAdapter;

impl OutputNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for OutputNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeAdapter for OutputNodeAdapter {
    async fn execute(&self, _node_config: &Value, ctx: &mut FlowContext) -> Result<Value> {
        // Adapter nie ma dostepu do FlowNode (tylko do config Value) — wiec
        // resolver dziala na ctx.execution_log + ctx.input. To wystarczy bo
        // seedowane flows maja wlasciwe wpisy w execution_log przed outputem.
        let text = ctx
            .execution_log
            .iter()
            .rev()
            .find_map(|step| {
                ctx.node_results
                    .get(&step.node_id)
                    .and_then(|r| r.get("text").and_then(|v| v.as_str()).map(|s| s.to_string()))
            })
            .unwrap_or_else(|| ctx.input.clone());

        Ok(serde_json::json!({
            "type": "flow_output",
            "text": text,
        }))
    }

    fn node_type(&self) -> &'static str {
        "output"
    }
}
