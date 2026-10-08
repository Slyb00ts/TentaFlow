// =============================================================================
// File: vision/hands.rs — hand keypoints: MediaPipe palm detector + hand landmarks
// =============================================================================
//
// Two MediaPipe models from the OpenCV zoo (Apache-2.0, pinned by sha256 in the
// `gesture-cv` bundle), run like MoveNet: on ONNX Runtime (CUDA) under
// `vision-ort`, otherwise on tract (CPU):
//
//   * palm detector (192×192 NHWC, /255): anchor-based boxes + 7 palm keypoints;
//   * hand landmarker (224×224 NHWC, /255) on a crop rotated so the fingers point
//     up: 21 keypoints in crop pixels plus a presence score.
//
// The caller decides WHERE to look: a robot camera sees a person meters away,
// where a hand is a few dozen pixels of a 1280-wide frame — the palm detector
// on the whole frame (shrunk to 192) cannot find it. So detection runs on a
// square region around a wrist the pose model found, and every coordinate is
// mapped back to frame pixels here.

use std::path::Path;
#[cfg(not(feature = "vision-ort"))]
use std::sync::Arc;

use anyhow::{anyhow, Result};
#[cfg(not(feature = "vision-ort"))]
use anyhow::Context;
#[cfg(not(feature = "vision-ort"))]
use tract_onnx::prelude::*;

const PALM_INPUT: usize = 192;
const LANDMARK_INPUT: usize = 224;
/// Palm score (after sigmoid) a detection must reach; the upstream demo's value.
const PALM_SCORE: f32 = 0.5;
const PALM_NMS_IOU: f32 = 0.3;
/// Hand presence the landmarker must report for a hand the palm detector just
/// found; the upstream demo's value.
const HAND_PRESENCE: f32 = 0.8;
/// …and for a hand followed from the previous frame — MediaPipe's default
/// `min_tracking_confidence`. A swinging hand is motion-blurred; holding it to
/// the detection bar drops it mid-wave.
const TRACKING_PRESENCE: f32 = 0.5;
/// Upper bound of palms kept per region: a heart is made with two hands.
const MAX_PALMS: usize = 2;

/// Backend handle of one model: an ort session pool (CUDA → CPU) or a tract plan.
#[cfg(feature = "vision-ort")]
type Net = crate::vision::ort_common::SessionPool;
#[cfg(not(feature = "vision-ort"))]
type Net = Arc<RunnableModel<TypedFact, Box<dyn TypedOp>>>;

/// One model output: shape and values, in the graph's output order.
type Output = (Vec<usize>, Vec<f32>);

/// A 2-D point in frame pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
}

impl Pt {
    fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// One hand: 21 MediaPipe landmarks (0 wrist, 4 thumb tip, 8 index tip, 12 middle
/// tip, 16 ring tip, 20 pinky tip; MCP joints 5/9/13/17) in frame pixels.
#[derive(Debug, Clone)]
pub struct Hand {
    pub landmarks: [Pt; 21],
    pub presence: f32,
    /// Wrist movement since the previous frame (zero for a newly found hand):
    /// where to expect the hand on the next frame.
    pub motion: Pt,
}

/// A detected palm in frame pixels: box center, box size and the 7 palm keypoints
/// (0 = wrist / palm base, 2 = middle-finger base).
#[derive(Debug, Clone)]
struct Palm {
    score: f32,
    cx: f32,
    cy: f32,
    w: f32,
    h: f32,
    keypoints: [Pt; 7],
}

/// A square image region, axis-aligned, in frame pixels.
#[derive(Debug, Clone, Copy)]
pub struct Region {
    pub x: f32,
    pub y: f32,
    pub size: f32,
}

/// Rotated square crop: `center`, side `size` (frame px) and the crop's unit axes
/// in frame coordinates (`u` = crop right, `v` = crop down).
#[derive(Debug, Clone, Copy)]
struct Roi {
    center: Pt,
    size: f32,
    u: Pt,
    v: Pt,
}

impl Roi {
    /// Frame point of the crop pixel `(px, py)` on an `n`-pixel crop grid.
    fn to_frame(&self, px: f32, py: f32, n: usize) -> Pt {
        let half = n as f32 / 2.0;
        let s = self.size / n as f32;
        let a = (px - half) * s;
        let b = (py - half) * s;
        Pt::new(
            self.center.x + a * self.u.x + b * self.v.x,
            self.center.y + a * self.u.y + b * self.v.y,
        )
    }
}

pub struct HandTracker {
    palm: Net,
    landmark: Net,
    anchors: Vec<Pt>,
}

impl HandTracker {
    pub fn load(palm_path: &Path, landmark_path: &Path) -> Result<Self> {
        Ok(Self {
            palm: load_nhwc(palm_path, PALM_INPUT)?,
            landmark: load_nhwc(landmark_path, LANDMARK_INPUT)?,
            anchors: palm_anchors(),
        })
    }

    /// Hands inside `region` of an RGB24 frame (`w`×`h`). At most two.
    pub fn hands_in(&self, rgb: &[u8], w: u32, h: u32, region: Region) -> Result<Vec<Hand>> {
        let palms = self.palms_in(rgb, w, h, region)?;
        let mut hands = Vec::with_capacity(palms.len());
        for palm in palms {
            if let Some(hand) = self.landmarks_in(rgb, w, h, &hand_roi(&palm), HAND_PRESENCE)? {
                hands.push(hand);
            }
        }
        Ok(hands)
    }

    /// Re-locates a hand seen on the previous frame from its landmarks alone,
    /// without the palm detector — the MediaPipe tracking step. The crop is moved
    /// by the hand's last movement, so a fast swing stays inside it. `None` when
    /// the landmarker no longer sees a hand there (it moved too far, or left).
    pub fn track(&self, rgb: &[u8], w: u32, h: u32, previous: &Hand) -> Result<Option<Hand>> {
        let mut roi = tracking_roi(previous);
        roi.center = Pt::new(roi.center.x + previous.motion.x, roi.center.y + previous.motion.y);
        let found = self.landmarks_in(rgb, w, h, &roi, TRACKING_PRESENCE)?;
        Ok(found.map(|mut hand| {
            let (a, b) = (previous.landmarks[0], hand.landmarks[0]);
            hand.motion = Pt::new(b.x - a.x, b.y - a.y);
            hand
        }))
    }

    fn landmarks_in(
        &self,
        rgb: &[u8],
        w: u32,
        h: u32,
        roi: &Roi,
        min_presence: f32,
    ) -> Result<Option<Hand>> {
        let input = sample_crop(rgb, w, h, roi, LANDMARK_INPUT);
        let out = forward(&self.landmark, input, LANDMARK_INPUT, "hand landmarks")?;
        let (lm, presence) = landmark_outputs(&out)?;
        if presence < min_presence {
            return Ok(None);
        }
        let mut landmarks = [Pt::new(0.0, 0.0); 21];
        for (i, p) in landmarks.iter_mut().enumerate() {
            *p = roi.to_frame(lm[i * 3], lm[i * 3 + 1], LANDMARK_INPUT);
        }
        Ok(Some(Hand {
            landmarks,
            presence,
            motion: Pt::new(0.0, 0.0),
        }))
    }

    fn palms_in(&self, rgb: &[u8], w: u32, h: u32, region: Region) -> Result<Vec<Palm>> {
        // The region is square, so the 192 input needs no letterbox.
        let roi = Roi {
            center: Pt::new(region.x + region.size / 2.0, region.y + region.size / 2.0),
            size: region.size,
            u: Pt::new(1.0, 0.0),
            v: Pt::new(0.0, 1.0),
        };
        let input = sample_crop(rgb, w, h, &roi, PALM_INPUT);
        let out = forward(&self.palm, input, PALM_INPUT, "palm detector")?;
        let (boxes, scores) = palm_outputs(&out)?;
        let scale = region.size / PALM_INPUT as f32;
        let mut palms = Vec::new();
        for (i, anchor) in self.anchors.iter().enumerate() {
            let score = sigmoid(scores[i]);
            if score < PALM_SCORE {
                continue;
            }
            let r = &boxes[i * 18..i * 18 + 18];
            // Raw values are 192-px offsets from the anchor center.
            let to_px = |dx: f32, dy: f32| {
                Pt::new(
                    region.x + (anchor.x * PALM_INPUT as f32 + dx) * scale,
                    region.y + (anchor.y * PALM_INPUT as f32 + dy) * scale,
                )
            };
            let c = to_px(r[0], r[1]);
            let mut keypoints = [Pt::new(0.0, 0.0); 7];
            for (k, p) in keypoints.iter_mut().enumerate() {
                *p = to_px(r[4 + k * 2], r[5 + k * 2]);
            }
            palms.push(Palm {
                score,
                cx: c.x,
                cy: c.y,
                w: r[2] * scale,
                h: r[3] * scale,
                keypoints,
            });
        }
        Ok(nms(palms))
    }
}

#[cfg(feature = "vision-ort")]
fn load_nhwc(path: &Path, _n: usize) -> Result<Net> {
    if !path.exists() {
        return Err(anyhow!("hand model missing: {}", path.display()));
    }
    crate::vision::ort_common::ensure_ort_dylib();
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("hand");
    crate::vision::ort_common::build_session_pool_from_file(
        path,
        &path.with_file_name(format!("trt-cache-{stem}")),
        None,
        // Two hands are tracked (or two wrist regions searched) at once.
        2,
        false,
        // Tiny static graphs: an engine build would cost more than it saves.
        crate::vision::ort_common::EpChain::CudaOnly,
    )
    .map_err(|e| anyhow!("ort session for {}: {e:#}", path.display()))
}

#[cfg(not(feature = "vision-ort"))]
fn load_nhwc(path: &Path, n: usize) -> Result<Net> {
    if !path.exists() {
        return Err(anyhow!("hand model missing: {}", path.display()));
    }
    tract_onnx::onnx()
        .model_for_path(path)
        .with_context(|| format!("tract: load {}", path.display()))?
        .with_input_fact(0, f32::fact([1, n, n, 3]).into())?
        .into_optimized()?
        .into_runnable()
        .with_context(|| format!("tract: optimize {}", path.display()))
}

/// Runs one `[1, n, n, 3]` NHWC f32 input and returns every output.
#[cfg(feature = "vision-ort")]
fn forward(net: &Net, input: Vec<f32>, n: usize, what: &'static str) -> Result<Vec<Output>> {
    net.run(move |session| {
        let name = session
            .inputs()
            .first()
            .map(|i| i.name().to_string())
            .ok_or_else(|| anyhow!("{what}: model has no inputs"))?;
        let names: Vec<String> = session.outputs().iter().map(|o| o.name().to_string()).collect();
        let value = ort::value::Tensor::from_array(([1usize, n, n, 3], input))
            .map_err(|e| anyhow!("{what}: input tensor: {e}"))?;
        let outputs = session
            .run(ort::inputs! { name => value })
            .map_err(|e| anyhow!("{what}: session.run: {e}"))?;
        names
            .iter()
            .map(|name| {
                let (shape, data) = outputs[name.as_str()]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| anyhow!("{what}: extract {name}: {e}"))?;
                Ok((shape.iter().map(|&d| d.max(0) as usize).collect(), data.to_vec()))
            })
            .collect()
    })
}

#[cfg(not(feature = "vision-ort"))]
fn forward(net: &Net, input: Vec<f32>, n: usize, what: &'static str) -> Result<Vec<Output>> {
    let tensor: Tensor = tract_ndarray::Array4::from_shape_vec((1, n, n, 3), input)
        .expect("n*n*3 buffer matches its shape")
        .into();
    let out = net
        .run(tvec!(tensor.into()))
        .with_context(|| format!("{what}: tract forward failed"))?;
    out.iter()
        .map(|t| Ok((t.shape().to_vec(), t.view().as_slice::<f32>()?.to_vec())))
        .collect()
}

/// The detector's 2016 anchor centers (normalized 0..1): stride 8 → 24×24 cells
/// with 2 anchors, stride 16 → 12×12 cells with 6, row-major, anchors per cell
/// consecutive. Identical to the table the upstream demo hard-codes.
fn palm_anchors() -> Vec<Pt> {
    let mut a = Vec::with_capacity(2016);
    for (grid, per_cell) in [(24usize, 2usize), (12, 6)] {
        for y in 0..grid {
            for x in 0..grid {
                let c = Pt::new((x as f32 + 0.5) / grid as f32, (y as f32 + 0.5) / grid as f32);
                for _ in 0..per_cell {
                    a.push(c);
                }
            }
        }
    }
    a
}

/// Regressors `[2016×18]` and score logits `[2016]`, told apart by shape (output
/// names differ between ONNX converters).
fn palm_outputs(out: &[Output]) -> Result<(Vec<f32>, Vec<f32>)> {
    let mut boxes = None;
    let mut scores = None;
    for (shape, data) in out {
        match shape.last() {
            Some(&18) => boxes = Some(data.clone()),
            Some(&1) => scores = Some(data.clone()),
            _ => {}
        }
    }
    match (boxes, scores) {
        (Some(b), Some(s)) if b.len() == 2016 * 18 && s.len() == 2016 => Ok((b, s)),
        _ => Err(anyhow!("palm detector: unexpected outputs")),
    }
}

/// Screen landmarks `[63]` and the presence score — the first `[1,1]` output in
/// graph order (the second one is handedness).
fn landmark_outputs(out: &[Output]) -> Result<(&[f32], f32)> {
    let lm = out
        .iter()
        .find(|(_, d)| d.len() == 63)
        .ok_or_else(|| anyhow!("hand landmarks: no 63-value output"))?;
    let presence = out
        .iter()
        .find(|(_, d)| d.len() == 1)
        .ok_or_else(|| anyhow!("hand landmarks: no presence output"))?;
    Ok((&lm.1, presence.1[0]))
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

fn iou(a: &Palm, b: &Palm) -> f32 {
    let (ax1, ay1, ax2, ay2) = (a.cx - a.w / 2.0, a.cy - a.h / 2.0, a.cx + a.w / 2.0, a.cy + a.h / 2.0);
    let (bx1, by1, bx2, by2) = (b.cx - b.w / 2.0, b.cy - b.h / 2.0, b.cx + b.w / 2.0, b.cy + b.h / 2.0);
    let iw = (ax2.min(bx2) - ax1.max(bx1)).max(0.0);
    let ih = (ay2.min(by2) - ay1.max(by1)).max(0.0);
    let inter = iw * ih;
    let union = a.w * a.h + b.w * b.h - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

fn nms(mut palms: Vec<Palm>) -> Vec<Palm> {
    palms.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Palm> = Vec::new();
    for p in palms {
        if kept.len() == MAX_PALMS {
            break;
        }
        if kept.iter().all(|k| iou(k, &p) < PALM_NMS_IOU) {
            kept.push(p);
        }
    }
    kept
}

/// The landmarker's input region for a palm, as the upstream pipeline builds it:
/// rotate so wrist → middle-finger base points up, take the box of the palm
/// keypoints in that frame, shift it 0.4 of its height toward the fingers and
/// enlarge it 3× into a square.
fn hand_roi(palm: &Palm) -> Roi {
    let wrist = palm.keypoints[0];
    let middle = palm.keypoints[2];
    let (dx, dy) = (middle.x - wrist.x, middle.y - wrist.y);
    let len = (dx * dx + dy * dy).sqrt().max(1e-3);
    // Crop "down" is from the fingers toward the wrist.
    let v = Pt::new(-dx / len, -dy / len);
    let u = Pt::new(v.y, -v.x);
    let center = Pt::new(palm.cx, palm.cy);
    let (mut umin, mut umax, mut vmin, mut vmax) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for k in &palm.keypoints {
        let (rx, ry) = (k.x - center.x, k.y - center.y);
        let a = rx * u.x + ry * u.y;
        let b = rx * v.x + ry * v.y;
        umin = umin.min(a);
        umax = umax.max(a);
        vmin = vmin.min(b);
        vmax = vmax.max(b);
    }
    let bw = umax - umin;
    let bh = vmax - vmin;
    let mid_a = (umin + umax) / 2.0;
    let mid_b = (vmin + vmax) / 2.0 - 0.4 * bh;
    Roi {
        center: Pt::new(
            center.x + mid_a * u.x + mid_b * v.x,
            center.y + mid_a * u.y + mid_b * v.y,
        ),
        size: bw.max(bh) * 3.0,
        u,
        v,
    }
}

/// Landmarks MediaPipe frames a tracked hand by: wrist, thumb base joints and the
/// lower two joints of every finger — fingertips are left out, they move too much.
const TRACKING_LANDMARKS: [usize; 12] = [0, 1, 2, 3, 5, 6, 9, 10, 13, 14, 17, 18];

/// The landmarker's input region for the next frame, from this frame's hand
/// (MediaPipe's landmarks-to-ROI step): rotate so the wrist points at the knuckle
/// line, box the stable landmarks, shift 0.1 of the box toward the fingers and
/// enlarge 2× into a square — enough margin for a hand moving between frames.
fn tracking_roi(hand: &Hand) -> Roi {
    let lm = &hand.landmarks;
    let wrist = lm[0];
    let knuckles = Pt::new(
        ((lm[5].x + lm[13].x) / 2.0 + lm[9].x) / 2.0,
        ((lm[5].y + lm[13].y) / 2.0 + lm[9].y) / 2.0,
    );
    let (dx, dy) = (wrist.x - knuckles.x, wrist.y - knuckles.y);
    let len = (dx * dx + dy * dy).sqrt().max(1e-3);
    let v = Pt::new(dx / len, dy / len);
    let u = Pt::new(v.y, -v.x);
    let (mut umin, mut umax, mut vmin, mut vmax) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for &i in &TRACKING_LANDMARKS {
        let a = lm[i].x * u.x + lm[i].y * u.y;
        let b = lm[i].x * v.x + lm[i].y * v.y;
        umin = umin.min(a);
        umax = umax.max(a);
        vmin = vmin.min(b);
        vmax = vmax.max(b);
    }
    let bh = vmax - vmin;
    let mid_a = (umin + umax) / 2.0;
    let mid_b = (vmin + vmax) / 2.0 - 0.1 * bh;
    Roi {
        center: Pt::new(mid_a * u.x + mid_b * v.x, mid_a * u.y + mid_b * v.y),
        size: (umax - umin).max(bh) * 2.0,
        u,
        v,
    }
}

/// Bilinear sample of the rotated square `roi` into an `n`×`n` NHWC tensor scaled
/// to 0..1. Pixels outside the frame are black, like the upstream padding.
fn sample_crop(rgb: &[u8], w: u32, h: u32, roi: &Roi, n: usize) -> Vec<f32> {
    let (wi, hi) = (w as i64, h as i64);
    let px = |x: i64, y: i64, c: usize| -> f32 {
        if x < 0 || y < 0 || x >= wi || y >= hi {
            0.0
        } else {
            rgb[((y * wi + x) * 3) as usize + c] as f32
        }
    };
    let mut data = vec![0f32; n * n * 3];
    for oy in 0..n {
        for ox in 0..n {
            let p = roi.to_frame(ox as f32 + 0.5, oy as f32 + 0.5, n);
            let (fx, fy) = (p.x - 0.5, p.y - 0.5);
            let (x0, y0) = (fx.floor() as i64, fy.floor() as i64);
            let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
            for c in 0..3 {
                let top = px(x0, y0, c) * (1.0 - tx) + px(x0 + 1, y0, c) * tx;
                let bot = px(x0, y0 + 1, c) * (1.0 - tx) + px(x0 + 1, y0 + 1, c) * tx;
                data[(oy * n + ox) * 3 + c] = (top * (1.0 - ty) + bot * ty) / 255.0;
            }
        }
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end on the real models: `TENTAFLOW_GESTURE_MODELS` is a directory
    /// with the two MediaPipe ONNX files, `TENTAFLOW_HANDS_TEST_IMAGE` a photo of
    /// two raised hands (MediaPipe's `woman_hands.jpg`).
    #[test]
    #[ignore = "needs the gesture-cv models and TENTAFLOW_HANDS_TEST_IMAGE"]
    fn finds_both_hands_on_a_real_photo() {
        use crate::vision::camera_cv_models::{HAND_LANDMARK_FILE, PALM_DETECTION_FILE};
        let dir = std::path::PathBuf::from(std::env::var("TENTAFLOW_GESTURE_MODELS").unwrap());
        let img = image::open(std::env::var("TENTAFLOW_HANDS_TEST_IMAGE").unwrap())
            .unwrap()
            .to_rgb8();
        let (w, h) = img.dimensions();
        let tracker =
            HandTracker::load(&dir.join(PALM_DETECTION_FILE), &dir.join(HAND_LANDMARK_FILE)).unwrap();
        let side = w.max(h) as f32;
        let region = Region {
            x: (w as f32 - side) / 2.0,
            y: (h as f32 - side) / 2.0,
            size: side,
        };
        let hands = tracker.hands_in(img.as_raw(), w, h, region).unwrap();
        assert_eq!(hands.len(), 2, "both hands found");
        for hand in &hands {
            assert!(hand.presence > HAND_PRESENCE);
            for p in &hand.landmarks {
                assert!(p.x > 0.0 && p.x < w as f32 && p.y > 0.0 && p.y < h as f32, "{p:?}");
            }
        }
        // Reference points of the same photo from the upstream OpenCV pipeline
        // (mp_palmdet.py + mp_handpose.py): (wrist, index tip, middle tip).
        let reference = [
            ((294.0, 380.5), (465.5, 404.2), (468.5, 373.4)),
            ((262.9, 680.5), (41.2, 744.3), (12.4, 706.9)),
        ];
        for (wrist, index, middle) in reference {
            let hand = hands
                .iter()
                .min_by(|a, b| {
                    let d = |h: &Hand| (h.landmarks[0].x - wrist.0).hypot(h.landmarks[0].y - wrist.1);
                    d(a).total_cmp(&d(b))
                })
                .unwrap();
            for (i, (x, y)) in [(0, wrist), (8, index), (12, middle)] {
                let p = hand.landmarks[i];
                let err = (p.x - x).hypot(p.y - y);
                assert!(err < 12.0, "landmark {i}: {p:?} vs reference ({x}, {y}), error {err:.1} px");
            }
        }
        // Tracking from this frame's landmarks must find the same hands again.
        for hand in &hands {
            let tracked = tracker
                .track(img.as_raw(), w, h, hand)
                .unwrap()
                .expect("hand re-found from its own landmarks");
            for i in [0, 8, 12] {
                let (a, b) = (hand.landmarks[i], tracked.landmarks[i]);
                assert!((a.x - b.x).hypot(a.y - b.y) < 12.0, "landmark {i}: {a:?} vs tracked {b:?}");
            }
        }
    }

    #[test]
    fn anchor_table_matches_the_detector_layout() {
        let a = palm_anchors();
        assert_eq!(a.len(), 2016);
        // First stride-8 cell, two anchors.
        assert_eq!(a[0], Pt::new(0.5 / 24.0, 0.5 / 24.0));
        assert_eq!(a[1], a[0]);
        assert_eq!(a[2], Pt::new(1.5 / 24.0, 0.5 / 24.0));
        // First stride-16 cell starts after 24*24*2 anchors, six of them.
        assert_eq!(a[1152], Pt::new(0.5 / 12.0, 0.5 / 12.0));
        assert_eq!(a[1157], a[1152]);
        assert_eq!(a[2015], Pt::new(11.5 / 12.0, 11.5 / 12.0));
    }

    fn upright_palm() -> Palm {
        // Wrist below, middle-finger base above: fingers point up (−y).
        let mut keypoints = [Pt::new(100.0, 100.0); 7];
        keypoints[0] = Pt::new(100.0, 120.0);
        keypoints[2] = Pt::new(100.0, 80.0);
        keypoints[1] = Pt::new(90.0, 90.0);
        keypoints[3] = Pt::new(110.0, 90.0);
        Palm {
            score: 0.9,
            cx: 100.0,
            cy: 100.0,
            w: 40.0,
            h: 40.0,
            keypoints,
        }
    }

    #[test]
    fn upright_hand_roi_is_axis_aligned_and_shifted_toward_the_fingers() {
        let roi = hand_roi(&upright_palm());
        assert!((roi.u.x - 1.0).abs() < 1e-5 && roi.u.y.abs() < 1e-5, "u = {:?}", roi.u);
        assert!(roi.v.x.abs() < 1e-5 && (roi.v.y - 1.0).abs() < 1e-5, "v = {:?}", roi.v);
        // Palm keypoint box is 20 wide, 40 tall → square side 120, center moved up 16.
        assert!((roi.size - 120.0).abs() < 1e-3, "size = {}", roi.size);
        assert!((roi.center.y - 84.0).abs() < 1e-3, "center = {:?}", roi.center);
    }

    #[test]
    fn rotated_roi_maps_the_crop_top_toward_the_fingers() {
        // Fingers point right (+x): the crop's top edge must lie on the finger side.
        let mut palm = upright_palm();
        palm.keypoints[0] = Pt::new(80.0, 100.0);
        palm.keypoints[2] = Pt::new(120.0, 100.0);
        let roi = hand_roi(&palm);
        let top = roi.to_frame(112.0, 0.0, 224);
        let bottom = roi.to_frame(112.0, 224.0, 224);
        assert!(top.x > bottom.x, "top {top:?} must be right of bottom {bottom:?}");
    }

    #[test]
    fn tracking_roi_follows_an_upright_hand_and_leaves_room_to_move() {
        // Wrist at the bottom, knuckles 40 px above, fingers further up.
        let mut landmarks = [Pt::new(100.0, 100.0); 21];
        landmarks[0] = Pt::new(100.0, 140.0);
        for (i, x) in [(1, 85.0), (5, 90.0), (9, 100.0), (13, 110.0), (17, 118.0)] {
            landmarks[i] = Pt::new(x, 100.0);
        }
        for (i, x) in [(2, 82.0), (3, 80.0), (6, 90.0), (10, 100.0), (14, 110.0), (18, 118.0)] {
            landmarks[i] = Pt::new(x, 85.0);
        }
        let roi = tracking_roi(&Hand {
            landmarks,
            presence: 1.0,
            motion: Pt::new(0.0, 0.0),
        });
        assert!((roi.u.x - 1.0).abs() < 1e-5 && roi.u.y.abs() < 1e-5, "u = {:?}", roi.u);
        assert!(roi.v.x.abs() < 1e-5 && (roi.v.y - 1.0).abs() < 1e-5, "v = {:?}", roi.v);
        // Stable box: x 80..118, y 85..140 → side 2·55, center shifted 5.5 up from 112.5.
        assert!((roi.size - 110.0).abs() < 1e-3, "size = {}", roi.size);
        assert!((roi.center.x - 99.0).abs() < 1e-3, "center = {:?}", roi.center);
        assert!((roi.center.y - 107.0).abs() < 1e-3, "center = {:?}", roi.center);
    }

    #[test]
    fn nms_keeps_the_best_of_overlapping_palms_and_caps_at_two() {
        let p = |score: f32, cx: f32| Palm {
            score,
            cx,
            cy: 50.0,
            w: 20.0,
            h: 20.0,
            keypoints: [Pt::new(0.0, 0.0); 7],
        };
        let kept = nms(vec![p(0.6, 50.0), p(0.9, 52.0), p(0.8, 150.0), p(0.7, 250.0)]);
        assert_eq!(kept.len(), 2);
        assert!((kept[0].score - 0.9).abs() < 1e-6);
        assert!((kept[1].score - 0.8).abs() < 1e-6);
    }
}
