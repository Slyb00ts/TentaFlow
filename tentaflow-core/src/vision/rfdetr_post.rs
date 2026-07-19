// =============================================================================
// File: vision/rfdetr_post.rs — shared RF-DETR postprocess (DETR head → boxes)
// =============================================================================
//
// Backend-agnostyczny postprocess wyjscia RF-DETR, wydzielony z
// `detector_rfdetr.rs` i sparametryzowany klasami + progiem, zeby dynamiczne
// modele rejestru `vision_models` (kontrakt `rfdetr`, runner `onnx_cv`) i
// wbudowany detektor ADR (ort i Burn) liczyly wspolrzedne co do bitu tak samo.

#![cfg(any(feature = "inference-vision-gpu", feature = "inference-supertonic"))]

use crate::services::detection_bus::Detection;

/// Minimum sigmoid confidence to surface a detection when the caller passes
/// no explicit threshold (the historical RF-DETR default).
pub const DEFAULT_SCORE_THRESHOLD: f32 = 0.5;

/// Per-image DETR postprocess: per-query sigmoid + argmax over the real
/// classes (index `classes.len()` is the background slot), threshold, and
/// cxcywh→xywh-normalized box. No NMS. `threshold = None` uses
/// [`DEFAULT_SCORE_THRESHOLD`].
///
/// Caller contract (validated by both backends before slicing): `dets` is one
/// image slot `[queries * 4]` (cxcywh), `labels` is `[queries * label_dim]`
/// with `label_dim > classes.len()`.
pub fn postprocess_image(
    dets: &[f32],
    labels: &[f32],
    queries: usize,
    label_dim: usize,
    classes: &[String],
    threshold: Option<f32>,
) -> Vec<Detection> {
    let num_classes = classes.len();
    let score_threshold = threshold.unwrap_or(DEFAULT_SCORE_THRESHOLD);
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
        if score <= score_threshold {
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
            klasa: classes[best_idx].clone(),
            bbox: [x1, y1, x2 - x1, y2 - y1],
            score,
            stan: Vec::new(),
            tekst: None,
            track_id: 0,
            vx: 0.,
            vy: 0.,
        });
    }
    items
}

#[inline]
pub fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classes(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("class-{i}")).collect()
    }

    /// One query with a strong logit on class 1 must produce exactly one
    /// detection with the parameterized class name and a cxcywh→xywh box.
    #[test]
    fn strong_query_maps_class_and_box() {
        let cls = classes(3);
        // label_dim = 4 (3 classes + background slot).
        let labels = vec![-10.0, 5.0, -10.0, 0.0];
        // cxcywh: centered box 0.5,0.5 size 0.2x0.4 → xywh 0.4,0.3,0.2,0.4.
        let dets = vec![0.5, 0.5, 0.2, 0.4];
        let out = postprocess_image(&dets, &labels, 1, 4, &cls, None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].klasa, "class-1");
        let b = out[0].bbox;
        assert!((b[0] - 0.4).abs() < 1e-6 && (b[1] - 0.3).abs() < 1e-6);
        assert!((b[2] - 0.2).abs() < 1e-6 && (b[3] - 0.4).abs() < 1e-6);
        assert!(out[0].score > 0.99);
    }

    /// The threshold parameter (Chunk 2 `CameraCvOp::Detect.threshold`) must
    /// gate detections: a sigmoid(1.0)≈0.73 query passes at 0.5 and is
    /// dropped at 0.9.
    #[test]
    fn explicit_threshold_gates_detections() {
        let cls = classes(2);
        let labels = vec![1.0, -10.0, 0.0];
        let dets = vec![0.5, 0.5, 0.1, 0.1];
        assert_eq!(
            postprocess_image(&dets, &labels, 1, 3, &cls, Some(0.5)).len(),
            1
        );
        assert_eq!(
            postprocess_image(&dets, &labels, 1, 3, &cls, Some(0.9)).len(),
            0
        );
    }

    /// The background slot (last logit) must be ignored by the argmax even
    /// when it dominates every real class.
    #[test]
    fn background_slot_is_ignored() {
        let cls = classes(2);
        // Background logit huge, real classes weak → best real class decides.
        let labels = vec![-3.0, -2.0, 50.0];
        let dets = vec![0.5, 0.5, 0.1, 0.1];
        let out = postprocess_image(&dets, &labels, 1, 3, &cls, Some(0.05));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].klasa, "class-1");
    }
}
