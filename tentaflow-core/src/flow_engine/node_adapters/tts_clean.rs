// =============================================================================
// Plik: flow_engine/node_adapters/tts_clean.rs
// Opis: TtsCleanNodeAdapter — czyści tekst przed TTS (emoji, skróty, fonetyka).
//       Plan v4.2 D3 — DbPool wycięty z adaptera, regex+cache+TTL siedzą w
//       impl `TtsCleaningStore`. Adapter widzi tylko clean(text) -> text.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};

use crate::flow_engine::envelope::{
    EnvelopeDelta, EnvelopeDeltaKind, FlowEnvelope, FlowValue, LlmStreamChunk, NodeInput,
};
use crate::flow_engine::node_adapter::{
    ExecutionContext, NodeAdapter, PortSpec, StreamingNodeAdapter,
};
use crate::flow_engine::types::{FlowDataType, FlowNode};

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

#[async_trait]
impl NodeAdapter for TtsCleanNodeAdapter {
    fn node_type(&self) -> &str {
        "tts_clean"
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Text)]
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        vec![
            PortSpec::new("full", FlowDataType::Text),
            PortSpec::new("stream", FlowDataType::Text),
        ]
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

/// Streaming wariant — czyści każdą deltę tekstu (zazwyczaj całe zdanie, gdy
/// node siedzi za `sentence_buffer`) przez `ctx.tts_cleaning.clean()` zanim
/// trafi do TTS. Llm→Llm: nie zmienia kindu, tylko transformuje text_delta.
/// Cancel sprawdzany przed każdym blocking clean.
#[async_trait]
impl StreamingNodeAdapter for TtsCleanNodeAdapter {
    fn stream_input_kind(&self) -> EnvelopeDeltaKind {
        EnvelopeDeltaKind::Llm
    }
    fn stream_output_kind(&self) -> EnvelopeDeltaKind {
        EnvelopeDeltaKind::Llm
    }

    async fn process_stream(
        &self,
        _node: &FlowNode,
        upstream: BoxStream<'static, Result<EnvelopeDelta>>,
        _seed_envelope: std::sync::Arc<FlowEnvelope>,
        ctx: &ExecutionContext,
    ) -> Result<BoxStream<'static, Result<EnvelopeDelta>>> {
        let cancel = ctx.cancel_token.clone();
        let cleaning = ctx.tts_cleaning.clone();

        let stream = futures::stream::unfold(upstream, move |mut upstream| {
            let cancel = cancel.clone();
            let cleaning = cleaning.clone();
            async move {
                if cancel.is_cancelled() {
                    return None;
                }
                match upstream.next().await {
                    Some(Ok(EnvelopeDelta::Llm(chunk))) => {
                        let cleaned = if chunk.text_delta.is_empty() {
                            String::new()
                        } else {
                            match cleaning.clean(&chunk.text_delta).await {
                                Ok(c) => c,
                                Err(e) => {
                                    return Some((Err(anyhow!("tts_clean stream: {e}")), upstream))
                                }
                            }
                        };
                        Some((
                            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                                choice_index: chunk.choice_index,
                                text_delta: cleaned,
                                finish_reason: chunk.finish_reason,
                                ..Default::default()
                            })),
                            upstream,
                        ))
                    }
                    Some(other) => Some((other, upstream)),
                    None => None,
                }
            }
        });

        Ok(stream.boxed())
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
    fn tts_clean_advertises_full_and_stream_ports() {
        let a = TtsCleanNodeAdapter;
        let in_names: Vec<String> = a.input_ports().iter().map(|p| p.name.clone()).collect();
        let out_names: Vec<String> = a.output_ports().iter().map(|p| p.name.clone()).collect();
        assert_eq!(in_names, vec!["in"]);
        assert_eq!(out_names, vec!["full", "stream"]);
    }

    #[tokio::test]
    async fn tts_clean_streaming_cleans_each_delta() {
        let mut ctx = stub_ctx();
        ctx.tts_cleaning = Arc::new(FakeCleaning);

        let upstream = futures::stream::iter(vec![
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: " First 🎉 Sentence. ".into(),
                ..Default::default()
            })),
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: " Second 🎉 One. ".into(),
                finish_reason: Some(crate::flow_engine::envelope::FinishReason::Stop),
                ..Default::default()
            })),
        ])
        .boxed();

        let seed = Arc::new(FlowEnvelope::empty());
        let mut out = TtsCleanNodeAdapter
            .process_stream(
                &FlowNode {
                    id: "ttsc".into(),
                    node_type: "tts_clean".into(),
                    config: serde_json::Value::Null,
                    position: None,
                    label: None,
                },
                upstream,
                seed,
                &ctx,
            )
            .await
            .unwrap();

        let mut got = Vec::new();
        while let Some(item) = out.next().await {
            if let EnvelopeDelta::Llm(c) = item.unwrap() {
                got.push((c.text_delta, c.finish_reason));
            }
        }
        // FakeCleaning strip emoji + lowercase + trim — per delta.
        assert_eq!(got[0].0, "first  sentence.");
        assert_eq!(got[1].0, "second  one.");
        assert_eq!(
            got[1].1,
            Some(crate::flow_engine::envelope::FinishReason::Stop)
        );
    }
}
