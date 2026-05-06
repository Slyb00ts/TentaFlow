// =============================================================================
// Plik: flow_engine/node_adapters/tts.rs
// Opis: TtsNodeAdapter — synteza mowy z payload.Text. Output: payload =
//       FlowValue::Audio (BlobRef + mime + sample_rate). Tekst źródłowy
//       trafia do artifacts['source_text'] żeby downstream node mógł
//       odwołać się do oryginału (np. log / debug widok).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::dispatchers::TtsRequest;
use crate::flow_engine::envelope::{ArtifactProvenance, FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter};
use crate::flow_engine::types::FlowNode;

const NODE_TYPE: &str = "tts";

pub struct TtsNodeAdapter;

impl TtsNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    fn pick_model(node: &FlowNode, envelope: &FlowEnvelope) -> Result<String> {
        if let Some(m) = node
            .config
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(m.to_string());
        }
        if let Some(m) = envelope
            .meta
            .get("tts_model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(m.to_string());
        }
        Err(anyhow!(
            "tts adapter: no model — node config 'model' nor envelope.meta['tts_model']"
        ))
    }

    fn pick_optional_str(node: &FlowNode, key: &str) -> Option<String> {
        node.config
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }
}

impl Default for TtsNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for TtsNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn supported_input_ports(&self) -> &[&'static str] {
        &["in"]
    }
    fn supported_output_ports(&self) -> &[&'static str] {
        &["full"]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("tts adapter: missing input edge"))?;
        let envelope = &input.envelope;

        let text = match &envelope.payload {
            FlowValue::Text(t) if !t.is_empty() => t.clone(),
            FlowValue::Text(_) | FlowValue::Empty => {
                return Err(anyhow!("tts adapter: empty input text"));
            }
            other => {
                return Err(anyhow!(
                    "tts adapter: payload must be Text, got {}",
                    other.kind()
                ));
            }
        };

        let req = TtsRequest {
            model: Self::pick_model(node, envelope)?,
            text: text.clone(),
            voice: Self::pick_optional_str(node, "voice"),
            format: Self::pick_optional_str(node, "format"),
            user_id: ctx.user_id,
            user_role: ctx.user_role.clone(),
        };

        let response = ctx
            .tts
            .synthesize(req)
            .await
            .map_err(|e| anyhow!("tts adapter: dispatcher failed: {e}"))?;

        let mut out: FlowEnvelope = (**envelope).clone();
        out.payload = FlowValue::Audio {
            blob_ref: response.audio,
            mime: response.mime,
            sample_rate: response.sample_rate,
        };
        out.put_artifact(
            "source_text",
            FlowValue::Text(text),
            ArtifactProvenance {
                producer_node_id: node.id.clone(),
                producer_node_type: NODE_TYPE.to_string(),
                timestamp_ms: ctx.clock.now_ms(),
            },
        )
        .map_err(|e| anyhow!("tts adapter: {e}"))?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::blob_store::BlobRef;
    use crate::flow_engine::dispatchers::{TtsDispatcher, TtsResponse};
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "t1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
        }
    }

    fn input(envelope: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "trigger".into(),
            from_port: "full".into(),
            envelope: Arc::new(envelope),
        }
    }

    struct FakeTts {
        last: Mutex<Option<TtsRequest>>,
    }

    #[async_trait]
    impl TtsDispatcher for FakeTts {
        async fn synthesize(&self, req: TtsRequest) -> Result<TtsResponse> {
            *self.last.lock().unwrap() = Some(req);
            Ok(TtsResponse {
                audio: BlobRef {
                    id: "out-blob".into(),
                    size_bytes: 100,
                    mime: "audio/wav".into(),
                    sha256: "y".into(),
                },
                mime: "audio/wav".into(),
                sample_rate: Some(22_050),
            })
        }
    }

    #[tokio::test]
    async fn synthesizes_text_payload_into_audio_value() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("hello".into());
        let mut ctx = stub_ctx();
        let fake = Arc::new(FakeTts {
            last: Mutex::new(None),
        });
        ctx.tts = fake.clone();

        let out = TtsNodeAdapter::new()
            .execute(
                &node(json!({"model": "m", "voice": "alloy"})),
                &[input(env)],
                &ctx,
            )
            .await
            .unwrap();

        match out.payload {
            FlowValue::Audio {
                blob_ref,
                sample_rate,
                ..
            } => {
                assert_eq!(blob_ref.id, "out-blob");
                assert_eq!(sample_rate, Some(22_050));
            }
            other => panic!("expected Audio, got {other:?}"),
        }
        let last = fake.last.lock().unwrap();
        assert_eq!(last.as_ref().unwrap().voice.as_deref(), Some("alloy"));
        assert_eq!(last.as_ref().unwrap().text, "hello");
        match out.artifacts.get("source_text") {
            Some(FlowValue::Text(s)) => assert_eq!(s, "hello"),
            other => panic!("expected source_text Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn rejects_non_text_payload() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Empty;
        let mut ctx = stub_ctx();
        ctx.tts = Arc::new(FakeTts {
            last: Mutex::new(None),
        });
        let err = TtsNodeAdapter::new()
            .execute(&node(json!({"model": "m"})), &[input(env)], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("empty input"));
    }
}
