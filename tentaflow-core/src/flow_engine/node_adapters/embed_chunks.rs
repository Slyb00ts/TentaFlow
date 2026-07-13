// =============================================================================
// Plik: flow_engine/node_adapters/embed_chunks.rs
// Opis: EmbedChunksNodeAdapter — mostek chunk→store. Bierze listę chunków
//       {index,text} z `chunk`, woła embeddings dispatcher (batch, jeden call
//       na cały dokument) i dokłada `embedding:[f32]` do KAŻDEGO chunka,
//       zachowując index+text. Wyjście pasuje wprost do `store.parse_chunks`.
//       Input: in(Json {chunks:[{index,text}]}) → output: full(Json
//       {chunks:[{index,text,embedding:[f32]}]}). Bez własnego HTTP — reużywa
//       `ctx.embeddings` jak `embeddings.rs`.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::dispatchers::EmbeddingsRequest;
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "embed_chunks";
/// Domyślny alias modelu embeddingów ingestu RAG. Alias rozwiązuje failover
/// dispatchera do realnego serwisu, więc — jak w innych węzłach ingestu — brak
/// konfiguracji NIE jest błędem (węzeł ma ustaloną rolę w flow).
const DEFAULT_MODEL: &str = "rag-embeddings";

/// Jeden sparsowany chunk wejściowy: index (stabilny dla `store.ref_id_for`) +
/// tekst do wektoryzacji. Trzymamy razem, bo po embeddingach składamy z powrotem
/// obiekt {index,text,embedding} w tej samej kolejności.
struct InputChunk {
    index: u64,
    text: String,
}

pub struct EmbedChunksNodeAdapter;

impl EmbedChunksNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Model-picking wzorem `embeddings`/`vision_parse`: node.config['model'] >
    /// envelope.meta['embeddings_model'] > domyślny alias `rag-embeddings`.
    fn pick_model(node: &FlowNode, envelope: &FlowEnvelope) -> String {
        if let Some(m) = node
            .config
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return m.to_string();
        }
        if let Some(m) = envelope
            .meta
            .get("embeddings_model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return m.to_string();
        }
        DEFAULT_MODEL.to_string()
    }

    /// Parsuje wejściowe `{chunks:[{index,text}]}`. WALIDACJA-PRZED-WYWOŁANIEM:
    /// wszystkie chunki sprawdzamy przed jednym batch-callem embeddingów — pusty
    /// tekst chunka to błąd (wektor pustego tekstu nie ma sensu w retrievalu).
    fn parse_chunks(envelope: &FlowEnvelope) -> Result<Vec<InputChunk>> {
        let obj = match &envelope.payload {
            FlowValue::Json(v) => v,
            other => {
                return Err(anyhow!(
                    "embed_chunks: payload musi być Json{{chunks:[...]}}, dostał {}",
                    other.kind()
                ))
            }
        };
        let items = obj
            .get("chunks")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("embed_chunks: payload Json bez 'chunks' (tablica)"))?;
        if items.is_empty() {
            return Err(anyhow!("embed_chunks: pusta lista 'chunks'"));
        }
        let mut out = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let index = item
                .get("index")
                .and_then(|v| v.as_u64())
                .unwrap_or(i as u64);
            let text = item
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("embed_chunks: chunk[{i}] brak 'text'"))?;
            if text.trim().is_empty() {
                return Err(anyhow!("embed_chunks: chunk[{i}] ma pusty 'text'"));
            }
            out.push(InputChunk {
                index,
                text: text.to_string(),
            });
        }
        Ok(out)
    }
}

impl Default for EmbedChunksNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for EmbedChunksNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Json)]
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Json)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("embed_chunks: brak krawędzi wejściowej"))?;
        let envelope = &input.envelope;

        let chunks = Self::parse_chunks(envelope)?;
        let model = Self::pick_model(node, envelope);

        let dimensions = node
            .config
            .get("dimensions")
            .and_then(|v| v.as_u64())
            .or_else(|| envelope.meta.get("dimensions").and_then(|v| v.as_u64()))
            .map(|n| n as u32);

        // Jeden batch-call na cały dokument — dispatcher przyjmuje listę tekstów
        // i zwraca wektor per tekst (cardinality 1:1), więc nie potrzeba pętli
        // per-chunk z osobnym requestem.
        let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
        let req = EmbeddingsRequest {
            model,
            inputs: texts,
            dimensions,
            encoding_format: None,
            user_id: ctx.user_id.clone(),
            user_role: ctx.user_role.clone(),
            flow_depth: ctx.subflow_depth,
        };

        let response = ctx
            .embeddings
            .embed(req)
            .await
            .map_err(|e| anyhow!("embed_chunks: dispatcher failed: {e}"))?;

        if response.vectors.len() != chunks.len() {
            return Err(anyhow!(
                "embed_chunks: backend zwrócił {} wektorów dla {} chunków",
                response.vectors.len(),
                chunks.len()
            ));
        }
        if response.vectors.iter().any(|v| v.is_empty()) {
            return Err(anyhow!("embed_chunks: backend zwrócił pusty wektor"));
        }

        ctx.usage_sink.record(&node.id, response.usage);

        // Składamy z powrotem {index,text,embedding} w kolejności wejścia — store
        // czyta `embedding` jako tablicę liczb (store.parse_chunks).
        let out_chunks: Vec<serde_json::Value> = chunks
            .into_iter()
            .zip(response.vectors)
            .map(|(chunk, vector)| {
                let embedding: Vec<serde_json::Value> = vector
                    .into_iter()
                    .filter_map(|f| serde_json::Number::from_f64(f as f64))
                    .map(serde_json::Value::Number)
                    .collect();
                serde_json::json!({
                    "index": chunk.index,
                    "text": chunk.text,
                    "embedding": embedding,
                })
            })
            .collect();
        let count = out_chunks.len();

        let mut out = (**envelope).clone();
        out.payload = FlowValue::Json(serde_json::json!({ "chunks": out_chunks }));
        out.meta
            .insert("embedded_chunks".to_string(), serde_json::json!(count));
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
            id: "ec1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    fn input(payload: FlowValue) -> NodeInput {
        let mut env = FlowEnvelope::empty();
        env.payload = payload;
        NodeInput {
            from_node_id: "chunk".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    /// Zwraca jeden deterministyczny wektor per input (rozmiar 2), zapamiętuje
    /// listę tekstów żeby sprawdzić że doszły wszystkie chunki w kolejności.
    struct FakeEmbeddings {
        seen: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl EmbeddingsDispatcher for FakeEmbeddings {
        async fn embed(&self, req: EmbeddingsRequest) -> Result<EmbeddingsResponse> {
            *self.seen.lock().unwrap() = req.inputs.clone();
            let vectors: Vec<Vec<f32>> = req
                .inputs
                .iter()
                .enumerate()
                .map(|(i, _)| vec![i as f32, i as f32 + 0.5])
                .collect();
            Ok(EmbeddingsResponse {
                vectors,
                usage: TokenUsage::default(),
            })
        }
    }

    #[tokio::test]
    async fn injects_embedding_into_each_chunk_preserving_index_text() {
        let mut ctx = stub_ctx();
        let fake = Arc::new(FakeEmbeddings {
            seen: Mutex::new(Vec::new()),
        });
        ctx.embeddings = fake.clone();

        let payload = FlowValue::Json(json!({"chunks": [
            {"index": 0, "text": "pierwszy"},
            {"index": 1, "text": "drugi"},
        ]}));
        let out = EmbedChunksNodeAdapter::new()
            .execute(&node(json!({"model": "m"})), &[input(payload)], &ctx)
            .await
            .unwrap();

        assert_eq!(
            *fake.seen.lock().unwrap(),
            vec!["pierwszy".to_string(), "drugi".to_string()]
        );

        let chunks = match &out.payload {
            FlowValue::Json(v) => v.get("chunks").and_then(|c| c.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0]["index"].as_u64(), Some(0));
        assert_eq!(chunks[0]["text"].as_str(), Some("pierwszy"));
        let emb0 = chunks[0]["embedding"].as_array().unwrap();
        assert_eq!(emb0.len(), 2);
        assert_eq!(emb0[0].as_f64(), Some(0.0));
        // Drugi chunk dostaje swój wektor (i!=0) — dowód że nie nadpisujemy.
        assert_eq!(chunks[1]["embedding"][0].as_f64(), Some(1.0));
        assert_eq!(
            out.meta.get("embedded_chunks").and_then(|v| v.as_u64()),
            Some(2)
        );
    }

    #[tokio::test]
    async fn rejects_non_json_payload() {
        let ctx = stub_ctx();
        let err = EmbedChunksNodeAdapter::new()
            .execute(
                &node(json!({})),
                &[input(FlowValue::Text("nope".into()))],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("musi być Json"), "{err}");
    }

    #[tokio::test]
    async fn rejects_chunk_without_text() {
        let ctx = stub_ctx();
        let payload = FlowValue::Json(json!({"chunks": [{"index": 0}]}));
        let err = EmbedChunksNodeAdapter::new()
            .execute(&node(json!({})), &[input(payload)], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("brak 'text'"), "{err}");
    }
}
