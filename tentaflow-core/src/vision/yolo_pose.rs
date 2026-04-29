// =============================================================================
// File: vision/yolo_pose.rs
// Description: YOLOv8 nano pose estimator through pure Rust ONNX inference.
// =============================================================================

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tract_onnx::prelude::*;

use super::preprocessing::{letterbox, rgb_buf_to_image, rgb_to_nchw_normalized, unletterbox_xy};
use super::{PoseDetection, PoseEstimator, PoseKeypoint};

const INPUT_SIZE: u32 = 640;
const SCORE_THRESHOLD: f32 = 0.35;
const KEYPOINT_THRESHOLD: f32 = 0.2;
const NMS_IOU_THRESHOLD: f32 = 0.45;
const NUM_KEYPOINTS: usize = 17;
const ATTRS: usize = 5 + NUM_KEYPOINTS * 3;

type Runnable = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

pub const COCO_KEYPOINT_NAMES: [&str; NUM_KEYPOINTS] = [
    "nose",
    "left_eye",
    "right_eye",
    "left_ear",
    "right_ear",
    "left_shoulder",
    "right_shoulder",
    "left_elbow",
    "right_elbow",
    "left_wrist",
    "right_wrist",
    "left_hip",
    "right_hip",
    "left_knee",
    "right_knee",
    "left_ankle",
    "right_ankle",
];

pub struct YoloPoseEngine {
    model: Arc<Runnable>,
}

impl PoseEstimator for YoloPoseEngine {
    fn estimate(&self, image_rgb: &[u8], width: u32, height: u32) -> Result<Vec<PoseDetection>> {
        let img = rgb_buf_to_image(image_rgb, width, height).ok_or_else(|| {
            anyhow!(
                "YOLO-pose: invalid RGB buffer ({} bytes for {}x{})",
                image_rgb.len(),
                width,
                height
            )
        })?;

        let (canvas, meta) = letterbox(&img, INPUT_SIZE, [114, 114, 114]);
        let nchw = rgb_to_nchw_normalized(&canvas, [0.0, 0.0, 0.0], [255.0, 255.0, 255.0]);
        let input: Tensor = tract_ndarray::Array4::from_shape_vec(
            (1, 3, INPUT_SIZE as usize, INPUT_SIZE as usize),
            nchw,
        )
        .context("YOLO-pose: nchw shape mismatch")?
        .into();

        let outputs = self
            .model
            .run(tvec!(input.into()))
            .context("YOLO-pose: tract forward failed")?;
        let out = outputs
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("YOLO-pose: missing output tensor"))?;
        let shape = out.shape().to_vec();
        let data = out
            .as_slice::<f32>()
            .context("YOLO-pose: output is not f32")?;

        let (attrs, anchors, transposed) = match shape.as_slice() {
            [1, a, b] if *a == ATTRS => (*a, *b, false),
            [1, a, b] if *b == ATTRS => (*b, *a, true),
            _ => return Err(anyhow!("YOLO-pose: unexpected output shape {:?}", shape)),
        };

        let mut poses = Vec::with_capacity(16);
        for i in 0..anchors {
            let get = |attr: usize| -> f32 {
                if transposed {
                    data[i * attrs + attr]
                } else {
                    data[attr * anchors + i]
                }
            };

            let score = get(4);
            if score < SCORE_THRESHOLD {
                continue;
            }

            let cx = get(0);
            let cy = get(1);
            let w = get(2);
            let h = get(3);
            let (x1, y1) = unletterbox_xy(cx - w * 0.5, cy - h * 0.5, &meta);
            let (x2, y2) = unletterbox_xy(cx + w * 0.5, cy + h * 0.5, &meta);

            let mut keypoints = Vec::with_capacity(NUM_KEYPOINTS);
            for k in 0..NUM_KEYPOINTS {
                let base = 5 + k * 3;
                let kp_score = get(base + 2);
                if kp_score < KEYPOINT_THRESHOLD {
                    continue;
                }
                let (x, y) = unletterbox_xy(get(base), get(base + 1), &meta);
                keypoints.push(PoseKeypoint {
                    id: k as u8,
                    name: COCO_KEYPOINT_NAMES[k],
                    x,
                    y,
                    score: kp_score,
                });
            }

            poses.push(PoseDetection {
                bbox: (x1, y1, x2, y2),
                score,
                keypoints,
            });
        }

        Ok(nms_pose(poses, NMS_IOU_THRESHOLD))
    }
}

pub fn load(model_path: &Path) -> Result<YoloPoseEngine> {
    if !model_path.exists() {
        return Err(anyhow!("YOLO-pose ONNX missing: {}", model_path.display()));
    }
    let model = tract_onnx::onnx()
        .model_for_path(model_path)
        .with_context(|| format!("tract: YOLO-pose ONNX from {}", model_path.display()))?
        .with_input_fact(
            0,
            InferenceFact::dt_shape(
                f32::datum_type(),
                tvec!(1, 3, INPUT_SIZE as i32, INPUT_SIZE as i32),
            ),
        )?
        .into_optimized()?
        .into_runnable()?;
    Ok(YoloPoseEngine {
        model: Arc::new(model),
    })
}

fn nms_pose(detections: Vec<PoseDetection>, iou_threshold: f32) -> Vec<PoseDetection> {
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
        for &j in &idx {
            if j == i || suppressed[j] {
                continue;
            }
            if bbox_iou(&detections[i].bbox, &detections[j].bbox) >= iou_threshold {
                suppressed[j] = true;
            }
        }
    }
    keep
}

fn bbox_iou(a: &(f32, f32, f32, f32), b: &(f32, f32, f32, f32)) -> f32 {
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
