// =============================================================================
// Plik: flow_engine/node_adapters/vision_ocr.rs
// Opis: vision_ocr node — reads a plate/code string from an RGB image crop via
//       the VisionDispatcher. Input port = Image (raw RGB24 blob + dims, as the
//       camera-CV cold path produces); output = Text (the plate, "" if none),
//       also mirrored to meta["plate"] for downstream condition/verdict nodes.
//       Model chosen by node.config["alias"] (default tentavision-ocr).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::dispatchers::VisionOcrRequest;
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "vision_ocr";
const DEFAULT_ALIAS: &str = "tentavision-ocr";

pub struct VisionOcrNodeAdapter;

impl VisionOcrNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl NodeAdapter for VisionOcrNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Image)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("out", FlowDataType::Text)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let envelope = inputs
            .first()
            .map(|i| i.envelope.clone())
            .unwrap_or_else(|| ctx.initial_envelope.clone());

        let (blob_ref, dims) = match &envelope.payload {
            FlowValue::Image { blob_ref, dims, .. } => (blob_ref.clone(), *dims),
            _ => return Err(anyhow!("vision_ocr: expected Image payload (raw RGB24)")),
        };
        let (w, h) = dims.ok_or_else(|| anyhow!("vision_ocr: image has no dims"))?;
        let rgb = ctx.blobs.get(&blob_ref).await?;
        // Contract: raw RGB24. Reject encoded (JPEG/PNG) or mismatched blobs with
        // a clear error here instead of a deep failure inside the runner.
        let expected = w as usize * h as usize * 3;
        if rgb.len() != expected {
            return Err(anyhow!(
                "vision_ocr: blob is {} bytes, expected {}x{}x3={} (raw RGB24 only)",
                rgb.len(),
                w,
                h,
                expected
            ));
        }
        let alias = node
            .config
            .get("alias")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_ALIAS)
            .to_string();

        let text = ctx
            .vision
            .ocr(VisionOcrRequest {
                rgb,
                width: w,
                height: h,
                alias,
                caller_addon_id: None,
            })
            .await?;

        let plate = text.unwrap_or_default();
        let mut out = (*envelope).clone();
        out.meta
            .insert("plate".into(), serde_json::Value::String(plate.clone()));
        out.payload = FlowValue::Text(plate);
        Ok(out)
    }
}
