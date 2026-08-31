// ===== File: flow_engine/node_adapters/bus_transform.rs —
// BusTransformNodeAdapter (node_type "bus_transform", category "transform").
// Pure declarative reshaping — `config.expression` (CEL, required) is
// evaluated against the inbound envelope's scope (`payload`/`vars`/
// `artifacts`/`meta`) and its JSON result REPLACES the payload
// (`FlowValue::Json`); everything else on the envelope (meta, artifacts,
// variables) passes through unchanged. No LLM calls, no I/O — mirrors mockup
// M03's FHIR Observation -> cmc-wynik v2 example (PLAN §6.3).
//
// This is a DIFFERENT layer from the generic `io_mapping.rs`
// `input_mapping`/`output_mapping` (which the executor applies to every node,
// this one included, before/after `execute`): those reshape CONFIG and
// VARIABLES, never the payload itself. A flow author who only needs to stash
// a computed value in `result.variables` should use `output_mapping`, not
// this node; `bus_transform` exists for the case where the shape of the
// MESSAGE itself must change before the next `bus_publish`. =====

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::expr::{evaluate, ExprScope};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

pub const NODE_TYPE: &str = "bus_transform";

pub struct BusTransformNodeAdapter;

impl BusTransformNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for BusTransformNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for BusTransformNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Json)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("bus_transform node requires exactly 1 input edge"))?;
        let expr = node
            .config
            .get("expression")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| anyhow!("bus_transform requires a non-empty 'expression'"))?;

        let extras: [(&str, serde_json::Value); 0] = [];
        let scope = ExprScope {
            vars: &input.envelope.variables,
            payload: &input.envelope.payload,
            artifacts: &input.envelope.artifacts,
            meta: &input.envelope.meta,
            extras: &extras,
        };
        let result = evaluate(expr, &scope, None)
            .map_err(|e| anyhow!("bus_transform node '{}': {e}", node.id))?;

        let mut out = (*input.envelope).clone();
        out.payload = FlowValue::Json(result);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use serde_json::json;
    use std::sync::Arc;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "bt-1".into(),
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
    async fn reshapes_payload_via_cel() {
        let env = FlowEnvelope::with_payload(FlowValue::Json(json!({
            "resourceType": "Observation",
            "valueQuantity": {"value": 5.6}
        })));
        let n = node(json!({
            "expression": "{'wynik': payload.valueQuantity.value, 'wersja': 'cmc-wynik-v2'}"
        }));
        let out = BusTransformNodeAdapter::new()
            .execute(&n, &[input(env)], &stub_ctx())
            .await
            .unwrap();
        match out.payload {
            FlowValue::Json(v) => {
                assert_eq!(v["wynik"], json!(5.6));
                assert_eq!(v["wersja"], json!("cmc-wynik-v2"));
            }
            other => panic!("expected Json payload, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn requires_expression() {
        let env = FlowEnvelope::with_payload(FlowValue::Json(json!({})));
        let err = BusTransformNodeAdapter::new()
            .execute(&node(json!({})), &[input(env)], &stub_ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("expression"));
    }

    #[tokio::test]
    async fn requires_input_edge() {
        let err = BusTransformNodeAdapter::new()
            .execute(&node(json!({"expression": "payload"})), &[], &stub_ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("1 input edge"));
    }

    #[tokio::test]
    async fn preserves_meta_and_variables() {
        let mut env = FlowEnvelope::with_payload(FlowValue::Json(json!({"a": 1})));
        env.meta.insert("bus_topic".into(), json!("orders.raw"));
        let n = node(json!({"expression": "payload.a + 1"}));
        let out = BusTransformNodeAdapter::new()
            .execute(&n, &[input(env)], &stub_ctx())
            .await
            .unwrap();
        assert_eq!(out.meta.get("bus_topic"), Some(&json!("orders.raw")));
        match out.payload {
            FlowValue::Json(v) => assert_eq!(v, json!(2)),
            other => panic!("expected Json payload, got {other:?}"),
        }
    }
}
