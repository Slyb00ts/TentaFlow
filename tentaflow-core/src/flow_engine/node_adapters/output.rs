// =============================================================================
// Plik: flow_engine/node_adapters/output.rs
// Opis: OutputNodeAdapter — terminal sink flow. Bierze envelope z jedynego
//       inputu i zwraca go w niezmienionej formie. Zgodnie z hard rule 1
//       (single input edge) executor wstawia tu zawsze dokładnie 1 NodeInput.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter};
use crate::flow_engine::types::FlowNode;

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

const INPUT_PORTS: &[&str] = &["in"];
const OUTPUT_PORTS: &[&str] = &["full"];

#[async_trait]
impl NodeAdapter for OutputNodeAdapter {
    fn node_type(&self) -> &str {
        "output"
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
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("output node requires exactly 1 input edge"))?;
        // Klon Arc<FlowEnvelope> jest tani; envelope jest immutable z perspektywy
        // adaptera. Executor finalizuje wybór "ostatniego" envelope — output
        // node sygnalizuje terminal pozycję w grafie, semantycznie passthrough.
        Ok((*input.envelope).clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::envelope::FlowValue;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use std::sync::Arc;

    fn output_node() -> FlowNode {
        FlowNode {
            id: "out-1".into(),
            node_type: "output".into(),
            config: serde_json::Value::Null,
            position: None,
            label: None,
        }
    }

    #[tokio::test]
    async fn output_passes_through_payload_and_meta() {
        let adapter = OutputNodeAdapter::new();
        let mut env = FlowEnvelope::with_payload(FlowValue::Text("hello".into()));
        env.meta
            .insert("request_id".into(), serde_json::json!("r-1"));

        let inputs = vec![NodeInput {
            from_node_id: "llm-1".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }];

        let result = adapter
            .execute(&output_node(), &inputs, &stub_ctx())
            .await
            .unwrap();
        assert_eq!(result.payload.as_text(), Some("hello"));
        assert_eq!(
            result.meta.get("request_id").and_then(|v| v.as_str()),
            Some("r-1")
        );
    }

    #[tokio::test]
    async fn output_errors_when_no_inputs() {
        let adapter = OutputNodeAdapter::new();
        let err = adapter
            .execute(&output_node(), &[], &stub_ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("requires exactly 1 input edge"));
    }

    #[test]
    fn output_advertises_correct_ports() {
        let a = OutputNodeAdapter::new();
        assert_eq!(a.supported_input_ports(), &["in"]);
        assert_eq!(a.supported_output_ports(), &["full"]);
        assert_eq!(a.node_type(), "output");
    }
}
