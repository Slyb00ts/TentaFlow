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
use crate::flow_engine::types::{FlowDataType, FlowNode};

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

    /// Etap 2: priorytet `node.config` > `envelope.meta`. Pierwsze pasuje gdy
    /// operator pin'uje konkretne ustawienie w node config flow; drugie gdy
    /// wartość przyszła z request seed (TTS-as-flow, route_chat audio_input
    /// w przyszłości itp.).
    fn pick_optional_str(node: &FlowNode, envelope: &FlowEnvelope, key: &str) -> Option<String> {
        if let Some(s) = node
            .config
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Some(s.to_string());
        }
        envelope
            .meta
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    }

    /// Stage 3d-0b-2-fix: helper dla numerycznych pól (np. speed). Akceptuje
    /// JSON Number (zarówno f64 jak i i64).
    fn pick_optional_f32(node: &FlowNode, envelope: &FlowEnvelope, key: &str) -> Option<f32> {
        if let Some(n) = node.config.get(key).and_then(|v| v.as_f64()) {
            return Some(n as f32);
        }
        envelope
            .meta
            .get(key)
            .and_then(|v| v.as_f64())
            .map(|n| n as f32)
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

    fn input_port_type(&self, _port: &str) -> FlowDataType {
        FlowDataType::Text
    }

    fn output_port_type(&self, _port: &str) -> FlowDataType {
        FlowDataType::Audio
    }

    fn produced_artifacts(&self) -> &[(&'static str, FlowDataType)] {
        &[("source_text", FlowDataType::Text)]
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
            voice: Self::pick_optional_str(node, envelope, "voice"),
            format: Self::pick_optional_str(node, envelope, "format"),
            language: Self::pick_optional_str(node, envelope, "language"),
            speed: Self::pick_optional_f32(node, envelope, "speed"),
            user_id: ctx.user_id,
            user_role: ctx.user_role.clone(),
            cancel_token: ctx.cancel_token.clone(),
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
        async fn stream_synthesize(
            &self,
            req: TtsRequest,
        ) -> Result<futures::stream::BoxStream<'static, Result<crate::flow_engine::dispatchers::TtsStreamChunk>>> {
            *self.last.lock().unwrap() = Some(req);
            let chunk = crate::flow_engine::dispatchers::TtsStreamChunk {
                bytes_delta: vec![0u8; 8],
                mime: "audio/wav".into(),
                sample_rate: Some(22_050),
                finish_reason: Some(crate::flow_engine::envelope::FinishReason::Stop),
            };
            Ok(Box::pin(futures::stream::once(async move { Ok(chunk) })))
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
    async fn speed_propagated_from_envelope_meta() {
        // Stage 3d-0b-2-fix: speed seedowane z requestu do envelope.meta
        // (przez tts_request_to_initial_envelope) → adapter czyta z meta.
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("hello".into());
        env.meta.insert("speed".into(), json!(1.5));
        let mut ctx = stub_ctx();
        let fake = Arc::new(FakeTts { last: Mutex::new(None) });
        ctx.tts = fake.clone();

        TtsNodeAdapter::new()
            .execute(&node(json!({"model": "m"})), &[input(env)], &ctx)
            .await
            .unwrap();

        let last = fake.last.lock().unwrap();
        assert_eq!(last.as_ref().unwrap().speed, Some(1.5));
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
