// =============================================================================
// Plik: flow_engine/node_adapters/vision_ocr.rs
// Opis: vision_ocr node — reads plate text per detection. Iterates the frame's
//       detections (meta["detections"]), crops each license-plate box and runs
//       OCR on that crop via the VisionDispatcher, writing the result into the
//       detection's `tekst`. Passes the frame Image through unchanged (+ enriched
//       detections) so the next node sees the same frame. Model via
//       node.config["alias"] (default tentavision-ocr).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::dispatchers::VisionOcrRequest;
use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::node_adapters::vision_crop::crop_detection;
use crate::flow_engine::types::{FlowDataType, FlowNode};
use crate::services::detection_bus::Detection;

const NODE_TYPE: &str = "vision_ocr";
const DEFAULT_ALIAS: &str = "tentavision-ocr";
/// Detection class whose crop carries a readable license plate.
const PLATE_CLASS: &str = "tablica_rejestracyjna";

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
            _ => return Err(anyhow!("vision_ocr: expected Image payload (raw RGB24)")),
        };
        let (w, h) = dims.ok_or_else(|| anyhow!("vision_ocr: image has no dims"))?;
        let rgb = ctx.blobs.get(&blob_ref).await?;
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

        let mut out = (*envelope).clone();
        let mut detections: Vec<Detection> = match out.meta.get("detections") {
            Some(v) => serde_json::from_value(v.clone())
                .map_err(|e| anyhow!("vision_ocr: meta[detections] not a Detection list: {e}"))?,
            None => return Ok(out), // No detections to enrich — pass through.
        };

        // OCR each plate crop, writing the read text back into that detection.
        for det in detections.iter_mut() {
            if det.klasa != PLATE_CLASS {
                continue;
            }
            let Some(crop) = crop_detection(&rgb, w, h, det.bbox) else {
                continue;
            };
            let text = ctx
                .vision
                .ocr(VisionOcrRequest {
                    rgb: crop.rgb,
                    width: crop.width,
                    height: crop.height,
                    alias: alias.clone(),
                    caller_addon_id: None,
                })
                .await?;
            if let Some(plate) = text {
                det.tekst = Some(plate);
            }
        }

        out.meta
            .insert("detections".into(), serde_json::to_value(&detections)?);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::dispatchers::{VisionClassifyRequest, VisionDispatcher};
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use crate::flow_engine::node_adapters::{
        CameraAlertNodeAdapter, CameraVerdictNodeAdapter, VisionClassifyNodeAdapter,
    };
    use std::sync::{Arc, Mutex};

    /// Fake VisionDispatcher: records the (w,h) of every crop it receives and
    /// returns canned results, so a test can prove the nodes crop per detection
    /// (not the whole frame) and call the right model per class.
    #[derive(Default)]
    struct FakeVision {
        ocr_crops: Mutex<Vec<(u32, u32)>>,
        classify_crops: Mutex<Vec<(u32, u32)>>,
    }

    #[async_trait]
    impl VisionDispatcher for FakeVision {
        async fn ocr(&self, req: VisionOcrRequest) -> Result<Option<String>> {
            assert_eq!(req.rgb.len(), (req.width * req.height * 3) as usize, "tight crop");
            self.ocr_crops.lock().unwrap().push((req.width, req.height));
            Ok(Some("WX 12345".into()))
        }
        async fn classify(&self, req: VisionClassifyRequest) -> Result<Vec<String>> {
            assert_eq!(req.rgb.len(), (req.width * req.height * 3) as usize, "tight crop");
            self.classify_crops.lock().unwrap().push((req.width, req.height));
            Ok(vec!["pełna".into()])
        }
    }

    fn det(klasa: &str, bbox: [f32; 4]) -> Detection {
        Detection {
            klasa: klasa.into(),
            bbox,
            score: 0.9,
            stan: vec![],
            tekst: None,
            tekst_conf: None,
            tekst_thumb_ref: None,
            track_id: 0,
            vx: 0.,
            vy: 0.,
        }
    }

    fn node(node_type: &str) -> FlowNode {
        FlowNode {
            id: node_type.into(),
            node_type: node_type.into(),
            config: serde_json::json!({}),
            position: None,
            label: None,
            region: None,
        }
    }

    /// Cold-run shape: trigger seeds an Image frame + 3 detections; vision_ocr
    /// then vision_classify enrich per-crop. Asserts each model ran ONLY for its
    /// class, on a CROP (not the full frame), and that the final detections carry
    /// the enriched `tekst` / `stan` for the overlay.
    #[tokio::test]
    async fn ocr_then_classify_enrich_per_detection_crop() {
        let fake = Arc::new(FakeVision::default());
        let mut ctx = stub_ctx();
        ctx.vision = fake.clone();

        // 40x40 frame; plate in the top-left quarter (→20x20 crop), an ADR
        // sticker in the bottom-right quarter, and a person box ocr/classify
        // must both ignore.
        let frame = vec![128u8; 40 * 40 * 3];
        let blob = ctx.blobs.put(frame, "image/x-rgb24").await.unwrap();
        let dets = vec![
            det("tablica_rejestracyjna", [0.0, 0.0, 0.5, 0.5]),
            det("nalepka_adr", [0.5, 0.5, 0.5, 0.5]),
            det("osoba", [0.0, 0.0, 0.5, 0.5]),
        ];
        let mut env = FlowEnvelope::with_payload(FlowValue::Image {
            blob_ref: blob,
            mime: "image/x-rgb24".into(),
            dims: Some((40, 40)),
        });
        env.meta
            .insert("detections".into(), serde_json::to_value(&dets).unwrap());

        let ocr_input = NodeInput {
            from_node_id: "trigger".into(),
            from_port: "image".into(),
            envelope: Arc::new(env),
        };
        let after_ocr = VisionOcrNodeAdapter::new()
            .execute(&node("vision_ocr"), &[ocr_input], &ctx)
            .await
            .unwrap();
        // vision_ocr passes the frame through so classify sees the same Image.
        assert!(matches!(after_ocr.payload, FlowValue::Image { .. }));

        let cls_input = NodeInput {
            from_node_id: "vision_ocr".into(),
            from_port: "out".into(),
            envelope: Arc::new(after_ocr),
        };
        let after_cls = VisionClassifyNodeAdapter::new()
            .execute(&node("vision_classify"), &[cls_input], &ctx)
            .await
            .unwrap();

        // OCR ran once, on the plate crop only (20x20), never the 40x40 frame.
        assert_eq!(*fake.ocr_crops.lock().unwrap(), vec![(20, 20)]);
        // Classify ran once, on the sticker crop only.
        assert_eq!(*fake.classify_crops.lock().unwrap(), vec![(20, 20)]);

        let enriched: Vec<Detection> =
            serde_json::from_value(after_cls.meta.get("detections").unwrap().clone()).unwrap();
        assert_eq!(enriched[0].tekst.as_deref(), Some("WX 12345"));
        assert!(enriched[0].stan.is_empty());
        assert_eq!(enriched[1].stan, vec!["pełna".to_string()]);
        assert_eq!(enriched[1].tekst, None);
        // The non-worthy detection is untouched by both nodes.
        assert_eq!(enriched[2].tekst, None);
        assert!(enriched[2].stan.is_empty());

        // Continue the chain: verdict over the enriched detections, then alert.
        // Readable plate + clean placard → verdict "ok" → alert not emitted.
        let verdict_input = NodeInput {
            from_node_id: "vision_classify".into(),
            from_port: "out".into(),
            envelope: Arc::new(after_cls),
        };
        let after_verdict = CameraVerdictNodeAdapter::new()
            .execute(&node("camera_verdict"), &[verdict_input], &ctx)
            .await
            .unwrap();
        assert_eq!(after_verdict.meta.get("verdict").unwrap()["decision"], "ok");

        let alert_input = NodeInput {
            from_node_id: "camera_verdict".into(),
            from_port: "out".into(),
            envelope: Arc::new(after_verdict),
        };
        let after_alert = CameraAlertNodeAdapter::new()
            .execute(&node("camera_alert"), &[alert_input], &ctx)
            .await
            .unwrap();
        assert_eq!(after_alert.meta.get("alert").unwrap()["emitted"], false);
    }
}
