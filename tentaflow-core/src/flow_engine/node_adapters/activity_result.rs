// ============ File: activity_result.rs — explicit validated terminal activity results for blocking flows ============

use anyhow::{anyhow, ensure, Context, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::expr::{evaluate, validate_syntax, ExprScope};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowDefinition, FlowNode};

pub const NODE_TYPE: &str = "activity_result";

pub fn validate_config(node: &FlowNode) -> Result<&str> {
    let expression = node
        .config
        .get("result_expression")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .context("activity_result requires a non-empty result_expression")?;
    validate_syntax(expression, None)?;
    Ok(expression)
}

pub fn validate_process_result_binding(graph_json: &str, expression: Option<&str>) -> Result<()> {
    let definition: FlowDefinition = serde_json::from_str(graph_json)?;
    if definition
        .nodes
        .iter()
        .any(|node| node.node_type == NODE_TYPE)
    {
        ensure!(
            expression.map(str::trim) == Some("outputs.payload"),
            "a service flow with activity_result requires result expression outputs.payload"
        );
    }
    Ok(())
}

pub fn validate_graph(definition: &FlowDefinition) -> Result<()> {
    let results: Vec<_> = definition
        .nodes
        .iter()
        .filter(|node| node.node_type == NODE_TYPE)
        .collect();
    if results.is_empty() {
        return Ok(());
    }
    ensure!(
        results.len() == 1,
        "a flow must have exactly one activity_result terminal"
    );
    let terminal = results[0];
    ensure!(
        terminal.region.is_none(),
        "activity_result cannot terminate an inline loop region"
    );
    ensure!(
        definition
            .edges
            .iter()
            .all(|edge| edge.from_port != "stream"),
        "activity_result requires a blocking flow"
    );
    ensure!(
        definition.edges.iter().all(|edge| edge.from != terminal.id),
        "activity_result cannot have outgoing edges"
    );
    ensure!(
        definition
            .edges
            .iter()
            .filter(|edge| edge.to == terminal.id)
            .count()
            == 1,
        "activity_result requires exactly one input edge"
    );
    for node in &definition.nodes {
        ensure!(
            node.id == terminal.id
                || definition
                    .edges
                    .iter()
                    .any(|edge| edge.from == node.id && !edge.is_loop_back()),
            "node '{}' is a competing terminal; activity_result must be the only terminal",
            node.id
        );
    }
    validate_config(terminal)?;
    Ok(())
}

pub struct ActivityResultNodeAdapter;

impl ActivityResultNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ActivityResultNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for ActivityResultNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        Vec::new()
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        ensure!(
            inputs.len() == 1,
            "activity_result requires exactly one input edge"
        );
        let expression = validate_config(node)?;
        let input = &inputs[0].envelope;
        let scope = ExprScope {
            vars: &input.variables,
            payload: &input.payload,
            artifacts: &input.artifacts,
            meta: &input.meta,
            extras: &[],
        };
        let value = evaluate(expression, &scope, None)
            .map_err(|error| anyhow!("activity_result node '{}': {error}", node.id))?;
        let result = crate::processes::jobs::parse_contract_result(value)
            .with_context(|| format!("activity_result node '{}'", node.id))?;
        let mut envelope = (**input).clone();
        envelope.payload = FlowValue::Json(serde_json::to_value(result)?);
        Ok(envelope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use serde_json::json;
    use std::sync::Arc;

    fn node(expression: &str) -> FlowNode {
        FlowNode {
            id: "result".into(),
            node_type: NODE_TYPE.into(),
            config: json!({"result_expression": expression}),
            position: None,
            label: None,
            region: None,
        }
    }

    fn input() -> NodeInput {
        let mut envelope = FlowEnvelope::with_payload(FlowValue::Json(json!({"fixed":true})));
        envelope
            .meta
            .insert("request_id".into(), json!("server-request"));
        envelope
            .variables
            .insert("ticket".into(), FlowValue::Text("ISSUE-1".into()));
        NodeInput {
            from_node_id: "work".into(),
            from_port: "full".into(),
            envelope: Arc::new(envelope),
        }
    }

    #[tokio::test]
    async fn explicit_outcomes_preserve_outputs_and_upstream_context() {
        for outcome in ["Completed", "Error", "NeedsHuman", "Cancelled"] {
            let expression = format!("{{'outcome':'{outcome}','code':'TEST.RESULT','summary':vars.ticket,'outputs':payload,'evidence':['log:ISSUE-1']}}");
            let result = ActivityResultNodeAdapter::new()
                .execute(&node(&expression), &[input()], &stub_ctx())
                .await
                .unwrap();
            let FlowValue::Json(value) = result.payload else {
                panic!("result must be JSON");
            };
            assert_eq!(value["outcome"], outcome);
            assert_eq!(value["outputs"], json!({"fixed":true}));
            assert_eq!(value["summary"], "ISSUE-1");
            assert_eq!(value["evidence"], json!(["log:ISSUE-1"]));
            assert_eq!(result.meta["request_id"], "server-request");
            assert_eq!(result.variables["ticket"].as_text(), Some("ISSUE-1"));
        }
    }

    #[tokio::test]
    async fn invalid_or_incomplete_contracts_do_not_become_success() {
        for expression in ["payload", "{'outcome':'Completed'}",
            "{'outcome':'Unknown','code':null,'summary':'x','outputs':{},'evidence':[]}",
            "{'outcome':'Error','code':'invalid code','summary':'x','outputs':{},'evidence':[]}",
            "{'outcome':'Completed','code':null,'summary':'x','outputs':{},'evidence':[],'foreign_job':'job-2'}"] {
            assert!(ActivityResultNodeAdapter::new()
                .execute(&node(expression), &[input()], &stub_ctx()).await.is_err());
        }
    }

    #[tokio::test]
    async fn result_metadata_and_outputs_obey_the_process_limits() {
        for value in [
            json!({"outcome":"Completed", "code":null, "summary":"x".repeat(32*1024+1),
                "outputs":{}, "evidence":[]}),
            json!({"outcome":"Completed", "code":null, "summary":"x",
                "outputs":{"large":"x".repeat(256*1024)}, "evidence":[]}),
            json!({"outcome":"Completed", "code":null, "summary":"x",
                "outputs":{}, "evidence":vec!["log";65]}),
        ] {
            let mut oversized = input();
            Arc::make_mut(&mut oversized.envelope).payload = FlowValue::Json(value);
            assert!(ActivityResultNodeAdapter::new()
                .execute(&node("payload"), &[oversized], &stub_ctx())
                .await
                .is_err());
        }
    }

    #[test]
    fn advertised_ports_are_one_input_and_no_outputs() {
        let adapter = ActivityResultNodeAdapter::new();
        assert_eq!(adapter.input_ports().len(), 1);
        assert_eq!(adapter.input_ports()[0].name, "in");
        assert!(adapter.output_ports().is_empty());
    }

    fn graph() -> FlowDefinition {
        serde_json::from_value(json!({"nodes":[
            {"id":"trigger", "type":"trigger", "config":{}},
            {"id":"result", "type":"activity_result", "config":{"result_expression":"payload"}}
        ],"edges":[{"from":"trigger", "to":"result"}]}))
        .unwrap()
    }

    #[test]
    fn result_graph_refuses_competing_sinks_and_duplicate_results() {
        let mut definition = graph();
        assert!(validate_graph(&definition).is_ok());
        definition.nodes.push(FlowNode {
            id: "other".into(),
            node_type: "output".into(),
            config: json!({}),
            position: None,
            label: None,
            region: None,
        });
        assert!(validate_graph(&definition)
            .unwrap_err()
            .to_string()
            .contains("competing terminal"));
        definition.nodes[2].node_type = NODE_TYPE.into();
        assert!(validate_graph(&definition)
            .unwrap_err()
            .to_string()
            .contains("exactly one"));
    }

    #[test]
    fn result_graph_refuses_streams_regions_and_missing_input() {
        let mut definition = graph();
        definition.edges[0].from_port = "stream".into();
        assert!(validate_graph(&definition)
            .unwrap_err()
            .to_string()
            .contains("blocking"));
        definition.edges[0].from_port = "full".into();
        definition.nodes[1].region = Some("loop".into());
        assert!(validate_graph(&definition)
            .unwrap_err()
            .to_string()
            .contains("loop region"));
        definition.nodes[1].region = None;
        definition.edges.clear();
        assert!(validate_graph(&definition)
            .unwrap_err()
            .to_string()
            .contains("one input"));
    }

    #[test]
    fn result_config_refuses_missing_empty_and_invalid_expression() {
        for config in [
            json!({}),
            json!({"result_expression":" "}),
            json!({"result_expression":"{"}),
        ] {
            let mut result = node("payload");
            result.config = config;
            assert!(validate_config(&result).is_err());
        }
    }

    #[test]
    fn process_binding_requires_the_explicit_result_in_the_pinned_graph() {
        let graph_json = serde_json::to_string(&graph()).unwrap();
        for expression in [
            None,
            Some(""),
            Some("outputs"),
            Some("outputs.variables.result"),
        ] {
            assert!(validate_process_result_binding(&graph_json, expression).is_err());
        }
        assert!(validate_process_result_binding(&graph_json, Some("outputs.payload")).is_ok());
        let mut ordinary = graph();
        ordinary.nodes[1].node_type = "output".into();
        assert!(
            validate_process_result_binding(&serde_json::to_string(&ordinary).unwrap(), None)
                .is_ok()
        );
    }
}
