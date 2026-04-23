// =============================================================================
// Plik: flow_engine/adapters/trigger.rs
// Opis: Adapter wezla trigger — punkt wejscia flow. Przekazuje pola z
//       FlowContext (input, model, request_id) do reszty grafu.
// =============================================================================

use anyhow::Result;
use serde_json::Value;

use crate::flow_engine::adapters::NodeAdapter;
use crate::flow_engine::types::FlowContext;

/// Buduje output JSON dla trigger node'a na podstawie kontekstu flow.
/// Uzywane bezposrednio przez executor (match arm `trigger`) oraz przez
/// TriggerNodeAdapter::execute, zeby logika istniala w jednym miejscu.
pub fn build_trigger_output(ctx: &FlowContext) -> Value {
    serde_json::json!({
        "input": ctx.input,
        "model": ctx.model,
        "request_id": ctx.request_id,
    })
}

/// Adapter rejestrowany w AdapterRegistry. Sluzy walidacji flow_json
/// (metadata portow) — executor zatrzymuje sie na match arm zanim dotrze
/// do rejestru, ale adapter jest kompletna, dzialajaca implementacja.
pub struct TriggerNodeAdapter;

impl TriggerNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TriggerNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeAdapter for TriggerNodeAdapter {
    async fn execute(&self, _node_config: &Value, ctx: &mut FlowContext) -> Result<Value> {
        Ok(build_trigger_output(ctx))
    }

    fn node_type(&self) -> &'static str {
        "trigger"
    }
}
