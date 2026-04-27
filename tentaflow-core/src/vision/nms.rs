// =============================================================================
// Plik: vision/nms.rs
// Opis: Non-Maximum Suppression dla detekcji bounding boxow. Klasyczny
//       greedy IoU-based NMS — sortujemy po score malejaco, zachowujemy
//       boxy ktorych IoU z juz przyjetymi jest < threshold.
//
//       Implementacja jest sync (CPU bound) i alokuje raz Vec<bool> o
//       rozmiarze N. Dla typowego SCRFD/YOLO output (do ~1000 candidates
//       po score thresholdzie) to mikro-sekundy.
// =============================================================================

use super::FaceDetection;

/// IoU dwoch bboxow (x1, y1, x2, y2).
#[inline]
fn iou(a: &(f32, f32, f32, f32), b: &(f32, f32, f32, f32)) -> f32 {
    let ix1 = a.0.max(b.0);
    let iy1 = a.1.max(b.1);
    let ix2 = a.2.min(b.2);
    let iy2 = a.3.min(b.3);
    let iw = (ix2 - ix1).max(0.0);
    let ih = (iy2 - iy1).max(0.0);
    let inter = iw * ih;
    let area_a = (a.2 - a.0).max(0.0) * (a.3 - a.1).max(0.0);
    let area_b = (b.2 - b.0).max(0.0) * (b.3 - b.1).max(0.0);
    let union = area_a + area_b - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// In-place NMS na liscie detekcji. Sortuje po score malejaco i zachowuje
/// te ktore maja IoU < `iou_threshold` z poprzedzajacymi je (juz przyjetymi).
pub fn nms(detections: Vec<FaceDetection>, iou_threshold: f32) -> Vec<FaceDetection> {
    if detections.is_empty() {
        return detections;
    }

    let mut idx: Vec<usize> = (0..detections.len()).collect();
    idx.sort_by(|a, b| {
        detections[*b]
            .score
            .partial_cmp(&detections[*a].score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut keep = Vec::with_capacity(detections.len());
    let mut suppressed = vec![false; detections.len()];

    for &i in &idx {
        if suppressed[i] {
            continue;
        }
        keep.push(detections[i].clone());
        // Po przyjeciu i, suppress wszystko co IoU z i >= threshold.
        for &j in idx.iter().skip_while(|&&x| x != i).skip(1) {
            if !suppressed[j] && iou(&detections[i].bbox, &detections[j].bbox) >= iou_threshold {
                suppressed[j] = true;
            }
        }
    }

    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(x1: f32, y1: f32, x2: f32, y2: f32, score: f32) -> FaceDetection {
        FaceDetection {
            bbox: (x1, y1, x2, y2),
            score,
            keypoints: None,
        }
    }

    #[test]
    fn nms_pusta_lista() {
        let r = nms(vec![], 0.5);
        assert!(r.is_empty());
    }

    #[test]
    fn nms_zostawia_jedyny_box() {
        let r = nms(vec![d(0.0, 0.0, 10.0, 10.0, 0.9)], 0.5);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn nms_usuwa_overlapping_lower_score() {
        let r = nms(
            vec![
                d(0.0, 0.0, 10.0, 10.0, 0.5),
                d(1.0, 1.0, 11.0, 11.0, 0.9), // bardziej confident, IoU duze
            ],
            0.4,
        );
        assert_eq!(r.len(), 1);
        assert!((r[0].score - 0.9).abs() < 1e-6);
    }

    #[test]
    fn nms_zostawia_disjoint_boxes() {
        let r = nms(
            vec![
                d(0.0, 0.0, 10.0, 10.0, 0.9),
                d(20.0, 20.0, 30.0, 30.0, 0.8), // brak overlap
            ],
            0.5,
        );
        assert_eq!(r.len(), 2);
    }
}
