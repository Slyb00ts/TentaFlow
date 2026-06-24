// =============================================================================
// Plik: flow_engine/node_adapters/chunk.rs
// Opis: ChunkNodeAdapter — dzieli tekst (markdown) na chunki po zdaniach/
//       akapitach z overlap. Input Text → output Json {chunks:[{text,index}]}.
//       Config `size`/`overlap` (domyślne ze stałych extract.rs). Bez modelu —
//       reużywa `split_into_chunks`.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::services::document::extract::{
    split_into_chunks, CHUNK_OVERLAP_CHARS, CHUNK_SIZE_CHARS,
};

const NODE_TYPE: &str = "chunk";

pub struct ChunkNodeAdapter;

impl ChunkNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Rozmiar chunka z node.config (domyślnie CHUNK_SIZE_CHARS). 0 → błąd
    /// (chunk zerowej długości nie ma sensu).
    fn pick_size(node: &FlowNode) -> Result<usize> {
        match node.config.get("size").and_then(|v| v.as_u64()) {
            None => Ok(CHUNK_SIZE_CHARS),
            Some(0) => Err(anyhow!("chunk: 'size' musi być > 0")),
            Some(n) => Ok(n as usize),
        }
    }

    /// Overlap z node.config (domyślnie CHUNK_OVERLAP_CHARS). Overlap >= size jest
    /// błędem (chunk składałby się głównie z ogona poprzedniego).
    fn pick_overlap(node: &FlowNode, size: usize) -> Result<usize> {
        let overlap = node
            .config
            .get("overlap")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(CHUNK_OVERLAP_CHARS);
        if overlap >= size {
            return Err(anyhow!(
                "chunk: 'overlap' ({overlap}) musi być mniejszy niż 'size' ({size})"
            ));
        }
        Ok(overlap)
    }
}

impl Default for ChunkNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for ChunkNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Text)]
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Json)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("chunk: brak krawędzi wejściowej"))?;
        let envelope = &input.envelope;

        let text = match &envelope.payload {
            FlowValue::Text(t) if !t.trim().is_empty() => t.clone(),
            FlowValue::Text(_) | FlowValue::Empty => {
                return Err(anyhow!("chunk: pusty tekst wejściowy"))
            }
            other => {
                return Err(anyhow!(
                    "chunk: payload musi być Text, dostał {}",
                    other.kind()
                ))
            }
        };

        let size = Self::pick_size(node)?;
        let overlap = Self::pick_overlap(node, size)?;

        let chunks = split_into_chunks(&text, size, overlap);
        if chunks.is_empty() {
            return Err(anyhow!("chunk: chunking nie wyprodukował żadnego chunka"));
        }

        let arr: Vec<serde_json::Value> = chunks
            .into_iter()
            .enumerate()
            .map(|(index, text)| serde_json::json!({ "index": index, "text": text }))
            .collect();
        let count = arr.len();

        let mut out = (**envelope).clone();
        out.payload = FlowValue::Json(serde_json::json!({ "chunks": arr }));
        out.meta
            .insert("chunk_count".to_string(), serde_json::json!(count));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use std::sync::Arc;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "chunk-1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    fn text_input(text: &str) -> NodeInput {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text(text.into());
        NodeInput {
            from_node_id: "extract".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    #[tokio::test]
    async fn chunks_text_into_indexed_json() {
        let ctx = stub_ctx();
        let text = "Pierwsze zdanie. Drugie zdanie.\n\nTrzeci akapit.";
        let out = ChunkNodeAdapter::new()
            .execute(&node(serde_json::json!({})), &[text_input(text)], &ctx)
            .await
            .unwrap();
        let chunks = match &out.payload {
            FlowValue::Json(v) => v.get("chunks").and_then(|c| c.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0]["index"].as_u64(), Some(0));
        assert!(chunks[0]["text"].as_str().unwrap().contains("Pierwsze zdanie"));
        assert_eq!(
            out.meta.get("chunk_count").and_then(|v| v.as_u64()),
            Some(chunks.len() as u64)
        );
    }

    #[tokio::test]
    async fn many_chunks_for_long_text_with_small_size() {
        let ctx = stub_ctx();
        let text = "zdanie. ".repeat(500);
        let out = ChunkNodeAdapter::new()
            .execute(
                &node(serde_json::json!({"size": 200, "overlap": 20})),
                &[text_input(&text)],
                &ctx,
            )
            .await
            .unwrap();
        let chunks = match &out.payload {
            FlowValue::Json(v) => v.get("chunks").and_then(|c| c.as_array()).cloned().unwrap(),
            _ => panic!("expected Json"),
        };
        assert!(chunks.len() > 1, "długi tekst → wiele chunków");
        for c in &chunks {
            assert!(c["text"].as_str().unwrap().chars().count() <= 200);
        }
    }

    #[tokio::test]
    async fn overlap_ge_size_is_error() {
        let ctx = stub_ctx();
        let err = ChunkNodeAdapter::new()
            .execute(
                &node(serde_json::json!({"size": 100, "overlap": 100})),
                &[text_input("dowolny tekst")],
                &ctx,
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("overlap"), "{err}");
    }

    #[tokio::test]
    async fn rejects_non_text_payload() {
        let ctx = stub_ctx();
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Json(serde_json::json!({"x": 1}));
        let input = NodeInput {
            from_node_id: "x".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        };
        let err = ChunkNodeAdapter::new()
            .execute(&node(serde_json::json!({})), &[input], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("musi być Text"), "{err}");
    }
}
