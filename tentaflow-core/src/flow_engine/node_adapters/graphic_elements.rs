// =============================================================================
// Plik: flow_engine/node_adapters/graphic_elements.rs
// Opis: GraphicElementsNodeAdapter — detekcja elementów graficznych strony
//       (figure/chart/diagram/logo…) przez typed surface Documents (`/v1/infer`,
//       task=graphic_elements). Reużywa ctx.documents.infer (PARTIA 0) i wspólne
//       helpery z page_detect. Input: image(Image) → output: regions(Json).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::node_adapters::page_detect::{regions_payload, resolve_image};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "graphic_elements";
const DEFAULT_MODEL: &str = "rag-graphic-elements";
const TASK: &str = "graphic_elements";

pub struct GraphicElementsNodeAdapter;

impl GraphicElementsNodeAdapter {
    pub fn new() -> Self {
        Self
    }

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
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            return m.to_string();
        }
        DEFAULT_MODEL.to_string()
    }
}

impl Default for GraphicElementsNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for GraphicElementsNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Image)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("regions", FlowDataType::Json)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("{NODE_TYPE}: brak krawędzi wejściowej"))?;
        let envelope = &input.envelope;

        let (blob_ref, mime) = resolve_image(envelope)?;
        let image = ctx
            .blobs
            .get(&blob_ref)
            .await
            .map_err(|e| anyhow!("{NODE_TYPE}: pobranie obrazu: {e}"))?;
        if image.is_empty() {
            return Err(anyhow!("{NODE_TYPE}: pusty obraz strony"));
        }
        let model = Self::pick_model(node, envelope);

        let result = ctx
            .documents
            .infer(&model, &image, &mime, TASK, ctx.provenance())
            .await
            .map_err(|e| anyhow!("{NODE_TYPE}: detektor zawiódł: {e}"))?;

        let mut out: FlowEnvelope = (**envelope).clone();
        out.payload = FlowValue::Json(regions_payload(&result.regions)?);
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
            id: "ge1".into(),
            node_type: NODE_TYPE.into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    #[test]
    fn pick_model_defaults_to_graphic_elements_alias() {
        let env = FlowEnvelope::empty();
        assert_eq!(
            GraphicElementsNodeAdapter::pick_model(&node(json!({})), &env),
            "rag-graphic-elements"
        );
    }

    #[tokio::test]
    async fn emits_regions_json_payload() {
        let ctx = stub_ctx();
        let blob = ctx.blobs.put(vec![1u8; 16], "image/png").await.unwrap();
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Image {
            blob_ref: blob,
            mime: "image/png".into(),
            dims: None,
        };
        let input = NodeInput {
            from_node_id: "raster".into(),
            from_port: "images".into(),
            envelope: Arc::new(env),
        };
        let out = GraphicElementsNodeAdapter::new()
            .execute(&node(json!({})), &[input], &ctx)
            .await
            .unwrap();
        assert!(matches!(out.payload, FlowValue::Json(_)));
    }
}
