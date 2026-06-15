// =============================================================================
// File: vision/detector_rfdetr.rs — RF-DETR ADR detector (ONNX via `ort`)
// =============================================================================
//
// Always-on ADR (dangerous-goods placards / labels) detector for the Acme
// camera-CV PoC. Loads `rfdetr-base.onnx` through ONNX Runtime (`ort`, CPU EP)
// and produces `detection_bus::Detection` items so the live overlay renders
// real detections instead of the dev stub.
//
// The preprocessing mirrors the reference `model.predict` 1:1: RGB → 560×560
// bilinear STRETCH (no letterbox) → /255 → per-channel ImageNet normalize →
// NCHW f32 [1,3,560,560]. The model is a DETR head, so postprocessing is a
// per-query sigmoid + argmax over the 17 real classes (index 17 is the
// background/ignore slot) with NO NMS.

#![cfg(feature = "inference-vision-gpu")]

use std::path::Path;

use anyhow::{anyhow, bail, Context, Result};
use ndarray::Array4;
use ort::session::Session;
use ort::value::Value;
use serde::Deserialize;
use tracing::info;

use crate::paths;
use crate::services::detection_bus::Detection;

/// Square input resolution the exported RF-DETR graph expects.
const RESOLUTION: u32 = 560;

/// Number of object queries emitted by the decoder (`dets`/`labels` dim 1).
const NUM_QUERIES: usize = 300;

/// Per-channel ImageNet normalization (matches the training transform).
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// Minimum sigmoid confidence to surface a detection.
const SCORE_THRESHOLD: f32 = 0.5;

/// `rfdetr-classes.json` shape: `{ "classes": [...], "resolution": 560 }`.
#[derive(Debug, Deserialize)]
struct ClassesFile {
    classes: Vec<String>,
    #[allow(dead_code)]
    resolution: u32,
}

/// Loaded RF-DETR session plus the class-name table. `Session::run` needs
/// `&mut`, so `detect` takes `&mut self`; a single shared instance is driven
/// from one analysis task (or behind a mutex when shared across cameras).
pub struct RfDetrDetector {
    session: Session,
    classes: Vec<String>,
    /// Graph input tensor name, read from the model so we do not hard-code it.
    input_name: String,
}

impl RfDetrDetector {
    /// Builds the detector from the deploy-time model dir
    /// (`vision_models_dir()/rfdetr-{base.onnx,classes.json}`). CPU EP for now.
    pub fn load() -> Result<Self> {
        let dir = paths::vision_models_dir();
        let model_path = dir.join("rfdetr-base.onnx");
        let classes_path = dir.join("rfdetr-classes.json");

        let classes_bytes = std::fs::read(&classes_path)
            .with_context(|| format!("read {}", classes_path.display()))?;
        let parsed: ClassesFile = serde_json::from_slice(&classes_bytes)
            .with_context(|| format!("parse {}", classes_path.display()))?;
        if parsed.classes.is_empty() {
            bail!("rfdetr-classes.json has no classes");
        }

        let session = super::ort_session::build_session(&model_path)?;

        let input_name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| anyhow!("RF-DETR model has no inputs"))?;

        info!(
            "[rfdetr] loaded {} ({} classes, input '{}')",
            model_path.display(),
            parsed.classes.len(),
            input_name
        );
        Ok(Self {
            session,
            classes: parsed.classes,
            input_name,
        })
    }

    /// Single-frame convenience. Delegates to `detect_batch` (N=1) so there is
    /// exactly one preprocess + postprocess code path — a single live camera
    /// gets bit-identical results to the batched fleet path.
    pub fn detect(&mut self, rgb: &[u8], w: u32, h: u32) -> Result<Vec<Detection>> {
        Ok(self.detect_batch(&[(rgb, w, h)])?.pop().unwrap_or_default())
    }

    /// Batched inference across cameras: stacks N frames into one
    /// `[N,3,560,560]` tensor and issues a SINGLE `Session::run`. Returns
    /// per-frame detections in the same order as `frames`. This is the fleet
    /// throughput lever — one GPU launch amortized over many cameras.
    pub fn detect_batch(&mut self, frames: &[(&[u8], u32, u32)]) -> Result<Vec<Vec<Detection>>> {
        if frames.is_empty() {
            return Ok(Vec::new());
        }
        let n = frames.len();
        let res = RESOLUTION as usize;
        let mut tensor = Array4::<f32>::zeros((n, 3, res, res));
        for (bi, &(rgb, w, h)) in frames.iter().enumerate() {
            fill_frame(&mut tensor, bi, rgb, w, h)?;
        }
        let input_value = Value::from_array(tensor)?;

        // Run + copy outputs to owned buffers inside this scope so the session
        // borrow is released before postprocessing (which reads `self.classes`).
        let (dets_v, labels_v, queries, label_dim) = {
            let outputs = self
                .session
                .run(ort::inputs! { self.input_name.as_str() => &input_value })?;
            let (dets_shape, dets) = outputs["dets"].try_extract_tensor::<f32>()?;
            let (labels_shape, labels) = outputs["labels"].try_extract_tensor::<f32>()?;

            if dets_shape.len() != 3 || dets_shape[2] != 4 {
                bail!("unexpected dets shape {:?}", &*dets_shape);
            }
            if labels_shape.len() != 3 {
                bail!("unexpected labels shape {:?}", &*labels_shape);
            }
            if dets_shape[0] as usize != n || labels_shape[0] as usize != n {
                bail!(
                    "batch mismatch: requested {} got dets {} labels {}",
                    n,
                    dets_shape[0],
                    labels_shape[0]
                );
            }
            (
                dets.to_vec(),
                labels.to_vec(),
                dets_shape[1] as usize,
                labels_shape[2] as usize,
            )
        };
        let num_classes = self.classes.len();
        if label_dim <= num_classes {
            bail!(
                "labels dim {} must exceed class count {} (background slot)",
                label_dim,
                num_classes
            );
        }

        let mut results = Vec::with_capacity(n);
        for bi in 0..n {
            let dets_off = bi * queries * 4;
            let labels_off = bi * queries * label_dim;
            results.push(self.postprocess_image(
                &dets_v[dets_off..dets_off + queries * 4],
                &labels_v[labels_off..labels_off + queries * label_dim],
                queries,
                label_dim,
                num_classes,
            ));
        }
        Ok(results)
    }

    /// Per-image DETR postprocess: per-query sigmoid + argmax over the real
    /// classes (index `num_classes` is the background slot), threshold, and
    /// cxcywh→xywh-normalized box. No NMS.
    fn postprocess_image(
        &self,
        dets: &[f32],
        labels: &[f32],
        queries: usize,
        label_dim: usize,
        num_classes: usize,
    ) -> Vec<Detection> {
        let mut items = Vec::new();
        for q in 0..queries {
            let logits = &labels[q * label_dim..q * label_dim + label_dim];
            let mut best_idx = 0usize;
            let mut best_logit = f32::NEG_INFINITY;
            for (idx, &l) in logits.iter().take(num_classes).enumerate() {
                if l > best_logit {
                    best_logit = l;
                    best_idx = idx;
                }
            }
            let score = sigmoid(best_logit);
            if score <= SCORE_THRESHOLD {
                continue;
            }
            let base = q * 4;
            let cx = dets[base];
            let cy = dets[base + 1];
            let bw = dets[base + 2];
            let bh = dets[base + 3];
            let x1 = (cx - bw / 2.0).clamp(0.0, 1.0);
            let y1 = (cy - bh / 2.0).clamp(0.0, 1.0);
            let x2 = (cx + bw / 2.0).clamp(0.0, 1.0);
            let y2 = (cy + bh / 2.0).clamp(0.0, 1.0);
            items.push(Detection {
                klasa: self.classes[best_idx].clone(),
                bbox: [x1, y1, x2 - x1, y2 - y1],
                score,
                stan: Vec::new(),
                tekst: None,
            });
        }
        items
    }
}

/// Writes one RGB24 frame into batch slot `bi` of an NCHW tensor:
/// stretch-resize to 560×560, /255, per-channel ImageNet normalize.
fn fill_frame(tensor: &mut Array4<f32>, bi: usize, rgb: &[u8], w: u32, h: u32) -> Result<()> {
    let resized = crate::vision::resize::resize_rgb(rgb, w, h, RESOLUTION, RESOLUTION)
        .map_err(|e| anyhow!("resize_rgb failed: {e}"))?;
    let res = RESOLUTION as usize;
    for y in 0..res {
        for x in 0..res {
            let p = (y * res + x) * 3;
            for c in 0..3 {
                let v = resized[p + c] as f32 / 255.0;
                tensor[[bi, c, y, x]] = (v - MEAN[c]) / STD[c];
            }
        }
    }
    let _ = NUM_QUERIES;
    Ok(())
}

/// Single-frame NCHW tensor [1,3,560,560] — used by tests.
#[cfg(test)]
fn preprocess(rgb: &[u8], w: u32, h: u32) -> Result<Array4<f32>> {
    let res = RESOLUTION as usize;
    let mut tensor = Array4::<f32>::zeros((1, 3, res, res));
    fill_frame(&mut tensor, 0, rgb, w, h)?;
    Ok(tensor)
}

#[inline]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigmoid_midpoint_is_half() {
        assert!((sigmoid(0.0) - 0.5).abs() < 1e-6);
        assert!(sigmoid(10.0) > 0.99);
        assert!(sigmoid(-10.0) < 0.01);
    }

    #[test]
    fn preprocess_produces_normalized_nchw() {
        // 2x2 white frame → every channel value is (1 - mean)/std after norm.
        let rgb = vec![255u8; 2 * 2 * 3];
        let t = preprocess(&rgb, 2, 2).expect("preprocess");
        assert_eq!(t.shape(), &[1, 3, RESOLUTION as usize, RESOLUTION as usize]);
        for c in 0..3 {
            let expected = (1.0 - MEAN[c]) / STD[c];
            let got = t[[0, c, 0, 0]];
            assert!((got - expected).abs() < 1e-4, "channel {c}: {got} vs {expected}");
        }
    }
}
