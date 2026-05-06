// =============================================================================
// Plik: flow_engine/node_adapters/trigger.rs
// Opis: TriggerNodeAdapter — punkt wejścia flow. Brak input edge'a; bierze
//       envelope z `ctx.initial_envelope` (seed dostarczony przez routing
//       przed `execute_blocking`/`execute_streaming`). Plan v4.2 D2.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter};
use crate::flow_engine::types::FlowNode;

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

const INPUT_PORTS: &[&str] = &[];
const OUTPUT_PORTS: &[&str] = &["full"];

#[async_trait]
impl NodeAdapter for TriggerNodeAdapter {
    fn node_type(&self) -> &str {
        "trigger"
    }

    fn supported_input_ports(&self) -> &[&'static str] {
        INPUT_PORTS
    }

    fn supported_output_ports(&self) -> &[&'static str] {
        OUTPUT_PORTS
    }

    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        // Trigger jest źródłem flow — nie powinien dostać input edge'a.
        // Validation w stage 1c (compile) odrzuca flow z trigger-with-input,
        // tu defensywne bail.
        if !inputs.is_empty() {
            return Err(anyhow!(
                "trigger node must not have incoming edges (got {})",
                inputs.len()
            ));
        }
        Ok((*ctx.initial_envelope).clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::envelope::FlowValue;
    use crate::flow_engine::node_adapter::test_support::stub_ctx_with_initial;

    fn trigger_node() -> FlowNode {
        FlowNode {
            id: "trigger-1".into(),
            node_type: "trigger".into(),
            config: serde_json::Value::Null,
            position: None,
            label: None,
        }
    }

    #[tokio::test]
    async fn trigger_emits_clone_of_initial_envelope() {
        let mut env = FlowEnvelope::with_payload(FlowValue::Text("hi".into()));
        env.meta.insert("model".into(), serde_json::json!("gpt-4"));
        let ctx = stub_ctx_with_initial(env);

        let adapter = TriggerNodeAdapter::new();
        let out = adapter
            .execute(&trigger_node(), &[], &ctx)
            .await
            .unwrap();
        assert_eq!(out.payload.as_text(), Some("hi"));
        assert_eq!(out.meta.get("model").and_then(|v| v.as_str()), Some("gpt-4"));
    }

    #[tokio::test]
    async fn trigger_rejects_incoming_inputs() {
        use std::sync::Arc;
        let adapter = TriggerNodeAdapter::new();
        let inputs = vec![NodeInput {
            from_node_id: "x".into(),
            from_port: "full".into(),
            envelope: Arc::new(FlowEnvelope::empty()),
        }];
        let ctx = stub_ctx_with_initial(FlowEnvelope::empty());
        let err = adapter
            .execute(&trigger_node(), &inputs, &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must not have incoming edges"));
    }

    #[test]
    fn trigger_advertises_zero_input_ports() {
        let a = TriggerNodeAdapter::new();
        assert!(a.supported_input_ports().is_empty());
        assert_eq!(a.supported_output_ports(), &["full"]);
        assert_eq!(a.node_type(), "trigger");
    }
}
