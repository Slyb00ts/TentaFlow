// =============================================================================
// Plik: vision/scrfd.rs
// Opis: SCRFD (InsightFace) face detector. Pure Rust ONNX inference przez
//       tract-onnx. Model `det_500m.onnx` z buffalo_s.
//
//       Architektura SCRFD: 3 FPN strides (8 / 16 / 32), 2 anchory per pozycja,
//       3 heady (score / bbox / kps) — razem 9 output tensorow.
//       Dla input 640x640 liczba anchorow per stride:
//         stride 8  → 80*80*2 = 12800
//         stride 16 → 40*40*2 = 3200
//         stride 32 → 20*20*2 = 800
//       Razem 16800 candidate boxes per forward.
//
//       Decode (referencja: insightface/python/scrfd.py):
//         anchor_centers[i] = (sx*stride, sy*stride)        // top-left grid pos
//         bbox = distance2bbox(anchor_centers, bbox_preds * stride)
//             x1 = ac.x - dl, y1 = ac.y - dt
//             x2 = ac.x + dr, y2 = ac.y + db
//         kp[k] = ac + kp_pred[k] * stride  // 5 punktow (oczy, nos, kaciki ust)
// =============================================================================

use std::path::Path;
use std::sync::Arc;

use tract_onnx::prelude::TValue;

use anyhow::{anyhow, Context, Result};
use tract_onnx::prelude::*;

use super::nms::nms;
use super::preprocessing::{letterbox, rgb_buf_to_image, rgb_to_nchw_scrfd, unletterbox_xy};
use super::{FaceDetection, FaceDetector};

const SCRFD_INPUT_SIZE: u32 = 640;
const SCORE_THRESHOLD: f32 = 0.5;
const NMS_IOU_THRESHOLD: f32 = 0.4;
const STRIDES: [u32; 3] = [8, 16, 32];
const NUM_ANCHORS: usize = 2;

type RunnableScrfd = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

pub struct ScrfdEngine {
    model: Arc<RunnableScrfd>,
}

impl ScrfdEngine {
    pub fn new(model_path: &Path) -> Result<Self> {
        let model = tract_onnx::onnx()
            .model_for_path(model_path)
            .with_context(|| format!("tract: SCRFD ONNX z {}", model_path.display()))?
            .with_input_fact(
                0,
                InferenceFact::dt_shape(
                    f32::datum_type(),
                    tvec!(1, 3, SCRFD_INPUT_SIZE as i32, SCRFD_INPUT_SIZE as i32),
                ),
            )?
            .into_optimized()?
            .into_runnable()?;
        Ok(Self {
            model: Arc::new(model),
        })
    }
}

impl FaceDetector for ScrfdEngine {
    fn detect(&self, image_rgb: &[u8], width: u32, height: u32) -> Result<Vec<FaceDetection>> {
        let img = rgb_buf_to_image(image_rgb, width, height).ok_or_else(|| {
            anyhow!(
                "SCRFD: invalid RGB buffer ({} bytes for {}x{})",
                image_rgb.len(),
                width,
                height
            )
        })?;

        let (canvas, meta) = letterbox(&img, SCRFD_INPUT_SIZE, [0, 0, 0]);
        let nchw = rgb_to_nchw_scrfd(&canvas);

        let input: Tensor = tract_ndarray::Array4::from_shape_vec(
            (1, 3, SCRFD_INPUT_SIZE as usize, SCRFD_INPUT_SIZE as usize),
            nchw,
        )
        .context("SCRFD: nchw shape mismatch")?
        .into();

        let outputs = self
            .model
            .run(tvec!(input.into()))
            .context("SCRFD: tract forward failed")?;

        // Sortujemy 9 wyjsc po heurystyce rozmiaru (3-cia oska): 1=score, 4=bbox, 10=kps.
        // 1-sza oska (po batch) decyduje stride: 12800/3200/800 dla 640x640.
        let mut buckets: Vec<TensorBucket> = Vec::with_capacity(9);
        for t in outputs.iter() {
            let shape = t.shape();
            if shape.len() < 3 {
                continue;
            }
            let n = shape[1];
            let c = shape[2];
            let head = match c {
                1 => Head::Score,
                4 => Head::Bbox,
                10 => Head::Kps,
                _ => continue,
            };
            buckets.push(TensorBucket {
                head,
                anchors: n,
                tensor: t.clone(),
            });
        }

        // Per stride zbieramy trzy gleby (score, bbox, kps) — match po anchors count.
        let mut detections: Vec<FaceDetection> = Vec::new();
        for &stride in &STRIDES {
            let feat = (SCRFD_INPUT_SIZE / stride) as usize;
            let expected = feat * feat * NUM_ANCHORS;
            let score = match find_bucket(&buckets, Head::Score, expected) {
                Some(b) => b,
                None => {
                    tracing::warn!(
                        "SCRFD: brak score head dla stride {} (expected {} anchors)",
                        stride,
                        expected
                    );
                    continue;
                }
            };
            let bbox = match find_bucket(&buckets, Head::Bbox, expected) {
                Some(b) => b,
                None => continue,
            };
            let kps = find_bucket(&buckets, Head::Kps, expected);

            let scores = score
                .tensor
                .as_slice::<f32>()
                .context("SCRFD: score tensor nie jest f32")?;
            let bboxes = bbox
                .tensor
                .as_slice::<f32>()
                .context("SCRFD: bbox tensor nie jest f32")?;
            let kps_slice = match kps {
                Some(k) => k.tensor.as_slice::<f32>().ok(),
                None => None,
            };

            decode_stride(
                stride,
                feat,
                scores,
                bboxes,
                kps_slice,
                &meta,
                &mut detections,
            );
        }

        let kept = nms(detections, NMS_IOU_THRESHOLD);
        Ok(kept)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Head {
    Score,
    Bbox,
    Kps,
}

struct TensorBucket {
    head: Head,
    anchors: usize,
    tensor: TValue,
}

fn find_bucket(buckets: &[TensorBucket], head: Head, anchors: usize) -> Option<&TensorBucket> {
    buckets
        .iter()
        .find(|b| b.head == head && b.anchors == anchors)
}

/// Decode jednego stride'a. `scores` (anchors,1), `bboxes` (anchors,4),
/// `kps` (anchors,10) opcjonalne. Anchor center dla pozycji (sy, sx, a):
/// `(sx * stride, sy * stride)` — top-left grid (NIE pixel center) zgodnie
/// z `insightface/python/scrfd.py::generate_anchors_centers`.
fn decode_stride(
    stride: u32,
    feat: usize,
    scores: &[f32],
    bboxes: &[f32],
    kps: Option<&[f32]>,
    meta: &super::preprocessing::LetterboxMeta,
    out: &mut Vec<FaceDetection>,
) {
    let stride_f = stride as f32;
    let mut idx = 0usize;
    for sy in 0..feat {
        for sx in 0..feat {
            for _a in 0..NUM_ANCHORS {
                let s = scores[idx];
                if s >= SCORE_THRESHOLD {
                    let cx = sx as f32 * stride_f;
                    let cy = sy as f32 * stride_f;

                    let bb = idx * 4;
                    let dl = bboxes[bb] * stride_f;
                    let dt = bboxes[bb + 1] * stride_f;
                    let dr = bboxes[bb + 2] * stride_f;
                    let db = bboxes[bb + 3] * stride_f;

                    // Bbox w pikselach letterbox'a (640x640). Unletterbox
                    // odwzorowuje do oryginalnego obrazka.
                    let (x1, y1) = unletterbox_xy(cx - dl, cy - dt, meta);
                    let (x2, y2) = unletterbox_xy(cx + dr, cy + db, meta);

                    let keypoints = kps.map(|k| {
                        let kb = idx * 10;
                        let mut pts = [(0f32, 0f32); 5];
                        for j in 0..5 {
                            let kx = cx + k[kb + j * 2] * stride_f;
                            let ky = cy + k[kb + j * 2 + 1] * stride_f;
                            pts[j] = unletterbox_xy(kx, ky, meta);
                        }
                        pts
                    });

                    out.push(FaceDetection {
                        bbox: (x1, y1, x2, y2),
                        score: s,
                        keypoints,
                    });
                }
                idx += 1;
            }
        }
    }
}

pub fn load(model_path: &Path) -> Result<ScrfdEngine> {
    if !model_path.exists() {
        return Err(anyhow!(
            "SCRFD ONNX nie istnieje: {} (uruchom setup.sh)",
            model_path.display()
        ));
    }
    ScrfdEngine::new(model_path)
}
