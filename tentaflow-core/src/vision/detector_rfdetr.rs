// =============================================================================
// File: vision/detector_rfdetr.rs — RF-DETR ADR detector (Burn)
// =============================================================================
//
// Always-on ADR (dangerous-goods placards / labels) detector for the Orlen
// camera-CV PoC. The architecture is the vendored `burn_rfdetr` model (build-time
// ONNX→Burn codegen); weights load at runtime from `rfdetr-base.bpk`. Runs on the
// backend chosen by the `vision-*` feature (CUDA/Metal/ROCm native, wgpu fallback).
//
// The preprocessing mirrors the reference `model.predict` 1:1: RGB → 560×560
// bilinear STRETCH (no letterbox) → /255 → per-channel ImageNet normalize →
// NCHW f32 [N,3,560,560]. DETR head → per-query sigmoid + argmax over the 17 real
// classes (index 17 is the background/ignore slot), NO NMS.

#![cfg(feature = "inference-vision-gpu")]

use anyhow::{anyhow, bail, Context, Result};
use burn::tensor::{Tensor, TensorData};
use burn_store::{BurnpackStore, ModuleSnapshot};
use serde::Deserialize;
use tracing::info;

use crate::paths;
use crate::services::detection_bus::Detection;
use crate::vision::burn_backend::{self, VisionBackend, VisionDevice};
use crate::vision::burn_rfdetr::Model;

/// Square input resolution the exported RF-DETR graph expects.
const RESOLUTION: u32 = 560;

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

/// Loaded RF-DETR model + class-name table + backend device. `detect`/`detect_batch`
/// keep `&mut self` so the cross-camera engine can hold it behind a single mutex.
pub struct RfDetrDetector {
    model: Model<VisionBackend>,
    device: VisionDevice,
    classes: Vec<String>,
}

impl RfDetrDetector {
    /// Builds the detector from the deploy-time model dir
    /// (`vision_models_dir()/rfdetr-{base.bpk,classes.json}`).
    pub fn load() -> Result<Self> {
        let dir = paths::vision_models_dir();
        let weights_path = dir.join("rfdetr-base.bpk");
        let classes_path = dir.join("rfdetr-classes.json");

        let classes_bytes = std::fs::read(&classes_path)
            .with_context(|| format!("read {}", classes_path.display()))?;
        let parsed: ClassesFile = serde_json::from_slice(&classes_bytes)
            .with_context(|| format!("parse {}", classes_path.display()))?;
        if parsed.classes.is_empty() {
            bail!("rfdetr-classes.json has no classes");
        }

        if !weights_path.exists() {
            bail!("RF-DETR weights missing: {}", weights_path.display());
        }
        let device = burn_backend::device();
        let mut model = Model::<VisionBackend>::new(&device);
        let mut store = BurnpackStore::from_file(&weights_path);
        model
            .load_from(&mut store)
            .map_err(|e| anyhow!("load RF-DETR weights {}: {e}", weights_path.display()))?;

        info!(
            "[rfdetr] loaded {} ({} classes, backend {})",
            weights_path.display(),
            parsed.classes.len(),
            std::any::type_name::<VisionBackend>()
        );
        Ok(Self {
            model,
            device,
            classes: parsed.classes,
        })
    }

    /// Single-frame convenience. Delegates to `detect_batch` (N=1) so there is
    /// exactly one preprocess + postprocess code path — a single live camera
    /// gets bit-identical results to the batched fleet path.
    pub fn detect(&mut self, rgb: &[u8], w: u32, h: u32) -> Result<Vec<Detection>> {
        Ok(self.detect_batch(&[(rgb, w, h)])?.into_iter().next().unwrap_or_default())
    }

    /// Stacks N camera frames into one `[N,3,560,560]` batch and runs a single
    /// forward — one GPU launch amortized across the fleet.
    pub fn detect_batch(&mut self, frames: &[(&[u8], u32, u32)]) -> Result<Vec<Vec<Detection>>> {
        if frames.is_empty() {
            return Ok(Vec::new());
        }
        let n = frames.len();
        let res = RESOLUTION as usize;
        let mut data = vec![0f32; n * 3 * res * res];
        for (bi, &(rgb, w, h)) in frames.iter().enumerate() {
            fill_frame(&mut data, bi, rgb, w, h)?;
        }
        let input =
            Tensor::<VisionBackend, 4>::from_data(TensorData::new(data, [n, 3, res, res]), &self.device);

        let (o0, o1) =
            crate::vision::burn_backend::guarded_forward("rfdetr", || self.model.forward(input))?;
        // dets last dim = 4 (cxcywh), labels last dim = num_classes + background.
        let (dets_t, labels_t) = if o0.dims()[2] == 4 { (o0, o1) } else { (o1, o0) };
        let queries = dets_t.dims()[1];
        let label_dim = labels_t.dims()[2];

        let dets_v: Vec<f32> = dets_t
            .to_data()
            .to_vec()
            .map_err(|e| anyhow!("dets to_vec: {e:?}"))?;
        let labels_v: Vec<f32> = labels_t
            .to_data()
            .to_vec()
            .map_err(|e| anyhow!("labels to_vec: {e:?}"))?;

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

/// Writes one RGB24 frame into batch slot `bi` of a flat NCHW buffer:
/// stretch-resize to 560×560, /255, per-channel ImageNet normalize.
fn fill_frame(data: &mut [f32], bi: usize, rgb: &[u8], w: u32, h: u32) -> Result<()> {
    let res = RESOLUTION as usize;
    let resized = crate::vision::resize::resize_rgb(rgb, w, h, RESOLUTION, RESOLUTION)
        .map_err(|e| anyhow!("resize_rgb failed: {e}"))?;
    let plane = res * res;
    let base = bi * 3 * plane;
    for y in 0..res {
        for x in 0..res {
            let p = (y * res + x) * 3;
            for c in 0..3 {
                let v = resized[p + c] as f32 / 255.0;
                data[base + c * plane + y * res + x] = (v - MEAN[c]) / STD[c];
            }
        }
    }
    Ok(())
}

#[inline]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}
