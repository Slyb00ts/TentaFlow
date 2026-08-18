// =============================================================================
// Plik: flow_engine/node_adapters/vision_parse_pages.rs
// Opis: VisionParsePagesNodeAdapter — batch-owy wariant `vision_parse` dla całej
//       gałęzi PDF bez fan-out. Wejście: Json{pages:[blob_refs]} z
//       `pdf_rasterize`; wyjście: Json{pages:[{index,markdown}]} wprost
//       konsumowalne przez `document_merge`. Każdą stronę parsuje TĄ SAMĄ
//       ścieżką vision-chat co `vision_parse` (`parse_image_to_markdown`) —
//       zero duplikacji modelu/HTTP. Cardinality 1:1: lista stron jako JEDEN
//       envelope.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::node_adapters::page_branch::parse_page_blobs;
use crate::flow_engine::node_adapters::vision_parse::{
    parse_image_to_markdown, VisionParseNodeAdapter,
};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "vision_parse_pages";

pub struct VisionParsePagesNodeAdapter;

impl VisionParsePagesNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for VisionParsePagesNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for VisionParsePagesNodeAdapter {
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
            .ok_or_else(|| anyhow!("vision_parse_pages: brak krawędzi wejściowej"))?;
        let envelope = &input.envelope;

        let pages = parse_page_blobs(envelope)?;
        // Konfiguracja taka jak single `vision_parse` (model/max_tokens/tryb).
        let model = VisionParseNodeAdapter::pick_model(node, envelope);
        let max_tokens = VisionParseNodeAdapter::pick_max_tokens(node, envelope);
        let instruction = VisionParseNodeAdapter::instruction(node);

        // Iterujemy strony sekwencyjnie — jeden envelope, brak fan-out. Każda
        // strona to osobny vision-chat (wspólny `parse_image_to_markdown`).
        let mut out_pages: Vec<serde_json::Value> = Vec::with_capacity(pages.len());
        for page in pages {
            let (markdown, usage) = parse_image_to_markdown(
                ctx,
                node,
                envelope,
                page.blob_ref,
                model.clone(),
                max_tokens,
                instruction,
            )
            .await?;
            ctx.usage_sink.record(&node.id, usage);
            out_pages.push(serde_json::json!({
                "index": page.index,
                "markdown": markdown,
            }));
        }

        let count = out_pages.len();
        let mut out = (**envelope).clone();
        out.payload = FlowValue::Json(serde_json::json!({ "pages": out_pages }));
        out.meta
            .insert("parsed_pages".to_string(), serde_json::json!(count));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::dispatchers::{LlmDispatcher, LlmResponse};
    use crate::flow_engine::envelope::{LlmStreamChunk, TokenUsage};
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use serde_json::json;
    use std::sync::Arc;

    fn node(config: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "vpp1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    /// LLM stub zwracający stały markdown per call — dowód że batch woła
    /// vision-chat raz na stronę i składa listę pages.
    struct FakeVlm;
    #[async_trait]
    impl LlmDispatcher for FakeVlm {
        async fn execute_chat(
            &self,
            _req: crate::flow_engine::dispatchers::LlmRequest,
        ) -> Result<LlmResponse> {
            Ok(LlmResponse {
                audio: None,
                content: "# strona".into(),
                reasoning_content: None,
                tool_calls: Vec::new(),
                finish_reason: crate::flow_engine::envelope::FinishReason::Stop,
                usage: TokenUsage::default(),
            })
        }
        async fn stream_chat(
            &self,
            _req: crate::flow_engine::dispatchers::LlmRequest,
        ) -> Result<BoxStream<'static, Result<LlmStreamChunk>>> {
            panic!("stream not used");
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

    #[tokio::test]
    async fn parses_each_page_into_markdown_list() {
        let mut ctx = stub_ctx();
        ctx.llm = Arc::new(FakeVlm);
        let input = pages_input(&ctx, 3).await;
        let out = VisionParsePagesNodeAdapter::new()
            .execute(&node(json!({"model": "m"})), &[input], &ctx)
            .await
            .unwrap();
        let pages = match &out.payload {
            FlowValue::Json(v) => v.get("pages").and_then(|p| p.as_array()).cloned().unwrap(),
            other => panic!("expected Json, got {other:?}"),
        };
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0]["index"].as_u64(), Some(0));
        assert_eq!(pages[2]["index"].as_u64(), Some(2));
        assert_eq!(pages[1]["markdown"].as_str(), Some("# strona"));
        assert_eq!(
            out.meta.get("parsed_pages").and_then(|v| v.as_u64()),
            Some(3)
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
        let err = VisionParsePagesNodeAdapter::new()
            .execute(&node(json!({})), &[input], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("musi być Json"), "{err}");
    }
}
