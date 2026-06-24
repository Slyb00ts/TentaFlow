// =============================================================================
// Plik: flow_engine/node_adapters/ocr_pages.rs
// Opis: OcrPagesNodeAdapter — batch-owy wariant `ocr` dla gałęzi PDF bez
//       fan-out. Wejście: Json{pages:[blob_refs]} z `pdf_rasterize`; wyjście:
//       Json{pages:[{index,markdown}]} (tekst strony w reading-order jako
//       markdown), wprost konsumowalne przez `document_merge`. Każdą stronę OCR
//       przez TĄ SAMĄ ścieżkę co `ocr` (ctx.documents.infer task=ocr +
//       `OcrNodeAdapter::spans_to_text`). Lista stron to JEDEN envelope
//       (cardinality 1:1).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::node_adapters::ocr::OcrNodeAdapter;
use crate::flow_engine::node_adapters::page_branch::parse_page_blobs;
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "ocr_pages";
const TASK: &str = "ocr";

pub struct OcrPagesNodeAdapter;

impl OcrPagesNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for OcrPagesNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for OcrPagesNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Json)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("pages", FlowDataType::Json)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("ocr_pages: brak krawędzi wejściowej"))?;
        let envelope = &input.envelope;

        let pages = parse_page_blobs(envelope)?;
        let model = OcrNodeAdapter::pick_model(node, envelope);

        let mut out_pages: Vec<serde_json::Value> = Vec::with_capacity(pages.len());
        for page in pages {
            let image = ctx
                .blobs
                .get(&page.blob_ref)
                .await
                .map_err(|e| anyhow!("ocr_pages: pobranie strony {}: {e}", page.index))?;
            if image.is_empty() {
                return Err(anyhow!("ocr_pages: pusty obraz strony {}", page.index));
            }
            let result = ctx
                .documents
                .infer(&model, &image, &page.blob_ref.mime, TASK)
                .await
                .map_err(|e| anyhow!("ocr_pages: OCR zawiódł (strona {}): {e}", page.index))?;
            let markdown = OcrNodeAdapter::spans_to_text(&result.regions);
            out_pages.push(serde_json::json!({
                "index": page.index,
                "markdown": markdown,
            }));
        }

        let count = out_pages.len();
        let mut out = (**envelope).clone();
        out.payload = FlowValue::Json(serde_json::json!({ "pages": out_pages }));
        out.meta
            .insert("ocr_pages".to_string(), serde_json::json!(count));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use serde_json::json;
    use std::sync::Arc;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "ocrp1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    async fn pages_input(ctx: &ExecutionContext, n: usize) -> NodeInput {
        let mut entries = Vec::new();
        for i in 0..n {
            let blob = ctx.blobs.put(vec![1u8; 16], "image/png").await.unwrap();
            entries.push(json!({
                "index": i,
                "blob_id": blob.id,
                "sha256": blob.sha256,
                "size_bytes": blob.size_bytes,
                "mime": "image/png",
            }));
        }
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Json(json!({ "pages": entries }));
        NodeInput {
            from_node_id: "raster".into(),
            from_port: "images".into(),
            envelope: Arc::new(env),
        }
    }

    /// StubDocuments zwraca puste regiony → pusty markdown per strona, ale
    /// kształt pages:[{index,markdown}] musi być poprawny (mergeable).
    #[tokio::test]
    async fn emits_markdown_per_page() {
        let ctx = stub_ctx();
        let input = pages_input(&ctx, 2).await;
        let out = OcrPagesNodeAdapter::new()
            .execute(&node(json!({})), &[input], &ctx)
            .await
            .unwrap();
        let pages = match &out.payload {
            FlowValue::Json(v) => v.get("pages").and_then(|p| p.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0]["index"].as_u64(), Some(0));
        assert!(pages[0]["markdown"].is_string());
        assert_eq!(out.meta.get("ocr_pages").and_then(|v| v.as_u64()), Some(2));
    }

    #[tokio::test]
    async fn rejects_non_json_payload() {
        let ctx = stub_ctx();
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Text("nope".into());
        let input = NodeInput {
            from_node_id: "x".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        };
        let err = OcrPagesNodeAdapter::new()
            .execute(&node(json!({})), &[input], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("musi być Json"), "{err}");
    }
}
