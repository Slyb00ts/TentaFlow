// =============================================================================
// Plik: flow_engine/node_adapters/page_detect_pages.rs
// Opis: PageDetectPagesNodeAdapter — batch-owy wariant `page_detect` dla gałęzi
//       PDF bez fan-out. Wejście: Json{pages:[blob_refs]} z `pdf_rasterize`;
//       wyjście: Json{pages:[{index,blocks:[{class,bbox,confidence}]}]} zgodne z
//       `document_merge` (parse_block). Każdą stronę detektuje TĄ SAMĄ ścieżką
//       co `page_detect` (ctx.documents.infer, task=page_elements). Lista stron
//       to JEDEN envelope (cardinality 1:1).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::node_adapters::page_branch::parse_page_blobs;
use crate::flow_engine::node_adapters::page_detect::PageDetectNodeAdapter;
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "page_detect_pages";
const DEFAULT_MODEL: &str = "rag-page-elements";
const TASK: &str = "page_elements";

pub struct PageDetectPagesNodeAdapter;

impl PageDetectPagesNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Mapuje `DocRegion` na blok layoutu czytany przez `document_merge`:
    /// {class, bbox, confidence}. Regiony layoutu nie niosą tekstu (to robi
    /// OCR/vision-parse), więc blok ma sam region — merge renumeruje go per
    /// strona i pomija pusty markdown.
    fn region_to_block(region: &tentaflow_protocol::DocRegion) -> serde_json::Value {
        serde_json::json!({
            "class": region.class,
            "bbox": region.bbox,
            "confidence": region.score,
        })
    }
}

impl Default for PageDetectPagesNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for PageDetectPagesNodeAdapter {
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
            .ok_or_else(|| anyhow!("page_detect_pages: brak krawędzi wejściowej"))?;
        let envelope = &input.envelope;

        let pages = parse_page_blobs(envelope)?;
        let model = PageDetectNodeAdapter::pick_model(node, envelope, DEFAULT_MODEL);

        let mut out_pages: Vec<serde_json::Value> = Vec::with_capacity(pages.len());
        for page in pages {
            let image =
                ctx.blobs.get(&page.blob_ref).await.map_err(|e| {
                    anyhow!("page_detect_pages: pobranie strony {}: {e}", page.index)
                })?;
            if image.is_empty() {
                return Err(anyhow!(
                    "page_detect_pages: pusty obraz strony {}",
                    page.index
                ));
            }
            let result = ctx
                .documents
                .infer(&model, &image, &page.blob_ref.mime, TASK, ctx.provenance())
                .await
                .map_err(|e| {
                    anyhow!(
                        "page_detect_pages: detektor zawiódł (strona {}): {e}",
                        page.index
                    )
                })?;
            let blocks: Vec<serde_json::Value> =
                result.regions.iter().map(Self::region_to_block).collect();
            out_pages.push(serde_json::json!({
                "index": page.index,
                "blocks": blocks,
            }));
        }

        let count = out_pages.len();
        let mut out = (**envelope).clone();
        out.payload = FlowValue::Json(serde_json::json!({ "pages": out_pages }));
        out.meta
            .insert("detected_pages".to_string(), serde_json::json!(count));
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
            id: "pdp1".into(),
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

    /// StubDocuments zwraca puste regiony — node musi mimo to wyemitować
    /// pages:[{index,blocks:[]}] dla każdej strony (kontrakt nawet przy 0
    /// detekcji), zgodny kształtem z document_merge.
    #[tokio::test]
    async fn emits_page_blocks_for_each_page() {
        let ctx = stub_ctx();
        let input = pages_input(&ctx, 2).await;
        let out = PageDetectPagesNodeAdapter::new()
            .execute(&node(json!({})), &[input], &ctx)
            .await
            .unwrap();
        let pages = match &out.payload {
            FlowValue::Json(v) => v.get("pages").and_then(|p| p.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0]["index"].as_u64(), Some(0));
        assert!(pages[0]["blocks"].as_array().is_some());
        assert_eq!(
            out.meta.get("detected_pages").and_then(|v| v.as_u64()),
            Some(2)
        );
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
        let err = PageDetectPagesNodeAdapter::new()
            .execute(&node(json!({})), &[input], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("musi być Json"), "{err}");
    }
}
