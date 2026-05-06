// =============================================================================
// Plik: flow_engine/node_adapters/embeddings.rs
// Opis: EmbeddingsNodeAdapter — wektoryzacja tekstu z payload. Single-input
//       (Text) → FlowValue::Embedding. Wsparcie batch (JSON tablica tekstów
//       w payload albo w artifacts['inputs']) zostaje na stage 2 razem z
//       Many cardinality.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::dispatchers::EmbeddingsRequest;
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "embeddings";

pub struct EmbeddingsNodeAdapter;

impl EmbeddingsNodeAdapter {
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
            .get("embeddings_model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return Ok(m.to_string());
        }
        Err(anyhow!(
            "embeddings adapter: no model — node config 'model' nor envelope.meta['embeddings_model']"
        ))
    }

    fn payload_text(envelope: &FlowEnvelope) -> Result<String> {
        match &envelope.payload {
            FlowValue::Text(t) if !t.is_empty() => Ok(t.clone()),
            FlowValue::Text(_) | FlowValue::Empty => {
                Err(anyhow!("embeddings adapter: empty input text"))
            }
            other => Err(anyhow!(
                "embeddings adapter: payload must be Text, got {}",
                other.kind()
            )),
        }
    }
}

impl Default for EmbeddingsNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for EmbeddingsNodeAdapter {
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
        // Single-input case zwraca Embedding. Batch case (Json) wraca w
        // Etap 3 razem z cardinality > 1.
        FlowDataType::Embedding
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("embeddings adapter: missing input edge"))?;
        let envelope = &input.envelope;

        let model = Self::pick_model(node, envelope)?;
        let text = Self::payload_text(envelope)?;
        let dimensions = node
            .config
            .get("dimensions")
            .and_then(|v| v.as_u64())
            .or_else(|| envelope.meta.get("dimensions").and_then(|v| v.as_u64()))
            .map(|n| n as u32);
        let encoding_format = node
            .config
            .get("encoding_format")
            .and_then(|v| v.as_str())
            .or_else(|| {
                envelope
                    .meta
                    .get("encoding_format")
                    .and_then(|v| v.as_str())
            })
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let req = EmbeddingsRequest {
            model,
            inputs: vec![text],
            dimensions,
            encoding_format,
            user_id: ctx.user_id,
            user_role: ctx.user_role.clone(),
        };

        let response = ctx
            .embeddings
            .embed(req)
            .await
            .map_err(|e| anyhow!("embeddings adapter: dispatcher failed: {e}"))?;

        let vector = response
            .vectors
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("embeddings adapter: backend returned 0 vectors"))?;

        ctx.usage_sink.record(&node.id, response.usage);
        // Empty vector też jest legalnym wynikiem? Backend zwracający pustkę
        // łamie kontrakt — adapter sygnalizuje błędem. Per OpenAI surface
        // /v1/embeddings nigdy nie zwraca pustego wektora dla niepustego
        // inputu.
        if vector.is_empty() {
            return Err(anyhow!("embeddings adapter: backend returned empty vector"));
        }

        let mut out: FlowEnvelope = (**envelope).clone();
        out.payload = FlowValue::Embedding(vector);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::dispatchers::{EmbeddingsDispatcher, EmbeddingsResponse};
    use crate::flow_engine::envelope::TokenUsage;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "e1".into(),
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

    struct FakeEmbeddings {
        last_input: Mutex<Option<String>>,
        vector: Vec<f32>,
    }

    #[async_trait]
    impl EmbeddingsDispatcher for FakeEmbeddings {
        async fn embed(
            &self,
            req: crate::flow_engine::dispatchers::EmbeddingsRequest,
        ) -> Result<EmbeddingsResponse> {
            *self.last_input.lock().unwrap() = req.inputs.first().cloned();
            Ok(EmbeddingsResponse {
                vectors: vec![self.vector.clone()],
                usage: TokenUsage {
                    prompt_tokens: 5,
                    completion_tokens: 0,
                    total_tokens: 5,
                },
            })
        }
    }

    #[tokio::test]
    async fn embeds_text_payload_into_embedding_value() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("hello world".into());
        let mut ctx = stub_ctx();
        let fake = Arc::new(FakeEmbeddings {
            last_input: Mutex::new(None),
            vector: vec![0.1, 0.2, 0.3],
        });
        ctx.embeddings = fake.clone();

        let adapter = EmbeddingsNodeAdapter::new();
        let out = adapter
            .execute(&node(json!({"model": "m"})), &[input(env)], &ctx)
            .await
            .unwrap();

        match out.payload {
            FlowValue::Embedding(v) => assert_eq!(v, vec![0.1, 0.2, 0.3]),
            other => panic!("expected Embedding, got {other:?}"),
        }
        assert_eq!(
            fake.last_input.lock().unwrap().as_deref(),
            Some("hello world")
        );
    }

    #[tokio::test]
    async fn rejects_non_text_payload() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Empty;
        let ctx = stub_ctx();
        let adapter = EmbeddingsNodeAdapter::new();
        let err = adapter
            .execute(&node(json!({"model": "m"})), &[input(env)], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("empty input"));
    }
}
