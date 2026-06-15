// =============================================================================
// Plik: flow_engine/node_adapters/vision_classify.rs
// Opis: vision_classify node — multi-label condition tags for a placard/label
//       RGB crop via the VisionDispatcher. Input = Image (raw RGB24 + dims);
//       output = Json (array of tag strings), mirrored to meta["stan"] for
//       downstream condition/verdict. Model via node.config["alias"]
//       (default tentavision-action).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::dispatchers::VisionClassifyRequest;
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const NODE_TYPE: &str = "vision_classify";
const DEFAULT_ALIAS: &str = "tentavision-action";

pub struct VisionClassifyNodeAdapter;

impl VisionClassifyNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl NodeAdapter for VisionClassifyNodeAdapter {
    fn node_type(&self) -> &str {
        NODE_TYPE
    }
    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Image)]
    }
    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("out", FlowDataType::Json)]
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
            _ => return Err(anyhow!("vision_classify: expected Image payload (raw RGB24)")),
        };
        let (w, h) = dims.ok_or_else(|| anyhow!("vision_classify: image has no dims"))?;
        let rgb = ctx.blobs.get(&blob_ref).await?;
        // Contract: raw RGB24. Reject encoded/mismatched blobs with a clear error.
        let expected = w as usize * h as usize * 3;
        if rgb.len() != expected {
            return Err(anyhow!(
                "vision_classify: blob is {} bytes, expected {}x{}x3={} (raw RGB24 only)",
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

        let tags = ctx
            .vision
            .classify(VisionClassifyRequest {
                rgb,
                width: w,
                height: h,
                alias,
                caller_addon_id: None,
            })
            .await?;

        let json = serde_json::Value::Array(
            tags.iter()
                .map(|t| serde_json::Value::String(t.clone()))
                .collect(),
        );
        let mut out = (*envelope).clone();
        out.meta.insert("stan".into(), json.clone());
        out.payload = FlowValue::Json(json);
        Ok(out)
    }
}
