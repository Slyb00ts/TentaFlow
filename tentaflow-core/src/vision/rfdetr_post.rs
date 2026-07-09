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
    // Conservative pre-gate in logit space: sigmoid is strictly monotonic, so a
    // query can only pass `sigmoid(best) > t` when `best > logit(t)`. The 1e-3
    // margin absorbs f32 rounding of ln/exp (orders of magnitude larger than
    // their error near the gate), so the pre-gate can only let borderline
    // queries THROUGH to the exact sigmoid check below — never drop one the
    // exact check would keep. Degenerate thresholds (<=0, >=1, NaN) disable the
    // gate entirely and fall through to the exact check, preserving historical
    // behavior bit-for-bit. This skips the per-query `exp` for the vast
    // majority of DETR queries (typically ~300 per image, few above threshold).
    let logit_gate = if score_threshold > 0.0 && score_threshold < 1.0 {
        (score_threshold / (1.0 - score_threshold)).ln() - 1e-3
    } else {
        f32::NEG_INFINITY
    };
    let mut items = Vec::new();
    for q in 0..queries {
        let logits = &labels[q * label_dim..q * label_dim + num_classes];
        // Pass 1: value-only running max over the real classes (background slot
        // excluded by the slice bound) — a branch-free select fold the compiler
        // can keep in registers. NaN logits never win (`>` is false), same as
        // the index-tracking fold below.
        let mut max_logit = f32::NEG_INFINITY;
        for &l in logits {
            max_logit = if l > max_logit { l } else { max_logit };
        }
        if max_logit < logit_gate {
            continue;
        }
        // Pass 2 (rare — only near/above threshold): the original first-max
        // argmax fold, so index tie-breaking and the reported score are exactly
        // the historical ones.
        let mut best_idx = 0usize;
        let mut best_logit = f32::NEG_INFINITY;
        for (idx, &l) in logits.iter().enumerate() {
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

    /// The logit-space pre-gate must be output-invisible: randomized logits
    /// (including values landing arbitrarily close to the threshold) decoded
    /// through `postprocess_image` must match a naive sigmoid-per-query
    /// reference bit-for-bit, for regular AND degenerate thresholds.
    #[test]
    fn logit_pregate_matches_naive_reference() {
        let queries = 400usize;
        let label_dim = 18usize;
        let num_classes = 17usize;
        let cls = classes(num_classes);

        // Deterministic LCG; logits spread across [-10, 10] so plenty of
        // queries straddle every tested threshold.
        let mut seed = 0x9E3779B97F4A7C15u64;
        let mut next = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 40) as f32 / (1u64 << 24) as f32) * 20.0 - 10.0
        };
        let labels: Vec<f32> = (0..queries * label_dim).map(|_| next()).collect();
        let dets: Vec<f32> = (0..queries * 4).map(|_| (next() + 10.0) / 20.0).collect();

        for thr in [None, Some(0.0), Some(0.05), Some(0.5), Some(0.9999), Some(1.0)] {
            let t = thr.unwrap_or(DEFAULT_SCORE_THRESHOLD);
            // Naive reference: unconditional argmax + sigmoid per query.
            let mut expected = Vec::new();
            for q in 0..queries {
                let logits = &labels[q * label_dim..q * label_dim + num_classes];
                let (mut bi, mut bl) = (0usize, f32::NEG_INFINITY);
                for (i, &l) in logits.iter().enumerate() {
                    if l > bl {
                        bl = l;
                        bi = i;
                    }
                }
                let score = sigmoid(bl);
                if score > t {
                    expected.push((q, bi, score));
                }
            }
            let got = postprocess_image(&dets, &labels, queries, label_dim, &cls, thr);
            assert_eq!(got.len(), expected.len(), "count mismatch at thr={thr:?}");
            for (d, (q, bi, score)) in got.iter().zip(&expected) {
                assert_eq!(d.klasa, cls[*bi], "class mismatch at thr={thr:?} q={q}");
                assert_eq!(
                    d.score.to_bits(),
                    score.to_bits(),
                    "score bits mismatch at thr={thr:?} q={q}"
                );
            }
        }
    }
}
