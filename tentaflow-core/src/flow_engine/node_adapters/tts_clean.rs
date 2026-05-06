// =============================================================================
// Plik: flow_engine/node_adapters/tts_clean.rs
// Opis: TtsCleanNodeAdapter — czyści tekst przed TTS (emoji, skróty, fonetyka).
//       Plan v4.2 D3 — DbPool wycięty z adaptera, regex+cache+TTL siedzą w
//       impl `TtsCleaningStore`. Adapter widzi tylko clean(text) -> text.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter};
use crate::flow_engine::types::FlowNode;

pub struct TtsCleanNodeAdapter;

impl TtsCleanNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TtsCleanNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

const INPUT_PORTS: &[&str] = &["in"];
const OUTPUT_PORTS: &[&str] = &["full"];

#[async_trait]
impl NodeAdapter for TtsCleanNodeAdapter {
    fn node_type(&self) -> &str {
        "tts_clean"
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
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("tts_clean node requires exactly 1 input edge"))?;

        let mut out = (*input.envelope).clone();
        let text = match &out.payload {
            FlowValue::Text(t) => t.clone(),
            // Non-text payload — passthrough bez transformacji.
            _ => return Ok(out),
        };

        let cleaned = ctx.tts_cleaning.clean(&text).await?;
        out.payload = FlowValue::Text(cleaned);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::dispatchers::tts_cleaning::TtsCleaningStore;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use anyhow::Result as AnyResult;
    use async_trait::async_trait;
    use std::sync::Arc;

    struct FakeCleaning;
    #[async_trait]
    impl TtsCleaningStore for FakeCleaning {
        async fn clean(&self, text: &str) -> AnyResult<String> {
            // Symuluje strip emoji + lowercase trim — adaptery testują
            // integrację, nie logikę cleaning'u (ta jest w impl).
            Ok(text.replace("🎉", "").trim().to_lowercase())
        }
    }

    fn tts_node() -> FlowNode {
        FlowNode {
            id: "ttsc-1".into(),
            node_type: "tts_clean".into(),
            config: serde_json::Value::Null,
            position: None,
            label: None,
        }
    }

    fn make_input(env: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "src".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    #[tokio::test]
    async fn tts_clean_applies_cleaning_to_text_payload() {
        let mut ctx = stub_ctx();
        ctx.tts_cleaning = Arc::new(FakeCleaning);

        let env = FlowEnvelope::with_payload(FlowValue::Text("  Hello 🎉 World  ".into()));
        let out = TtsCleanNodeAdapter
            .execute(&tts_node(), &[make_input(env)], &ctx)
            .await
            .unwrap();
        assert_eq!(out.payload.as_text(), Some("hello  world"));
    }

    #[tokio::test]
    async fn tts_clean_no_op_on_non_text_payload() {
        let env = FlowEnvelope::with_payload(FlowValue::Embedding(vec![0.5]));
        let out = TtsCleanNodeAdapter
            .execute(&tts_node(), &[make_input(env)], &stub_ctx())
            .await
            .unwrap();
        assert!(matches!(out.payload, FlowValue::Embedding(_)));
    }

    #[test]
    fn tts_clean_advertises_full_ports() {
        let a = TtsCleanNodeAdapter;
        assert_eq!(a.supported_input_ports(), &["in"]);
        assert_eq!(a.supported_output_ports(), &["full"]);
    }
}
