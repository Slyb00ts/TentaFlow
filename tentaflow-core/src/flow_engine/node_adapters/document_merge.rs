// =============================================================================
// Plik: flow_engine/node_adapters/document_merge.rs
// Opis: DocumentMergeNodeAdapter — scala per-stronowe wyniki parsowania
//       (markdown vision-parse + bloki regionów/OCR/tabel) w jeden markdown z
//       narastającą numeracją stron (reading-order). Reużywa
//       services::document::merge_page_responses. Input Json {pages:[...]} →
//       output Text (markdown). Bez modelu.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::services::document::merge_page_responses;
use crate::services::runtime::executor::{DocBlock, DocumentParseResponse};

const NODE_TYPE: &str = "document_merge";

pub struct DocumentMergeNodeAdapter;

impl DocumentMergeNodeAdapter {
    pub fn new() -> Self {
        Self
    }

    /// Parsuje jeden blok layoutu z Json `{class, bbox:[x1,y1,x2,y2], text,
    /// confidence?}`. bbox opcjonalny (gdy brak → zera). Bloki niosą regiony z
    /// detektorów (page_detect) oraz tekst z OCR/table — wszystkie wpadają do
    /// `DocBlock`, który `merge_page_responses` renumeruje per-strona.
    fn parse_block(v: &serde_json::Value) -> DocBlock {
        let class = v
            .get("class")
            .and_then(|c| c.as_str())
            .unwrap_or("Text")
            .to_string();
        let text = v
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        let bbox = v
            .get("bbox")
            .and_then(|b| b.as_array())
            .map(|arr| {
                let mut out = [0.0f32; 4];
                for (i, slot) in out.iter_mut().enumerate() {
                    *slot = arr.get(i).and_then(|n| n.as_f64()).unwrap_or(0.0) as f32;
                }
                out
            })
            .unwrap_or([0.0; 4]);
        let confidence = v
            .get("confidence")
            .and_then(|c| c.as_f64())
            .map(|f| f as f32);
        DocBlock {
            page: 0,
            class,
            bbox,
            text,
            confidence,
        }
    }

    /// Parsuje jedną stronę z Json `{markdown, blocks:[...]}`. Co najmniej jedno
    /// pole musi nieść treść — pusta strona (bez markdown i bez bloków) i tak
    /// scala się czysto (merge pomija puste markdown), ale wymagamy obecności
    /// `markdown` LUB `blocks`, by złapać błędny kształt wejścia.
    fn parse_page(i: usize, v: &serde_json::Value) -> Result<DocumentParseResponse> {
        let markdown = v
            .get("markdown")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let blocks: Vec<DocBlock> = v
            .get("blocks")
            .and_then(|b| b.as_array())
            .map(|arr| arr.iter().map(Self::parse_block).collect())
            .unwrap_or_default();
        if markdown.is_empty() && blocks.is_empty() {
            return Err(anyhow!(
                "document_merge: strona[{i}] nie ma ani 'markdown' ani 'blocks'"
            ));
        }
        Ok(DocumentParseResponse {
            markdown,
            blocks,
            usage: None,
        })
    }
}

impl Default for DocumentMergeNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for DocumentMergeNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Json)]
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Text)]
    }

    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("document_merge: brak krawędzi wejściowej"))?;
        let envelope = &input.envelope;

        let obj = match &envelope.payload {
            FlowValue::Json(v) => v,
            other => {
                return Err(anyhow!(
                    "document_merge: payload musi być Json{{pages:[...]}}, dostał {}",
                    other.kind()
                ))
            }
        };
        let pages_json = obj
            .get("pages")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("document_merge: payload Json bez 'pages' (tablica)"))?;
        if pages_json.is_empty() {
            return Err(anyhow!("document_merge: pusta lista 'pages'"));
        }

        // Kolejność stron = kolejność w tablicy (reading-order). Caller (fan-in po
        // vision-parse / OCR per-strona) MUSI dostarczyć strony posortowane po
        // indeksie — `merge_page_responses` nadaje numery stron po pozycji w Vec.
        let mut pages: Vec<DocumentParseResponse> = Vec::with_capacity(pages_json.len());
        for (i, page) in pages_json.iter().enumerate() {
            pages.push(Self::parse_page(i, page)?);
        }

        let merged = merge_page_responses(pages);
        if merged.markdown.trim().is_empty() {
            return Err(anyhow!("document_merge: scalony markdown jest pusty"));
        }

        let mut out = (**envelope).clone();
        out.meta.insert(
            "merged_block_count".to_string(),
            serde_json::json!(merged.blocks.len()),
        );
        out.payload = FlowValue::Text(merged.markdown);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use serde_json::json;
    use std::sync::Arc;

    fn node() -> FlowNode {
        FlowNode {
            id: "merge-1".into(),
            node_type: NODE_TYPE.into(),
            config: json!({}),
            position: None,
            label: None,
            region: None,
        }
    }

    fn input(payload: FlowValue) -> NodeInput {
        let mut env = FlowEnvelope::empty();
        env.payload = payload;
        NodeInput {
            from_node_id: "parse".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    #[tokio::test]
    async fn merges_pages_into_single_markdown_in_order() {
        let ctx = stub_ctx();
        let payload = FlowValue::Json(json!({"pages": [
            {"markdown": "# Strona 0", "blocks": [{"class": "Title", "text": "# Strona 0"}]},
            {"markdown": "# Strona 1", "blocks": [{"class": "Text", "text": "tresc 1"}]},
        ]}));
        let out = DocumentMergeNodeAdapter::new()
            .execute(&node(), &[input(payload)], &ctx)
            .await
            .unwrap();
        match out.payload {
            FlowValue::Text(md) => assert_eq!(md, "# Strona 0\n\n# Strona 1"),
            other => panic!("expected Text, got {other:?}"),
        }
        assert_eq!(
            out.meta.get("merged_block_count").and_then(|v| v.as_u64()),
            Some(2)
        );
    }

    #[tokio::test]
    async fn page_with_only_blocks_is_accepted() {
        let ctx = stub_ctx();
        let payload = FlowValue::Json(json!({"pages": [
            {"markdown": "tylko markdown"},
            {"blocks": [{"class": "Table", "text": "| a | b |", "bbox": [0,0,10,10]}]},
        ]}));
        let out = DocumentMergeNodeAdapter::new()
            .execute(&node(), &[input(payload)], &ctx)
            .await
            .unwrap();
        match out.payload {
            FlowValue::Text(md) => assert!(md.contains("tylko markdown")),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_page_shape_is_error() {
        let ctx = stub_ctx();
        let payload = FlowValue::Json(json!({"pages": [{}]}));
        let err = DocumentMergeNodeAdapter::new()
            .execute(&node(), &[input(payload)], &ctx)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("ani 'markdown' ani 'blocks'"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn rejects_non_json_payload() {
        let ctx = stub_ctx();
        let err = DocumentMergeNodeAdapter::new()
            .execute(&node(), &[input(FlowValue::Text("nope".into()))], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("musi być Json"), "{err}");
    }
}
