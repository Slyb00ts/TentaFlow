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
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
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
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Text)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        // Single-input case zwraca Embedding. Batch case (Json) wraca w
        // Etap 3 razem z cardinality > 1.
        vec![PortSpec::new("full", FlowDataType::Embedding)]
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
        // Stage 3d-0b-3-fix: batch path — envelope.meta["embeddings_inputs"]
        // (JSON array) seedowane przez embeddings_request_to_initial_envelope
        // dla EmbeddingInput::Multiple. Single-input fallback do payload.
        let inputs: Vec<String> = match envelope
            .meta
            .get("embeddings_inputs")
            .and_then(|v| v.as_array())
        {
            Some(arr) if !arr.is_empty() => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            _ => vec![Self::payload_text(envelope)?],
        };
        if inputs.is_empty() {
            return Err(anyhow!("embeddings adapter: zero inputs"));
        }
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
        let user = envelope
            .meta
            .get("embeddings_user")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let extra = envelope
            .meta
            .get("embeddings_extra")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let batch_count = inputs.len();
        let req = EmbeddingsRequest {
            model,
            inputs,
            dimensions,
            encoding_format,
            user_id: ctx.user_id.clone(),
            user_role: ctx.user_role.clone(),
            user,
            extra,
            flow_depth: ctx.subflow_depth,
            // §2.5 — the run's stamp, from `ctx`, never from `envelope.meta`.
            provenance: ctx.provenance(),
        };

        let response = ctx
            .embeddings
            .embed(req)
            .await
            .map_err(|e| anyhow!("embeddings adapter: dispatcher failed: {e}"))?;

        if response.vectors.is_empty() {
            return Err(anyhow!("embeddings adapter: backend returned 0 vectors"));
        }
        if response.vectors.iter().any(|v| v.is_empty()) {
            return Err(anyhow!("embeddings adapter: backend returned empty vector"));
        }

        ctx.usage_sink.record(&node.id, response.usage);

        let mut out: FlowEnvelope = (**envelope).clone();
        // RAG E2.1 — zachowaj ORYGINALNY tekst zapytania w meta ZANIM payload
        // zostanie zamieniony na Embedding. Po tym węźle tekst pytania ginie
        // (payload = wektor), a reranker (vector→reranker→combine) potrzebuje go
        // jako `query` do cross-encodera. Meta propaguje się przez vector→reranker
        // (każdy klonuje envelope bazowy). Stash tylko dla single-input path —
        // batch embeddingów nie ma jednego "query text".
        if batch_count == 1 {
            if let FlowValue::Text(t) = &envelope.payload {
                if !t.is_empty() {
                    out.meta.insert(
                        "rag_query_text".to_string(),
                        serde_json::Value::String(t.clone()),
                    );
                }
            }
        }
        if batch_count > 1 || response.vectors.len() > 1 {
            // Batch payload — Json shape akceptowany przez
            // converter::flow_outcome_to_embedding_response::parse_embedding_batch.
            let arr: Vec<serde_json::Value> = response
                .vectors
                .into_iter()
                .map(|v| {
                    serde_json::Value::Array(
                        v.into_iter()
                            .filter_map(|f| serde_json::Number::from_f64(f as f64))
                            .map(serde_json::Value::Number)
                            .collect(),
                    )
                })
                .collect();
            out.payload = FlowValue::Json(serde_json::json!({ "embeddings": arr }));
        } else {
            // Single-vector legacy path — payload Embedding bezpośrednio.
            out.payload = FlowValue::Embedding(response.vectors.into_iter().next().unwrap());
        }
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
            region: None,
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

    /// Captures the full dispatcher request so vendor passthrough fields can
    /// be asserted, not just the input text.
    struct CaptureEmbeddings {
        last: Mutex<Option<crate::flow_engine::dispatchers::EmbeddingsRequest>>,
    }

    #[async_trait]
    impl EmbeddingsDispatcher for CaptureEmbeddings {
        async fn embed(
            &self,
            req: crate::flow_engine::dispatchers::EmbeddingsRequest,
        ) -> Result<EmbeddingsResponse> {
            *self.last.lock().unwrap() = Some(req);
            Ok(EmbeddingsResponse {
                vectors: vec![vec![1.0]],
                usage: TokenUsage::default(),
            })
        }
    }

    /// `/v1/embeddings` seeds `dimensions`, `user` and unknown vendor fields
    /// into envelope.meta; the adapter must hand them to the dispatcher so an
    /// HTTP backend receives them verbatim.
    #[tokio::test]
    async fn forwards_dimensions_user_and_vendor_fields_from_meta() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("hello".into());
        env.meta.insert("dimensions".into(), json!(256));
        env.meta
            .insert("embeddings_user".into(), json!("end-user-7"));
        env.meta.insert(
            "embeddings_extra".into(),
            json!({"truncate": "END", "input_type": "query"}),
        );
        let mut ctx = stub_ctx();
        let fake = Arc::new(CaptureEmbeddings {
            last: Mutex::new(None),
        });
        ctx.embeddings = fake.clone();

        EmbeddingsNodeAdapter::new()
            .execute(&node(json!({"model": "m"})), &[input(env)], &ctx)
            .await
            .unwrap();

        let req = fake.last.lock().unwrap().take().expect("dispatcher called");
        assert_eq!(req.dimensions, Some(256));
        assert_eq!(req.user.as_deref(), Some("end-user-7"));
        assert_eq!(req.extra.get("truncate"), Some(&json!("END")));
        assert_eq!(req.extra.get("input_type"), Some(&json!("query")));
    }

    /// Stage 3d-0b-3-fix: batch embeddings przez envelope.meta["embeddings_inputs"].
    /// FakeEmbeddings zwraca jeden wektor per input — adapter musi sklejać
    /// w FlowValue::Json {embeddings:[...]} zamiast pojedynczego Embedding.
    struct FakeBatchEmbeddings {
        last_inputs: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl EmbeddingsDispatcher for FakeBatchEmbeddings {
        async fn embed(
            &self,
            req: crate::flow_engine::dispatchers::EmbeddingsRequest,
        ) -> Result<EmbeddingsResponse> {
            *self.last_inputs.lock().unwrap() = req.inputs.clone();
            // 1 wektor per input — symuluje normalny backend.
            let vectors: Vec<Vec<f32>> = req
                .inputs
                .iter()
                .enumerate()
                .map(|(i, _)| vec![i as f32 + 0.1])
                .collect();
            Ok(EmbeddingsResponse {
                vectors,
                usage: TokenUsage::default(),
            })
        }
    }

    #[tokio::test]
    async fn batch_inputs_propagate_through_meta_and_emit_json_payload() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("first".into());
        env.meta.insert(
            "embeddings_inputs".into(),
            serde_json::json!(["first", "second", "third"]),
        );
        let mut ctx = stub_ctx();
        let fake = Arc::new(FakeBatchEmbeddings {
            last_inputs: Mutex::new(Vec::new()),
        });
        ctx.embeddings = fake.clone();

        let out = EmbeddingsNodeAdapter::new()
            .execute(&node(json!({"model": "m"})), &[input(env)], &ctx)
            .await
            .unwrap();

        let recorded = fake.last_inputs.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![
                "first".to_string(),
                "second".to_string(),
                "third".to_string()
            ]
        );

        match out.payload {
            FlowValue::Json(v) => {
                let arr = v
                    .get("embeddings")
                    .and_then(|e| e.as_array())
                    .expect("embeddings array");
                assert_eq!(arr.len(), 3);
            }
            other => panic!("expected Json batch payload, got {other:?}"),
        }
    }

    /// RAG E2.1 — single-input embeddings stashuje ORYGINALNY tekst zapytania do
    /// meta["rag_query_text"] PRZED zamianą payloadu na Embedding, żeby reranker
    /// (downstream) miał query mimo że payload to już wektor.
    #[tokio::test]
    async fn stashes_query_text_into_meta_for_reranker() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("jakie jest pytanie?".into());
        let mut ctx = stub_ctx();
        ctx.embeddings = Arc::new(FakeEmbeddings {
            last_input: Mutex::new(None),
            vector: vec![0.1, 0.2],
        });
        let out = EmbeddingsNodeAdapter::new()
            .execute(&node(json!({"model": "m"})), &[input(env)], &ctx)
            .await
            .unwrap();
        assert_eq!(
            out.meta.get("rag_query_text").and_then(|v| v.as_str()),
            Some("jakie jest pytanie?")
        );
        // Payload mimo to jest wektorem (tekst już niedostępny w payloadzie).
        assert!(matches!(out.payload, FlowValue::Embedding(_)));
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
