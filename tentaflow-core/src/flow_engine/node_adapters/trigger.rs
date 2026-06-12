// =============================================================================
// Plik: flow_engine/node_adapters/trigger.rs
// Opis: TriggerNodeAdapter — punkt wejścia flow. Brak input edge'a; bierze
//       envelope z `ctx.initial_envelope` (seed dostarczony przez routing
//       przed `execute_blocking`/`execute_streaming`). Plan v4.2 D2.
// =============================================================================

use std::collections::HashSet;

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
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

// Sześć typed output portów per modality: `text` / `audio` / `image` /
// `video` / `embedding` / `other`. Każde wyjście emituje typed `FlowDataType`,
// R8 walidacja krawedzi wymusza ze krawedz z portu `audio` laczy sie tylko
// z node'm o input_port_type = Audio (lub Any). Runtime: trigger emituje
// pojedynczy envelope (passthrough z `ctx.initial_envelope`); informacja o
// porcie sluzy walidacji compile-time + GUI rendering. Multi-modal payload
// w envelope niesie wszystkie typy z requestu, downstream node konsumuje
// swoja czesc.
//
// `other` to kanał dla plików ktore nie sa native media (PDF, DOCX, XLSX,
// ZIP itp.) — adapter konsumujacy musi czytac `FlowValue::Other.mime`.

#[async_trait]
impl NodeAdapter for TriggerNodeAdapter {
    fn node_type(&self) -> &str {
        "trigger"
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        Vec::new()
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        vec![
            PortSpec::new("text", FlowDataType::Text),
            PortSpec::new("audio", FlowDataType::Audio),
            PortSpec::new("image", FlowDataType::Image),
            PortSpec::new("video", FlowDataType::Video),
            PortSpec::new("embedding", FlowDataType::Embedding),
            PortSpec::new("other", FlowDataType::Other),
        ]
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

    /// Bramkowanie gałęzi po modalności (§3.11 A): aktywne są tylko porty
    /// odpowiadające modalnościom obecnym w envelope — payload + artefakty
    /// `input_*` (multimodalny seed: pierwsze wejście → payload, kolejne →
    /// artefakty `input_{n}`, patrz `flow_envelope_from_inputs`). Payload Text
    /// nie aktywuje gałęzi `audio` (STT), payload Audio nie aktywuje gałęzi
    /// `text` itd. Envelope bez żadnej modalności (Empty seed, np. synthetic
    /// flow bez payloadu) nie bramkuje — `None` = wszystkie porty aktywne.
    fn active_output_ports(
        &self,
        _node: &FlowNode,
        result: &FlowEnvelope,
    ) -> Option<HashSet<String>> {
        let mut ports = HashSet::new();
        if let Some(p) = modality_port(&result.payload) {
            ports.insert(p.to_string());
        }
        for (name, value) in &result.artifacts {
            if name.starts_with("input_") {
                if let Some(p) = modality_port(value) {
                    ports.insert(p.to_string());
                }
            }
        }
        if ports.is_empty() {
            None
        } else {
            Some(ports)
        }
    }
}

/// Mapuje wariant `FlowValue` na nazwę output portu triggera. `Json` idzie
/// kanałem `text` (structured data konsumowane przez gałęzie tekstowe, nie
/// plikowe). `Empty` nie niesie modalności — brak portu.
fn modality_port(value: &FlowValue) -> Option<&'static str> {
    match value {
        FlowValue::Empty => None,
        FlowValue::Text(_) | FlowValue::Json(_) => Some("text"),
        FlowValue::Audio { .. } => Some("audio"),
        FlowValue::Image { .. } => Some("image"),
        FlowValue::Video { .. } => Some("video"),
        FlowValue::Embedding(_) => Some("embedding"),
        FlowValue::Other { .. } => Some("other"),
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
            region: None,
        }
    }

    #[tokio::test]
    async fn trigger_emits_clone_of_initial_envelope() {
        let mut env = FlowEnvelope::with_payload(FlowValue::Text("hi".into()));
        env.meta.insert("model".into(), serde_json::json!("gpt-4"));
        let ctx = stub_ctx_with_initial(env);

        let adapter = TriggerNodeAdapter::new();
        let out = adapter.execute(&trigger_node(), &[], &ctx).await.unwrap();
        assert_eq!(out.payload.as_text(), Some("hi"));
        assert_eq!(
            out.meta.get("model").and_then(|v| v.as_str()),
            Some("gpt-4")
        );
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
    fn trigger_advertises_six_typed_output_ports() {
        let a = TriggerNodeAdapter::new();
        assert!(a.input_ports().is_empty());
        let names: Vec<String> = a.output_ports().iter().map(|p| p.name.clone()).collect();
        assert_eq!(
            names,
            vec!["text", "audio", "image", "video", "embedding", "other"]
        );
        assert_eq!(a.node_type(), "trigger");
        assert_eq!(a.output_port_type("text"), FlowDataType::Text);
        assert_eq!(a.output_port_type("audio"), FlowDataType::Audio);
        assert_eq!(a.output_port_type("image"), FlowDataType::Image);
        assert_eq!(a.output_port_type("video"), FlowDataType::Video);
        assert_eq!(a.output_port_type("embedding"), FlowDataType::Embedding);
        assert_eq!(a.output_port_type("other"), FlowDataType::Other);
        assert_eq!(a.output_port_type("unknown"), FlowDataType::Any);
    }

    fn audio_value() -> FlowValue {
        FlowValue::Audio {
            blob_ref: crate::flow_engine::blob_store::BlobRef {
                id: "b1".into(),
                sha256: "deadbeef".into(),
                size_bytes: 4,
                mime: "audio/wav".into(),
            },
            mime: "audio/wav".into(),
            sample_rate: Some(16_000),
        }
    }

    #[test]
    fn text_payload_activates_only_text_port() {
        let a = TriggerNodeAdapter::new();
        let env = FlowEnvelope::with_payload(FlowValue::Text("hi".into()));
        let ports = a.active_output_ports(&trigger_node(), &env).unwrap();
        assert_eq!(ports, HashSet::from(["text".to_string()]));
    }

    #[test]
    fn audio_payload_activates_only_audio_port() {
        let a = TriggerNodeAdapter::new();
        let env = FlowEnvelope::with_payload(audio_value());
        let ports = a.active_output_ports(&trigger_node(), &env).unwrap();
        assert_eq!(ports, HashSet::from(["audio".to_string()]));
    }

    #[test]
    fn multimodal_seed_activates_port_per_modality() {
        let a = TriggerNodeAdapter::new();
        let mut env = FlowEnvelope::with_payload(FlowValue::Text("hi".into()));
        env.artifacts.insert("input_0".into(), audio_value());
        let ports = a.active_output_ports(&trigger_node(), &env).unwrap();
        assert_eq!(
            ports,
            HashSet::from(["text".to_string(), "audio".to_string()])
        );
    }

    #[test]
    fn non_input_artifacts_do_not_activate_ports() {
        let a = TriggerNodeAdapter::new();
        let mut env = FlowEnvelope::with_payload(FlowValue::Text("hi".into()));
        env.artifacts.insert("memory_hits".into(), audio_value());
        let ports = a.active_output_ports(&trigger_node(), &env).unwrap();
        assert_eq!(ports, HashSet::from(["text".to_string()]));
    }

    #[test]
    fn empty_payload_does_not_gate_any_port() {
        let a = TriggerNodeAdapter::new();
        let env = FlowEnvelope::empty();
        assert_eq!(a.active_output_ports(&trigger_node(), &env), None);
    }
}
