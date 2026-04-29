// =============================================================================
// File: vision/movenet.rs
// Description: MoveNet Lightning single-person pose estimator through ONNX.
// =============================================================================

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use image::imageops::FilterType;
use tract_onnx::prelude::*;

use super::preprocessing::rgb_buf_to_image;
use super::yolo_pose::COCO_KEYPOINT_NAMES;
use super::{PoseDetection, PoseEstimator, PoseKeypoint};

const INPUT_SIZE: u32 = 192;
const KEYPOINT_THRESHOLD: f32 = 0.2;

type Runnable = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

pub struct MovenetEngine {
    model: Arc<Runnable>,
}

impl PoseEstimator for MovenetEngine {
    fn estimate(&self, image_rgb: &[u8], width: u32, height: u32) -> Result<Vec<PoseDetection>> {
        let img = rgb_buf_to_image(image_rgb, width, height).ok_or_else(|| {
            anyhow!(
                "MoveNet: invalid RGB buffer ({} bytes for {}x{})",
                image_rgb.len(),
                width,
                height
            )
        })?;

        let resized = image::imageops::resize(&img, INPUT_SIZE, INPUT_SIZE, FilterType::Triangle);
        let mut nhwc = Vec::with_capacity((INPUT_SIZE * INPUT_SIZE * 3) as usize);
        for p in resized.pixels() {
            nhwc.push(p[0] as i32);
            nhwc.push(p[1] as i32);
            nhwc.push(p[2] as i32);
        }

        let input: Tensor = tract_ndarray::Array4::from_shape_vec(
            (1, INPUT_SIZE as usize, INPUT_SIZE as usize, 3),
            nhwc,
        )
        .context("MoveNet: nhwc shape mismatch")?
        .into();

        let outputs = self
            .model
            .run(tvec!(input.into()))
            .context("MoveNet: tract forward failed")?;
        let out = outputs
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("MoveNet: missing output tensor"))?;
        let shape = out.shape().to_vec();
        let data = out
            .as_slice::<f32>()
            .context("MoveNet: output is not f32")?;

        if data.len() < 17 * 3 {
            return Err(anyhow!("MoveNet: unexpected output shape {:?}", shape));
        }

        let mut keypoints = Vec::with_capacity(17);
        for k in 0..17 {
            let base = k * 3;
            let y = data[base] * height as f32;
            let x = data[base + 1] * width as f32;
            let score = data[base + 2];
            if score >= KEYPOINT_THRESHOLD {
                keypoints.push(PoseKeypoint {
                    id: k as u8,
                    name: COCO_KEYPOINT_NAMES[k],
                    x,
                    y,
                    score,
                });
            }
        }

        if keypoints.is_empty() {
            return Ok(Vec::new());
        }

        let (x1, y1, x2, y2) = keypoint_bounds(&keypoints, width, height);
        let score = keypoints.iter().map(|k| k.score).sum::<f32>() / keypoints.len() as f32;
        Ok(vec![PoseDetection {
            bbox: (x1, y1, x2, y2),
            score,
            keypoints,
        }])
    }
}

pub fn load(model_path: &Path) -> Result<MovenetEngine> {
    if !model_path.exists() {
        return Err(anyhow!("MoveNet ONNX missing: {}", model_path.display()));
    }
    let model = tract_onnx::onnx()
        .model_for_path(model_path)
        .with_context(|| format!("tract: MoveNet ONNX from {}", model_path.display()))?
        .with_input_fact(
            0,
            InferenceFact::dt_shape(
                i32::datum_type(),
                tvec!(1, INPUT_SIZE as i32, INPUT_SIZE as i32, 3),
            ),
        )?
        .into_optimized()?
        .into_runnable()?;
    Ok(MovenetEngine {
        model: Arc::new(model),
    })
}

fn keypoint_bounds(keypoints: &[PoseKeypoint], width: u32, height: u32) -> (f32, f32, f32, f32) {
    let mut x1 = width as f32;
    let mut y1 = height as f32;
    let mut x2 = 0.0f32;
    let mut y2 = 0.0f32;
    for kp in keypoints {
        x1 = x1.min(kp.x);
        y1 = y1.min(kp.y);
        x2 = x2.max(kp.x);
        y2 = y2.max(kp.y);
    }
    let pad_x = (x2 - x1).max(1.0) * 0.15;
    let pad_y = (y2 - y1).max(1.0) * 0.15;
    (
        (x1 - pad_x).max(0.0),
        (y1 - pad_y).max(0.0),
        (x2 + pad_x).min(width as f32),
        (y2 + pad_y).min(height as f32),
    )
}
