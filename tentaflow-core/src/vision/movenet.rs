// =============================================================================
// File: vision/movenet.rs
// Description: MoveNet Lightning single-person pose estimator through ONNX.
// =============================================================================

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tract_onnx::prelude::*;

use super::preprocessing::rgb_buf_to_image;
use super::resize::resize_rgb_image;
use super::yolo_pose::COCO_KEYPOINT_NAMES;
use super::{PoseDetection, PoseEstimator, PoseKeypoint};

const INPUT_SIZE: u32 = 192;
const GRID: usize = 48;
const KEYPOINT_THRESHOLD: f32 = 0.2;
const DIST_OFFSET: f32 = 1.8;

// tract zawiesza optymalizacje na prefiksie preprocessingu wejscia (int32 ->
// Cast/Split/normalizacja/Transpose), wiec re-rootujemy graf na tensor PO transpozycji
// (NCHW f32, wejscie pierwszego konwolutu) i wyprowadzamy cztery surowe glowy.
const INPUT_TENSOR: &str =
    "StatefulPartitionedCall/center_net_mobile_net_v2fpn_feature_extractor/model_1/model/Conv1/Conv2D__7:0";
const HEAD_CENTER: &str = "StatefulPartitionedCall/center_0/conv2d_4/BiasAdd:0";
const HEAD_HEATMAP: &str = "StatefulPartitionedCall/kpt_heatmap_0/conv2d_5/BiasAdd:0";
const HEAD_REGRESS: &str = "StatefulPartitionedCall/kpt_regress_0/conv2d_6/BiasAdd:0";
const HEAD_OFFSET: &str = "StatefulPartitionedCall/kpt_offset_0/conv2d_7/BiasAdd:0";

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

        let resized = resize_rgb_image(&img, INPUT_SIZE, INPUT_SIZE)
            .map_err(|e| anyhow!("MoveNet: resize failed: {e}"))?;

        // Graf jest re-rootowany za prefiksem preprocessingu (patrz `load`), wiec
        // normalizacje (px/127.5 - 1) i uklad NCHW robimy tutaj.
        let n = INPUT_SIZE as usize;
        let mut chw = vec![0f32; 3 * n * n];
        for (i, p) in resized.pixels().enumerate() {
            let y = i / n;
            let x = i % n;
            chw[y * n + x] = p[0] as f32 / 127.5 - 1.0;
            chw[n * n + y * n + x] = p[1] as f32 / 127.5 - 1.0;
            chw[2 * n * n + y * n + x] = p[2] as f32 / 127.5 - 1.0;
        }
        let input: Tensor = tract_ndarray::Array4::from_shape_vec((1, 3, n, n), chw)
            .context("MoveNet: nchw shape mismatch")?
            .into();

        let outputs = self
            .model
            .run(tvec!(input.into()))
            .context("MoveNet: tract forward failed")?;
        if outputs.len() < 4 {
            return Err(anyhow!("MoveNet: expected 4 head tensors, got {}", outputs.len()));
        }
        let center = outputs[0].as_slice::<f32>().context("MoveNet: center not f32")?;
        let heatmap = outputs[1].as_slice::<f32>().context("MoveNet: heatmap not f32")?;
        let regress = outputs[2].as_slice::<f32>().context("MoveNet: regress not f32")?;
        let offset = outputs[3].as_slice::<f32>().context("MoveNet: offset not f32")?;

        let keypoints = decode_pose(center, heatmap, regress, offset, width, height);
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
    let mut model = tract_onnx::onnx()
        .model_for_path(model_path)
        .with_context(|| format!("tract: MoveNet ONNX from {}", model_path.display()))?;

    // Re-root: wejscie modelu na tensor PO transpozycji (NCHW f32) — omija prefiks
    // preprocessingu int32, na ktorym optymalizator tracta nie konczy. Wyjscia to
    // cztery surowe glowy (center/heatmap/regress/offset); decode liczymy w `decode_pose`.
    let inlet = model
        .find_outlet_label(INPUT_TENSOR)
        .ok_or_else(|| anyhow!("MoveNet: input tensor {INPUT_TENSOR} not found in graph"))?;
    model.set_input_outlets(&[inlet])?;
    model.set_input_fact(
        0,
        InferenceFact::dt_shape(
            f32::datum_type(),
            tvec!(1, 3, INPUT_SIZE as i32, INPUT_SIZE as i32),
        ),
    )?;
    model.set_output_names(&[HEAD_CENTER, HEAD_HEATMAP, HEAD_REGRESS, HEAD_OFFSET])?;
    // compact() fizycznie usuwa osierocony prefiks preprocessingu — bez tego analiza
    // tracta wciaz przetwarza martwe wezly int32 i nie konczy optymalizacji.
    model.compact()?;
    let model = model.into_optimized()?.into_runnable()?;
    Ok(MovenetEngine {
        model: Arc::new(model),
    })
}

/// MoveNet (CenterNet single-pose) decode z czterech glow modelu, kazda NCHW [1,C,48,48]:
/// 1) center = argmax sigmoid(center)/(1.8 + dystans_od_srodka_siatki),
/// 2) regresja: pozycje 17 punktow = (cy,cx) + regress[*, cy, cx],
/// 3) refine: dla kazdego punktu argmax sigmoid(heatmap)/(1.8 + dystans_do_regresji),
/// 4) wspolrzedne = (argmax + lokalny offset)/48, znormalizowane -> piksele obrazu.
fn decode_pose(
    center: &[f32],
    heatmap: &[f32],
    regress: &[f32],
    offset: &[f32],
    width: u32,
    height: u32,
) -> Vec<PoseKeypoint> {
    let g = GRID;
    let at = |buf: &[f32], c: usize, y: usize, x: usize| buf[c * g * g + y * g + x];
    let sig = |v: f32| 1.0 / (1.0 + (-v).exp());
    let cc = (g / 2) as f32;

    let mut best = f32::MIN;
    let (mut cy, mut cx) = (0usize, 0usize);
    for y in 0..g {
        for x in 0..g {
            let dist = ((y as f32 - cc).powi(2) + (x as f32 - cc).powi(2)).sqrt() + DIST_OFFSET;
            let s = sig(at(center, 0, y, x)) / dist;
            if s > best {
                best = s;
                cy = y;
                cx = x;
            }
        }
    }

    let mut keypoints = Vec::with_capacity(17);
    for k in 0..17 {
        let ry = cy as f32 + at(regress, 2 * k, cy, cx);
        let rx = cx as f32 + at(regress, 2 * k + 1, cy, cx);

        let mut best = f32::MIN;
        let (mut ky, mut kx) = (0usize, 0usize);
        for y in 0..g {
            for x in 0..g {
                let dist = ((y as f32 - ry).powi(2) + (x as f32 - rx).powi(2)).sqrt() + DIST_OFFSET;
                let s = sig(at(heatmap, k, y, x)) / dist;
                if s > best {
                    best = s;
                    ky = y;
                    kx = x;
                }
            }
        }

        let score = sig(at(heatmap, k, ky, kx));
        if score < KEYPOINT_THRESHOLD {
            continue;
        }
        let yy = (ky as f32 + at(offset, 2 * k, ky, kx)) / g as f32;
        let xx = (kx as f32 + at(offset, 2 * k + 1, ky, kx)) / g as f32;
        keypoints.push(PoseKeypoint {
            id: k as u8,
            name: COCO_KEYPOINT_NAMES[k],
            x: xx * width as f32,
            y: yy * height as f32,
            score,
        });
    }
    keypoints
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
