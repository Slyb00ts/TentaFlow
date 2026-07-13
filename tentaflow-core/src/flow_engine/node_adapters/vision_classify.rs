// =============================================================================
// Plik: flow_engine/node_adapters/vision_classify.rs
// Opis: vision_classify node — multi-label condition tags per detection.
//       Iterates the frame's detections (meta["detections"]), crops each
//       placard/label/sign box and classifies that crop via the VisionDispatcher,
//       writing the tags into the detection's `stan`. Passes the frame Image
//       through unchanged (+ enriched detections). Model via node.config["alias"]
//       (default tentavision-action).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::dispatchers::VisionClassifyRequest;
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::node_adapters::vision_crop::crop_detection;
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::services::detection_bus::Detection;

const NODE_TYPE: &str = "vision_classify";
const DEFAULT_ALIAS: &str = "tentavision-action";

/// Detection classes whose crop carries a classifiable condition/state
/// (hazard placards, environmental signs, thermometers). Mirrors the hardcoded
/// enrich path's `wants_state`.
fn wants_state(klasa: &str) -> bool {
    klasa.starts_with("nalepka") || klasa == "znak_srodowiskowy" || klasa == "termometr"
}

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
        // Passes the frame through so a downstream vision node sees the same
        // Image; only meta["detections"] is enriched.
        vec![PortSpec::new("out", FlowDataType::Image)]
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
            _ => {
                return Err(anyhow!(
                    "vision_classify: expected Image payload (raw RGB24)"
                ))
            }
        };
        let (w, h) = dims.ok_or_else(|| anyhow!("vision_classify: image has no dims"))?;
        let rgb = ctx.blobs.get(&blob_ref).await?;
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

        let mut out = (*envelope).clone();
        let mut detections: Vec<Detection> = match out.meta.get("detections") {
            Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
                anyhow!("vision_classify: meta[detections] not a Detection list: {e}")
            })?,
            None => return Ok(out), // No detections to enrich — pass through.
        };

        for det in detections.iter_mut() {
            if !wants_state(&det.klasa) {
                continue;
            }
            let Some(crop) = crop_detection(&rgb, w, h, det.bbox) else {
                continue;
            };
            let tags = ctx
                .vision
                .classify(VisionClassifyRequest {
                    rgb: crop.rgb,
                    width: crop.width,
                    height: crop.height,
                    alias: alias.clone(),
                    caller_addon_id: None,
                })
                .await?;
            det.stan = tags;
        }

        out.meta
            .insert("detections".into(), serde_json::to_value(&detections)?);
        Ok(out)
    }
}
