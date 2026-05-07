// =============================================================================
// Plik: flow_engine/node_adapters/stt.rs
// Opis: SttNodeAdapter — transkrypcja audio z payload na tekst. Wymaga
//       FlowValue::Audio jako payloadu (BlobRef). Output: payload =
//       Text(transcript), oryginalny audio blob_ref ląduje w
//       artifacts['source_audio'] żeby downstream node mógł się odwołać.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::dispatchers::SttRequest;
use crate::flow_engine::envelope::{ArtifactProvenance, FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "stt";

pub struct SttNodeAdapter;

impl SttNodeAdapter {
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
            .get("stt_model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(m.to_string());
        }
        Err(anyhow!(
            "stt adapter: no model — node config 'model' nor envelope.meta['stt_model']"
        ))
    }

    /// Stage 3d-0b-4-fix: language/prompt/response_format priorytet
    /// `node.config` > `envelope.meta`. Operator pin'uje wartości w
    /// node config, ale request seedy mają fallback na meta.
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

impl Default for SttNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for SttNodeAdapter {
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
        FlowDataType::Audio
    }

    fn output_port_type(&self, _port: &str) -> FlowDataType {
        FlowDataType::Text
    }

    fn produced_artifacts(&self) -> &[(&'static str, FlowDataType)] {
        &[("source_audio", FlowDataType::Audio)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("stt adapter: missing input edge"))?;
        let envelope = &input.envelope;

        let (blob_ref, audio_mime, sample_rate) = match &envelope.payload {
            FlowValue::Audio {
                blob_ref,
                mime,
                sample_rate,
            } => (blob_ref.clone(), mime.clone(), *sample_rate),
            other => {
                return Err(anyhow!(
                    "stt adapter: payload must be Audio, got {}",
                    other.kind()
                ));
            }
        };

        let model = Self::pick_model(node, envelope)?;
        let language = Self::pick_optional_str(node, envelope, "language");
        let prompt = Self::pick_optional_str(node, envelope, "prompt");
        let temperature = Self::pick_optional_f32(node, envelope, "temperature");
        let response_format = Self::pick_optional_str(node, envelope, "response_format");

        let req = SttRequest {
            model,
            audio: blob_ref.clone(),
            language: language.clone(),
            prompt,
            temperature,
            response_format,
            user_id: ctx.user_id,
            user_role: ctx.user_role.clone(),
        };

        let response = ctx
            .stt
            .transcribe(req)
            .await
            .map_err(|e| anyhow!("stt adapter: dispatcher failed: {e}"))?;

        // Output envelope: payload Text(transcript), audio blob ląduje w
        // artifacts['source_audio']. Verbose pola (duration/segments/speakers)
        // lądują w meta — flow_outcome_to_stt_response je rozpakowuje gdy
        // klient prosił o response_format=verbose_json.
        let mut out: FlowEnvelope = (**envelope).clone();
        out.payload = FlowValue::Text(response.text);
        out.put_artifact(
            "source_audio",
            FlowValue::Audio {
                blob_ref,
                mime: audio_mime,
                sample_rate,
            },
            ArtifactProvenance {
                producer_node_id: node.id.clone(),
                producer_node_type: NODE_TYPE.to_string(),
                timestamp_ms: ctx.clock.now_ms(),
            },
        )
        .map_err(|e| anyhow!("stt adapter: {e}"))?;
        if let Some(lang) = response.detected_language {
            out.meta
                .insert("detected_language".into(), serde_json::Value::String(lang));
        }
        if let Some(dur) = response.duration {
            if let Some(num) = serde_json::Number::from_f64(dur as f64) {
                out.meta
                    .insert("duration".into(), serde_json::Value::Number(num));
            }
        }
        if let Some(segs_json) = response.segments_json {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&segs_json) {
                out.meta.insert("segments".into(), parsed);
            }
        }
        if let Some(sp_json) = response.speakers_json {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&sp_json) {
                out.meta.insert("speakers".into(), parsed);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::blob_store::BlobRef;
    use crate::flow_engine::dispatchers::{SttDispatcher, SttResponse};
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Arc;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "s1".into(),
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

    fn audio_envelope() -> FlowEnvelope {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Audio {
            blob_ref: BlobRef {
                id: "blob1".into(),
                size_bytes: 4,
                mime: "audio/wav".into(),
                sha256: "x".into(),
            },
            mime: "audio/wav".into(),
            sample_rate: Some(16_000),
        };
        env
    }

    struct FakeStt;
    #[async_trait]
    impl SttDispatcher for FakeStt {
        async fn transcribe(&self, req: SttRequest) -> Result<SttResponse> {
            assert_eq!(req.audio.id, "blob1");
            Ok(SttResponse {
                text: "transkrypcja".into(),
                detected_language: Some("pl".into()),
                ..SttResponse::default()
            })
        }
    }

    #[tokio::test]
    async fn transcribes_audio_and_writes_text_payload() {
        let mut ctx = stub_ctx();
        ctx.stt = Arc::new(FakeStt);
        let adapter = SttNodeAdapter::new();
        let out = adapter
            .execute(
                &node(json!({"model": "whisper"})),
                &[input(audio_envelope())],
                &ctx,
            )
            .await
            .unwrap();
        match out.payload {
            FlowValue::Text(t) => assert_eq!(t, "transkrypcja"),
            other => panic!("expected Text, got {other:?}"),
        }
        assert_eq!(
            out.meta.get("detected_language").and_then(|v| v.as_str()),
            Some("pl")
        );
        assert!(out.artifacts.contains_key("source_audio"));
    }

    #[tokio::test]
    async fn rejects_non_audio_payload() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("nope".into());
        let mut ctx = stub_ctx();
        ctx.stt = Arc::new(FakeStt);
        let err = SttNodeAdapter::new()
            .execute(&node(json!({"model": "w"})), &[input(env)], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must be Audio"));
    }
}
