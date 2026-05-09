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
use crate::flow_engine::types::{FlowDataType, FlowNode};

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
// Sześć typed output portów (`text` / `audio` / `image` / `video` /
// `embedding` / `other`) plus jeden legacy `full` (typ `Any`).
//
// Typed porty: GUI rysuje kazdy w innym kolorze (typed via
// `output_port_type`), R8 walidacja edge'y wymusza ze krawedz z portu
// `audio` laczy sie tylko z node'm ktory deklaruje `input_port_type = Audio`
// (lub `Any`). Runtime: trigger emituje pojedynczy envelope (passthrough z
// `ctx.initial_envelope`); informacja o porcie sluzy walidacji compile-time
// + GUI rendering. Multi-modal payload w envelope niesie wszystkie typy z
// requestu, downstream node konsumuje swoja czesc.
//
// `other` to kanał dla plików ktore nie sa native media (PDF, DOCX, XLSX,
// ZIP itp.) — adapter konsumujacy musi czytac `FlowValue::Other.mime` zeby
// zdecydowac co z tym zrobic.
//
// `full` zostaje TYLKO jako compat-passthrough dla legacy seed flowów
// ktore wpisuja `from_port = "full"` (default `FlowEdge::from_port`). Po
// migracji wszystkich seedów + testów na typed porty `full` znika razem ze
// starymi flowami (Standardowy LLM/TTS, Audio Chat).
const OUTPUT_PORTS: &[&str] = &[
    "text",
    "audio",
    "image",
    "video",
    "embedding",
    "other",
    "full",
];

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

    fn output_port_type(&self, port: &str) -> FlowDataType {
        match port {
            "text" => FlowDataType::Text,
            "audio" => FlowDataType::Audio,
            "image" => FlowDataType::Image,
            "video" => FlowDataType::Video,
            "embedding" => FlowDataType::Embedding,
            "other" => FlowDataType::Other,
            // `full` to compat-passthrough — Any pasuje do kazdego konsumenta
            // niezaleznie od jego `input_port_type`, dzieki czemu legacy seedy
            // dzialaja bez zmian. Trafi do usuniecia razem ze starymi flowami.
            "full" => FlowDataType::Any,
            _ => FlowDataType::Any,
        }
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
    fn trigger_advertises_six_typed_output_ports_plus_legacy_full() {
        let a = TriggerNodeAdapter::new();
        assert!(a.supported_input_ports().is_empty());
        assert_eq!(
            a.supported_output_ports(),
            &["text", "audio", "image", "video", "embedding", "other", "full"]
        );
        assert_eq!(a.node_type(), "trigger");
        assert_eq!(a.output_port_type("text"), FlowDataType::Text);
        assert_eq!(a.output_port_type("audio"), FlowDataType::Audio);
        assert_eq!(a.output_port_type("image"), FlowDataType::Image);
        assert_eq!(a.output_port_type("video"), FlowDataType::Video);
        assert_eq!(a.output_port_type("embedding"), FlowDataType::Embedding);
        assert_eq!(a.output_port_type("other"), FlowDataType::Other);
        assert_eq!(a.output_port_type("full"), FlowDataType::Any);
        assert_eq!(a.output_port_type("unknown"), FlowDataType::Any);
    }
}
