// =============================================================================
// Plik: flow_engine/node_adapters/combine.rs
// Opis: CombineNodeAdapter — fan-in node. Konsumuje N incoming edges z
//       roznych branchy flow, czeka az wszystkie wygeneruja envelope, laczy
//       ich tekstowa reprezentacje w jeden output (FlowValue::Text). Zwolniony
//       z R4 (1-input-edge) w validation.rs.
//
//       Metadane (session_id w envelope.meta + ctx.session_id, conversation
//       history, system_prompts) bierze z pierwszego inputu — wszystkie
//       branche zaczynaja od tego samego triggera, wiec metadane sa zwykle
//       identyczne. Payload nadpisany na zlepiony tekst, artifacts z
//       pierwszego inputu zachowane.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

pub struct CombineNodeAdapter;

impl CombineNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CombineNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Domyslny separator miedzy textami z poszczegolnych branchy. Operator
/// moze nadpisac w `node.config["separator"]`.
const DEFAULT_SEPARATOR: &str = "\n\n";

#[async_trait]
impl NodeAdapter for CombineNodeAdapter {
    fn node_type(&self) -> &str {
        "combine"
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        // Combine akceptuje wszystko (text, json, audio z pre-text bridge,
        // image z OCR itd.) — kazdy input mapowany na text representation
        // przez `flow_value_to_text`.
        vec![PortSpec::new("in", FlowDataType::Any)]
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Text)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        if inputs.is_empty() {
            return Err(anyhow!(
                "combine node '{}' has no incoming edges (need >=1)",
                node.id
            ));
        }

        let separator = node
            .config
            .get("separator")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_SEPARATOR);

        // Deterministyczna kolejnosc: po `from_node_id`, zeby ten sam zestaw
        // branchy zawsze laczyl sie tak samo. Inputs nie sa gwarantowanie
        // posortowane przez executor.
        let mut sorted: Vec<&NodeInput> = inputs.iter().collect();
        sorted.sort_by(|a, b| a.from_node_id.cmp(&b.from_node_id));

        let parts: Vec<String> = sorted
            .iter()
            .map(|inp| flow_value_to_text(&inp.envelope.payload))
            .collect();
        let joined = parts.join(separator);

        // Bierzemy envelope z pierwszego (po sortowaniu) brancha jako baze —
        // niesie session_id w meta, conversation context, artifacts. Pozostali
        // branche maja zwykle te same metadane (wspolny trigger), wiec ich
        // meta nie laczymy zeby uniknac konfliktu duplikatow.
        let mut out = (*sorted[0].envelope).clone();
        out.payload = FlowValue::Text(joined);
        Ok(out)
    }
}

/// Mapuje dowolny `FlowValue` na string. Dla typow blob-owych zwraca krotki
/// placeholder z mime, zeby downstream LLM widzial ze byl zalacznik bez
/// inline'owania bytes.
fn flow_value_to_text(v: &FlowValue) -> String {
    match v {
        FlowValue::Empty => String::new(),
        FlowValue::Text(s) => s.clone(),
        FlowValue::Json(j) => j.to_string(),
        FlowValue::Audio { mime, .. } => format!("<audio: {mime}>"),
        FlowValue::Image { mime, .. } => format!("<image: {mime}>"),
        FlowValue::Video { mime, .. } => format!("<video: {mime}>"),
        FlowValue::Embedding(values) => format!("<embedding: {} dims>", values.len()),
        FlowValue::Other { mime, filename, .. } => match filename {
            Some(name) => format!("<file: {name} ({mime})>"),
            None => format!("<file: {mime}>"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use std::sync::Arc;

    fn combine_node(separator: Option<&str>) -> FlowNode {
        let config = match separator {
            Some(s) => serde_json::json!({ "separator": s }),
            None => serde_json::Value::Null,
        };
        FlowNode {
            id: "c1".into(),
            node_type: "combine".into(),
            config,
            position: None,
            label: None,
        }
    }

    fn input(from_node_id: &str, payload: FlowValue) -> NodeInput {
        let mut env = FlowEnvelope::with_payload(payload);
        env.meta.insert(
            "session_id".into(),
            serde_json::json!("test-session-42"),
        );
        NodeInput {
            from_node_id: from_node_id.into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    #[tokio::test]
    async fn combine_joins_text_inputs_with_default_separator() {
        let adapter = CombineNodeAdapter::new();
        let inputs = vec![
            input("branch-a", FlowValue::Text("hello".into())),
            input("branch-b", FlowValue::Text("world".into())),
        ];
        let ctx = stub_ctx();
        let out = adapter
            .execute(&combine_node(None), &inputs, &ctx)
            .await
            .unwrap();
        assert_eq!(out.payload.as_text(), Some("hello\n\nworld"));
    }

    #[tokio::test]
    async fn combine_uses_custom_separator_from_config() {
        let adapter = CombineNodeAdapter::new();
        let inputs = vec![
            input("a", FlowValue::Text("x".into())),
            input("b", FlowValue::Text("y".into())),
            input("c", FlowValue::Text("z".into())),
        ];
        let ctx = stub_ctx();
        let out = adapter
            .execute(&combine_node(Some(" | ")), &inputs, &ctx)
            .await
            .unwrap();
        assert_eq!(out.payload.as_text(), Some("x | y | z"));
    }

    #[tokio::test]
    async fn combine_sorts_inputs_by_from_node_id_for_determinism() {
        let adapter = CombineNodeAdapter::new();
        let inputs = vec![
            input("z-last", FlowValue::Text("zzz".into())),
            input("a-first", FlowValue::Text("aaa".into())),
            input("m-mid", FlowValue::Text("mmm".into())),
        ];
        let ctx = stub_ctx();
        let out = adapter
            .execute(&combine_node(None), &inputs, &ctx)
            .await
            .unwrap();
        assert_eq!(out.payload.as_text(), Some("aaa\n\nmmm\n\nzzz"));
    }

    #[tokio::test]
    async fn combine_propagates_session_id_from_first_branch() {
        let adapter = CombineNodeAdapter::new();
        let inputs = vec![
            input("branch-a", FlowValue::Text("hi".into())),
            input("branch-b", FlowValue::Text("there".into())),
        ];
        let ctx = stub_ctx();
        let out = adapter
            .execute(&combine_node(None), &inputs, &ctx)
            .await
            .unwrap();
        assert_eq!(
            out.meta.get("session_id").and_then(|v| v.as_str()),
            Some("test-session-42")
        );
    }

    #[tokio::test]
    async fn combine_handles_mixed_payload_types() {
        let adapter = CombineNodeAdapter::new();
        let inputs = vec![
            input("branch-a", FlowValue::Text("transcript".into())),
            input(
                "branch-b",
                FlowValue::Other {
                    blob_ref: crate::flow_engine::blob_store::BlobRef {
                        id: "b1".into(),
                        sha256: "deadbeef".into(),
                        size_bytes: 100,
                        mime: "application/pdf".into(),
                    },
                    mime: "application/pdf".into(),
                    filename: Some("report.pdf".into()),
                },
            ),
        ];
        let ctx = stub_ctx();
        let out = adapter
            .execute(&combine_node(None), &inputs, &ctx)
            .await
            .unwrap();
        // Sorted: a (transcript), b (file placeholder)
        assert_eq!(
            out.payload.as_text(),
            Some("transcript\n\n<file: report.pdf (application/pdf)>")
        );
    }

    #[tokio::test]
    async fn combine_rejects_empty_inputs() {
        let adapter = CombineNodeAdapter::new();
        let ctx = stub_ctx();
        let err = adapter
            .execute(&combine_node(None), &[], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no incoming edges"));
    }

    #[test]
    fn combine_advertises_correct_ports_and_types() {
        let a = CombineNodeAdapter::new();
        assert_eq!(a.node_type(), "combine");
        let in_names: Vec<String> = a.input_ports().iter().map(|p| p.name.clone()).collect();
        let out_names: Vec<String> = a.output_ports().iter().map(|p| p.name.clone()).collect();
        assert_eq!(in_names, vec!["in"]);
        assert_eq!(out_names, vec!["full"]);
        assert_eq!(a.input_port_type("in"), FlowDataType::Any);
        assert_eq!(a.output_port_type("full"), FlowDataType::Text);
    }
}
