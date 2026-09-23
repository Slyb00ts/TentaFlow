// =============================================================================
// File: vision/detector_coco.rs — YOLOX-tiny COCO detector: vehicles + persons (ort+CUDA)
// =============================================================================
//
// One YOLOX-tiny COCO graph and ONE ort session pool serve two consumers:
//   * vehicles — a SECOND, parallel detector next to RF-DETR (ADR/plate/sticker)
//     on the SAME frame, so each placard / plate / sticker can be associated to
//     the vehicle it sits on (the per-truck separation). Its own pool (independent
//     CUDA streams) makes a `tokio::join!` of both forwards cost ~max(DETR, YOLOX);
//   * persons — the camera privacy probe (`detect_device_input`), fed a
//     preprocessed tensor that already lives in CUDA memory.
//
// Model: the official Megvii YOLOX-tiny release (Apache-2.0), file
// `yolox_tiny.onnx`. Input `images [1,3,416,416]` f32 in B,G,R order with RAW
// 0..255 values (no /255, no mean/std), letterboxed: aspect-preserving resize
// into the TOP-LEFT corner, pad value 114. Output `output [1,3549,85]`, per
// anchor `[dx, dy, dw, dh, obj, 80 class scores]` NOT decoded into boxes: the
// anchors are the grids of strides 8/16/32 (52² + 26² + 13², row-major gy then
// gx) and `cx = (dx + gx)·stride`, `w = exp(dw)·stride` in input pixels. `obj`
// and the class scores are already sigmoided; confidence = obj · class. The
// graph has a static batch of 1, so every batch entry point runs one forward
// per frame on the same pool session.
//
// The decode keeps a caller-chosen class set (`VEHICLE_CLASSES` = {2 car,
// 5 bus, 7 truck} → "vehicle", `PERSON_CLASSES` = {0} → "person"), runs NMS per
// output label and maps the box back to 0..1 of the frame by dividing by the
// letterbox content size — the exact `Detection` shape the rest of the
// pipeline consumes.

#![cfg(all(feature = "inference-vision-gpu", feature = "vision-ort"))]

use anyhow::{anyhow, bail, Result};
use tracing::info;

use crate::paths;
use crate::services::detection_bus::Detection;
use crate::vision::nms::nms;
use crate::vision::preprocessing::{letterbox_content, ChannelOrder, FrameFit};
use crate::vision::FaceDetection;

/// Square input side the exported YOLOX-tiny graph expects.
pub const RESOLUTION: u32 = 416;

/// How a frame is fitted into the input (the training-time preprocessing).
pub const INPUT_FIT: FrameFit = FrameFit::Letterbox { pad: 114 };

/// Channel order of the input (OpenCV-trained).
pub const INPUT_ORDER: ChannelOrder = ChannelOrder::Bgr;

/// Mean/std pair for the fused GPU kernel, which computes `(v/255 - mean)/std`:
/// std = 1/255 turns that back into the raw 0..255 value the model reads.
pub const INPUT_MEAN: [f32; 3] = [0.0, 0.0, 0.0];
pub const INPUT_STD: [f32; 3] = [1.0 / 255.0, 1.0 / 255.0, 1.0 / 255.0];

/// Input tensor name in the YOLOX ONNX graph.
const INPUT_NAME: &str = "images";

/// Output tensor name in the YOLOX ONNX graph.
const OUTPUT_NAME: &str = "output";

/// Detection-head strides; the anchor list is their grids concatenated.
const STRIDES: [u32; 3] = [8, 16, 32];

/// Number of COCO classes in the head.
const NUM_CLASSES: usize = 80;

/// Values per anchor: 4 box offsets + objectness + 80 class scores.
const ATTRS: usize = 5 + NUM_CLASSES;

/// Anchors for a 416 input: 52² + 26² + 13².
const ANCHORS: usize = 3549;

/// Confidence floor (obj · class) for a kept vehicle box. Vehicles are large,
/// high-contrast objects; 0.35 keeps distant trucks while dropping noise.
pub const VEHICLE_SCORE_THRESHOLD: f32 = 0.35;

/// Confidence floor for a kept person box. Lower than the vehicle floor on
/// purpose: the privacy probe blurs heads from these boxes, so a missed person
/// leaks a face while a false positive only blurs a patch of background. On the
/// reference photo a half-visible person at the frame edge scores 0.42 and
/// background clutter stays under 0.2, so 0.3 keeps the former with margin.
pub const PERSON_SCORE_THRESHOLD: f32 = 0.3;

/// NMS IoU threshold for overlapping boxes of one output label.
const NMS_IOU_THRESHOLD: f32 = 0.45;

/// The one class name every kept vehicle box carries.
pub const VEHICLE_CLASS: &str = "vehicle";

/// The class name every kept person box carries.
pub const PERSON_CLASS: &str = "person";

/// COCO class ids we treat as a vehicle: 2 = car, 5 = bus, 7 = truck. All map to
/// a single `klasa = "vehicle"` — the association layer only needs the box.
pub const VEHICLE_CLASSES: &[(usize, &str)] =
    &[(2, VEHICLE_CLASS), (5, VEHICLE_CLASS), (7, VEHICLE_CLASS)];

/// COCO class 0 = person.
pub const PERSON_CLASSES: &[(usize, &str)] = &[(0, PERSON_CLASS)];

/// Small session pool — the model is tiny (~20 MB per session), so 2 sessions
/// give parallel forwards at negligible VRAM. The privacy probe shares this
/// pool and needs `min(privacy_cameras, 2)` concurrent forwards, which the
/// default already covers. Configurable via `[vision] vehicle_sessions`.
const DEFAULT_SESSIONS: usize = 2;

thread_local! {
    /// Reusable host input buffer for `detect_batch`: grows to the largest
    /// batch this thread has seen and stays resident so the per-batch alloc +
    /// page-fault churn is paid once. Passed to ort as a BORROWED tensor (raw
    /// pointer, blocking run).
    static HOST_INPUT_SCRATCH: std::cell::RefCell<Vec<f32>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Loaded YOLOX COCO detector: one ort session pool shared by the vehicle and
/// person consumers. `detect*` take `&self` (the pool is internally concurrent),
/// so the launcher can drive it in parallel with RF-DETR and the privacy probe
/// can run on the same pool from its own thread.
pub struct CocoDetector {
    pool: crate::vision::ort_common::SessionPool,
}

impl CocoDetector {
    /// Builds the detector from `vision_models_dir()/yolox_tiny.onnx`.
    pub fn load() -> Result<Self> {
        let dir = paths::vision_models_dir();
        let onnx_path = dir.join(crate::vision::camera_cv_models::YOLOX_TINY_FILE);
        if !onnx_path.exists() {
            bail!("YOLOX ONNX missing: {}", onnx_path.display());
        }
        crate::vision::ort_common::ensure_ort_dylib();
        let n = crate::vision::ort_common::pool_size(configured_sessions());
        let pool = crate::vision::ort_common::build_session_pool_from_file(
            &onnx_path,
            &dir.join("trt-cache-yolox"),
            // Static [1,3,416,416] graph: no dynamic-batch profile to pin.
            None,
            n,
            // FP16 ok — vehicle and person boxes are coarse and robust to it
            // (head regions for the privacy blur carry a generous margin).
            true,
            crate::vision::ort_common::EpChain::CudaOnly,
        )
        .map_err(|e| anyhow!("building YOLOX ort session pool of {n} session(s): {e:#}"))?;
        info!(
            "[coco] loaded {} (backend ort CUDA→CPU, pool={} session(s))",
            onnx_path.display(),
            pool.len()
        );
        Ok(Self { pool })
    }

    /// Number of pooled ort sessions.
    pub fn pool_size(&self) -> usize {
        self.pool.len()
    }

    /// Runs N RGB24 frames through host-tensor forwards (one per frame, the
    /// graph has a static batch of 1) and returns their vehicle boxes. The host
    /// preprocess is the YOLOX one: letterbox into 416, BGR, raw 0..255, NCHW.
    /// Output order == input order, length == `frames.len()`.
    pub fn detect_batch(&self, frames: &[(&[u8], u32, u32)]) -> Result<Vec<Vec<Detection>>> {
        if frames.is_empty() {
            return Ok(Vec::new());
        }
        let n = frames.len();
        HOST_INPUT_SCRATCH.with(|cell| {
            let mut data = cell.borrow_mut();
            let need = n * frame_elements();
            if data.len() < need {
                data.resize(need, 0.0);
            }
            let mut contents = Vec::with_capacity(n);
            for (bi, &(rgb, w, h)) in frames.iter().enumerate() {
                contents.push(fill_frame_yolox(&mut data[..need], bi, rgb, w, h)?);
            }
            let outputs = self.forward(data.as_mut_ptr() as usize, InputMemory::Host, n)?;
            Ok(decode_all(
                &outputs,
                &contents,
                VEHICLE_CLASSES,
                VEHICLE_SCORE_THRESHOLD,
            ))
        })
    }

    /// GPU-resident detect from a batch of NV12 frames: the fused kernel
    /// letterboxes each frame into 416 as B,G,R raw 0..255 in a device buffer
    /// and ort reads it in place (zero host↔device copy of the model input).
    /// Returns the vehicle boxes per frame.
    #[cfg(all(
        any(target_os = "linux", target_os = "windows"),
        feature = "vision-cuda-preprocess"
    ))]
    pub fn detect_batch_gpu(
        &self,
        frames: &[crate::vision::gpu_preprocess::Nv12Frame<'_>],
        color: crate::vision::gpu_preprocess::ColorCoeffs,
    ) -> Result<Vec<Vec<Detection>>> {
        if frames.is_empty() {
            return Ok(Vec::new());
        }
        let n = frames.len();
        let batch = crate::vision::gpu_preprocess::preprocess_nv12_batch_gpu(
            frames,
            RESOLUTION as usize,
            INPUT_MEAN,
            INPUT_STD,
            INPUT_FIT,
            INPUT_ORDER,
            color,
        )?;
        let contents: Vec<(u32, u32)> = frames
            .iter()
            .map(|f| letterbox_content(f.w, f.h, RESOLUTION))
            .collect();
        // `batch` borrows this thread's preprocess scratch; the blocking forward
        // below finishes before any other preprocess can run on this thread.
        let outputs = self.forward(batch.device_ptr() as usize, InputMemory::Cuda, n);
        drop(batch);
        Ok(decode_all(
            &outputs?,
            &contents,
            VEHICLE_CLASSES,
            VEHICLE_SCORE_THRESHOLD,
        ))
    }

    /// Single-frame detect on a model input that ALREADY lives in CUDA memory —
    /// the privacy probe's entry point. `input` is a `[1, 3, 416, 416]` f32 NCHW
    /// tensor on CUDA device 0 prepared per [`INPUT_FIT`] / [`INPUT_ORDER`] /
    /// [`INPUT_MEAN`] / [`INPUT_STD`] (e.g. by `preprocess_nv12_device_into`),
    /// owned and reused by the caller. `content` is the `(dw, dh)` size the frame
    /// was resized to inside the input — the value that preprocess returns — and
    /// maps boxes back to the frame. Returns the boxes of `keep` (COCO id →
    /// label) above `score_threshold`, NMS'd per label, normalized to 0..1.
    ///
    /// Blocks the calling thread until the forward finished and its outputs are
    /// on the host; the forward itself runs on the pool's dedicated session
    /// thread (see [`crate::vision::ort_common::SessionPool::run`]).
    ///
    /// # Safety
    /// * `input` must point to at least `3·416·416` initialized f32 in CUDA
    ///   device-0 memory and stay allocated until this call returns.
    /// * Every write to `input` must be COMPLETE before the call: ort reads it on
    ///   the CUDA EP's own stream, which does not wait on the caller's stream, so
    ///   the caller synchronizes its stream (or an event recorded after the
    ///   preprocess) first.
    /// * On return ort no longer reads `input` (the outputs were copied to the
    ///   host on the same EP stream), so the caller may overwrite it immediately.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    pub unsafe fn detect_device_input(
        &self,
        input: *const f32,
        content: (u32, u32),
        keep: &[(usize, &str)],
        score_threshold: f32,
    ) -> Result<Vec<Detection>> {
        let outputs = self.forward(input as usize, InputMemory::Cuda, 1)?;
        Ok(decode_yolox(&outputs[0], content, keep, score_threshold))
    }

    /// Runs `n` consecutive `[1,3,416,416]` f32 inputs starting at `base_ptr`
    /// (in `memory`) through one pool session and returns each raw `[3549, 85]`
    /// output. The caller keeps the buffer alive and fully written for the
    /// whole blocking call.
    fn forward(&self, base_ptr: usize, memory: InputMemory, n: usize) -> Result<Vec<Vec<f32>>> {
        let res = RESOLUTION as i64;
        let frame_bytes = frame_elements() * std::mem::size_of::<f32>();
        self.pool.run(move |session| {
            // Built on the session thread: `MemoryInfo` is not `Send`.
            let info = memory.info()?;
            let mut outputs = Vec::with_capacity(n);
            for i in 0..n {
                let ptr = base_ptr + i * frame_bytes;
                // SAFETY: the caller guarantees `n` initialized frames of
                // 3·416·416 f32 at `base_ptr` in `memory`, alive
                // until this blocking run returns.
                let value = unsafe {
                    ort::value::TensorRefMut::<f32>::from_raw(
                        info.clone(),
                        (ptr as *mut ()).cast(),
                        ort::value::Shape::new([1, 3, res, res]),
                    )
                }
                .map_err(|e| anyhow!("yolox-ort: TensorRefMut::from_raw: {e}"))?;
                let result = session
                    .run(ort::inputs! { INPUT_NAME => value })
                    .map_err(|e| anyhow!("yolox-ort: session.run: {e}"))?;
                let (shape, out) = result[OUTPUT_NAME]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| anyhow!("yolox-ort: extract {OUTPUT_NAME}: {e}"))?;
                validate_output_shape(shape, out.len())?;
                outputs.push(out[..ANCHORS * ATTRS].to_vec());
            }
            Ok(outputs)
        })
    }
}

/// f32 elements of one `[3, 416, 416]` input frame.
fn frame_elements() -> usize {
    3 * (RESOLUTION as usize) * (RESOLUTION as usize)
}

/// Where a forward's input tensor lives.
#[derive(Clone, Copy)]
enum InputMemory {
    Host,
    /// CUDA device 0 (the device nvcodec, the preprocess kernels and ort bind).
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    Cuda,
}

impl InputMemory {
    fn info(self) -> Result<ort::memory::MemoryInfo> {
        match self {
            InputMemory::Host => Ok(ort::memory::MemoryInfo::default()),
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            InputMemory::Cuda => {
                use ort::memory::{AllocationDevice, AllocatorType, MemoryInfo, MemoryType};
                MemoryInfo::new(
                    AllocationDevice::CUDA,
                    0,
                    AllocatorType::Device,
                    MemoryType::Default,
                )
                .map_err(|e| anyhow!("yolox-ort gpu: MemoryInfo::new: {e}"))
            }
        }
    }
}

/// Resolves the pool size from `[vision] vehicle_sessions` (defaults to
/// [`DEFAULT_SESSIONS`] when unset/zero).
fn configured_sessions() -> usize {
    let cfg = crate::vision::settings::get().vehicle_sessions;
    if cfg == 0 {
        DEFAULT_SESSIONS
    } else {
        cfg
    }
}

/// Requires the official export layout `[1, 3549, 85]`. `shape` derefs to
/// `[i64]` (ort `Shape`).
fn validate_output_shape(shape: &[i64], data_len: usize) -> Result<()> {
    if shape != [1, ANCHORS as i64, ATTRS as i64] {
        bail!("yolox-ort: unexpected output shape {shape:?}, want [1, {ANCHORS}, {ATTRS}]");
    }
    if data_len < ANCHORS * ATTRS {
        bail!("yolox-ort: output buffer too short: {data_len}");
    }
    Ok(())
}

/// Decodes one raw output per frame with that frame's letterbox content size.
fn decode_all(
    outputs: &[Vec<f32>],
    contents: &[(u32, u32)],
    keep: &[(usize, &str)],
    score_threshold: f32,
) -> Vec<Vec<Detection>> {
    outputs
        .iter()
        .zip(contents)
        .map(|(out, &content)| decode_yolox(out, content, keep, score_threshold))
        .collect()
}

/// Decodes one frame's raw `[3549, 85]` YOLOX output (anchor-major). For each
/// anchor the confidence is `obj · best class`; the anchor counts only if that
/// WINNING class is in `keep` (taking the max over the kept classes alone would
/// let a person anchor with a weak truck score sneak in as a vehicle). Boxes
/// are decoded on the stride grids, NMS'd per output label and normalized by
/// the letterbox content `(dw, dh)` — the part of the input the frame covers.
/// `pub(crate)` so the unit tests can drive it with a synthetic tensor.
pub(crate) fn decode_yolox(
    out: &[f32],
    content: (u32, u32),
    keep: &[(usize, &str)],
    score_threshold: f32,
) -> Vec<Detection> {
    // Output labels in first-seen order: several COCO ids may share one label
    // (car/bus/truck → "vehicle") and are suppressed against each other.
    let mut labels: Vec<&str> = Vec::with_capacity(keep.len());
    for &(_, label) in keep {
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    let mut candidates: Vec<Vec<FaceDetection>> = vec![Vec::with_capacity(16); labels.len()];
    let mut anchor = 0usize;
    for stride in STRIDES {
        let side = RESOLUTION / stride;
        let s = stride as f32;
        for gy in 0..side {
            for gx in 0..side {
                let a = &out[anchor * ATTRS..(anchor + 1) * ATTRS];
                anchor += 1;
                let obj = a[4];
                // Class scores are ≤ 1, so an objectness under the floor can
                // never produce a kept box — skips ~all anchors of a frame.
                if obj < score_threshold {
                    continue;
                }
                let mut cls = usize::MAX;
                let mut cls_score = 0.0f32;
                for (c, &v) in a[5..].iter().enumerate() {
                    if v > cls_score {
                        cls = c;
                        cls_score = v;
                    }
                }
                let score = obj * cls_score;
                if score < score_threshold {
                    continue;
                }
                let Some(&(_, label)) = keep.iter().find(|(id, _)| *id == cls) else {
                    continue;
                };
                let group = labels
                    .iter()
                    .position(|l| *l == label)
                    .expect("every kept label was registered above");
                let cx = (a[0] + gx as f32) * s;
                let cy = (a[1] + gy as f32) * s;
                let w = a[2].exp() * s;
                let h = a[3].exp() * s;
                candidates[group].push(FaceDetection {
                    bbox: (cx - w * 0.5, cy - h * 0.5, cx + w * 0.5, cy + h * 0.5),
                    score,
                    keypoints: None,
                });
            }
        }
    }
    let (cw, ch) = (content.0.max(1) as f32, content.1.max(1) as f32);
    let mut dets = Vec::new();
    // NMS runs per label: a person standing in front of a car must not be
    // suppressed by the car box (and vice versa).
    for (label, group) in labels.into_iter().zip(candidates) {
        for d in nms(group, NMS_IOU_THRESHOLD) {
            // Input px → fraction of the frame, clamped (the pad is off-frame).
            let x1 = (d.bbox.0 / cw).clamp(0.0, 1.0);
            let y1 = (d.bbox.1 / ch).clamp(0.0, 1.0);
            let x2 = (d.bbox.2 / cw).clamp(0.0, 1.0);
            let y2 = (d.bbox.3 / ch).clamp(0.0, 1.0);
            dets.push(Detection {
                klasa: label.to_string(),
                bbox: [x1, y1, (x2 - x1).max(0.0), (y2 - y1).max(0.0)],
                score: d.score,
                stan: Vec::new(),
                tekst: None,
                tekst_conf: None,
                tekst_thumb_ref: None,
                track_id: 0,
                vehicle_id: 0,
                vx: 0.0,
                vy: 0.0,
            });
        }
    }
    dets
}

/// Writes one RGB24 frame into batch slot `bi` of a flat NCHW buffer with the
/// YOLOX preprocessing: letterbox into the top-left of 416×416 (the SAME Q8
/// bilinear resize as the GPU kernel), pad 114, B,G,R planes, raw 0..255.
/// Returns the content size `(dw, dh)` the decode maps boxes back with.
pub(crate) fn fill_frame_yolox(
    data: &mut [f32],
    bi: usize,
    rgb: &[u8],
    w: u32,
    h: u32,
) -> Result<(u32, u32)> {
    let res = RESOLUTION as usize;
    let plane = res * res;
    let base = bi * 3 * plane;
    let slot = &mut data[base..base + 3 * plane];
    let pad = match INPUT_FIT {
        FrameFit::Letterbox { pad } => pad as f32,
        FrameFit::Stretch => 0.0,
    };
    slot.fill(pad);
    let (dw, dh) = letterbox_content(w, h, RESOLUTION);
    let resized = if (dw, dh) == (w, h) {
        std::borrow::Cow::Borrowed(rgb)
    } else {
        std::borrow::Cow::Owned(
            crate::vision::resize::resize_rgb(rgb, w, h, dw, dh)
                .map_err(|e| anyhow!("resize_rgb failed: {e}"))?,
        )
    };
    if resized.len() != (dw * dh * 3) as usize {
        bail!("RGB frame {w}x{h}: buffer of {} bytes", rgb.len());
    }
    let (b_plane, rest) = slot.split_at_mut(plane);
    let (g_plane, r_plane) = rest.split_at_mut(plane);
    let dw = dw as usize;
    for (y, row) in resized.chunks_exact(dw * 3).enumerate() {
        let off = y * res;
        for (x, px) in row.chunks_exact(3).enumerate() {
            r_plane[off + x] = px[0] as f32;
            g_plane[off + x] = px[1] as f32;
            b_plane[off + x] = px[2] as f32;
        }
    }
    Ok((dw as u32, dh))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Index of the first anchor of the stride-`stride` grid.
    fn grid_base(stride: u32) -> usize {
        STRIDES
            .iter()
            .take_while(|&&s| s != stride)
            .map(|s| ((RESOLUTION / s) * (RESOLUTION / s)) as usize)
            .sum()
    }

    /// Writes one raw anchor: offsets relative to cell `(gx, gy)` of `stride`
    /// so the decoded box is centred at `(cx, cy)` with size `w`×`h` (input px).
    #[allow(clippy::too_many_arguments)]
    fn put(
        out: &mut [f32],
        stride: u32,
        gx: u32,
        gy: u32,
        (cx, cy, w, h): (f32, f32, f32, f32),
        obj: f32,
        cls: usize,
        cls_score: f32,
    ) {
        let side = RESOLUTION / stride;
        let i = grid_base(stride) + (gy * side + gx) as usize;
        let s = stride as f32;
        let a = &mut out[i * ATTRS..(i + 1) * ATTRS];
        a[0] = cx / s - gx as f32;
        a[1] = cy / s - gy as f32;
        a[2] = (w / s).ln();
        a[3] = (h / s).ln();
        a[4] = obj;
        a[5 + cls] = cls_score;
    }

    fn empty() -> Vec<f32> {
        // dw = dh = ln(1) = 0 → 1-stride boxes; objectness 0 everywhere.
        vec![0f32; ANCHORS * ATTRS]
    }

    #[test]
    fn anchor_grid_covers_the_whole_output() {
        let total: u32 = STRIDES.iter().map(|s| (RESOLUTION / s).pow(2)).sum();
        assert_eq!(total as usize, ANCHORS);
        assert_eq!(grid_base(16), 52 * 52);
        assert_eq!(grid_base(32), 52 * 52 + 26 * 26);
    }

    /// Grid math on every stride: the decoded box lands where it was encoded,
    /// the score is obj·class (no extra sigmoid), and a stretch content of
    /// 416×416 normalizes by the whole input.
    #[test]
    fn decode_places_boxes_on_every_stride_grid() {
        let mut out = empty();
        put(&mut out, 8, 3, 5, (30.0, 44.0, 16.0, 32.0), 0.9, 7, 0.8);
        put(&mut out, 16, 20, 7, (330.0, 120.0, 64.0, 48.0), 0.8, 2, 0.9);
        put(
            &mut out,
            32,
            12,
            12,
            (400.0, 400.0, 20.0, 20.0),
            0.95,
            5,
            0.95,
        );
        let mut dets = decode_yolox(&out, (416, 416), VEHICLE_CLASSES, VEHICLE_SCORE_THRESHOLD);
        dets.sort_by(|a, b| a.bbox[0].total_cmp(&b.bbox[0]));
        assert_eq!(dets.len(), 3);
        let r = RESOLUTION as f32;
        let want = [
            ([22.0, 28.0, 16.0, 32.0], 0.72),
            ([298.0, 96.0, 64.0, 48.0], 0.72),
            ([390.0, 390.0, 20.0, 20.0], 0.9025),
        ];
        for (d, (b, score)) in dets.iter().zip(want) {
            for k in 0..4 {
                assert!((d.bbox[k] - b[k] / r).abs() < 1e-4, "{:?} vs {b:?}", d.bbox);
            }
            assert!((d.score - score).abs() < 1e-5, "score {}", d.score);
            assert_eq!(d.klasa, VEHICLE_CLASS);
        }
    }

    /// Letterbox mapping: a 1280×720 frame fills 416×234 of the input, so a box
    /// at input px maps to frame fractions of the CONTENT, and a box reaching
    /// into the pad is clamped to the frame edge.
    #[test]
    fn decode_maps_letterbox_content_back_to_the_frame() {
        let content = letterbox_content(1280, 720, RESOLUTION);
        assert_eq!(content, (416, 234));
        let mut out = empty();
        // Centre (104, 117), 52×78 → x 78..130, y 78..156 in input px.
        put(&mut out, 16, 6, 7, (104.0, 117.0, 52.0, 78.0), 0.9, 0, 0.9);
        // A box whose bottom half hangs into the pad below y = 234.
        put(&mut out, 32, 3, 7, (100.0, 234.0, 40.0, 60.0), 0.9, 0, 0.9);
        let mut dets = decode_yolox(&out, content, PERSON_CLASSES, PERSON_SCORE_THRESHOLD);
        dets.sort_by(|a, b| a.bbox[1].total_cmp(&b.bbox[1]));
        assert_eq!(dets.len(), 2);
        let a = &dets[0];
        assert!((a.bbox[0] - 78.0 / 416.0).abs() < 1e-4);
        assert!((a.bbox[1] - 78.0 / 234.0).abs() < 1e-4);
        assert!((a.bbox[2] - 52.0 / 416.0).abs() < 1e-4);
        assert!((a.bbox[3] - 78.0 / 234.0).abs() < 1e-4);
        let b = &dets[1];
        assert!((b.bbox[1] - 204.0 / 234.0).abs() < 1e-4);
        assert!(
            (b.bbox[1] + b.bbox[3] - 1.0).abs() < 1e-6,
            "clamped to the frame"
        );
    }

    /// Only the WINNING class counts, objectness gates the score, and NMS runs
    /// per label: a duplicate truck is suppressed, a person on top of a car is
    /// not, a person is dropped from the vehicle set.
    #[test]
    fn decode_filters_classes_and_runs_nms_per_label() {
        let mut out = empty();
        put(
            &mut out,
            16,
            10,
            10,
            (168.0, 168.0, 80.0, 120.0),
            0.95,
            7,
            0.95,
        ); // truck
        put(
            &mut out,
            16,
            11,
            10,
            (170.0, 169.0, 82.0, 118.0),
            0.9,
            7,
            0.9,
        ); // duplicate truck
        put(
            &mut out,
            16,
            10,
            11,
            (166.0, 170.0, 60.0, 110.0),
            0.9,
            0,
            0.9,
        ); // person on it
        put(&mut out, 8, 40, 40, (324.0, 324.0, 20.0, 40.0), 0.9, 0, 0.3); // weak: 0.27
        put(&mut out, 8, 20, 20, (164.0, 164.0, 20.0, 40.0), 0.2, 0, 1.0); // low objectness
                                                                           // Person anchor with a weaker truck runner-up: never a vehicle.
        put(&mut out, 8, 45, 5, (364.0, 44.0, 20.0, 40.0), 0.9, 0, 0.9);
        let i = grid_base(8) + (5 * 52 + 45) as usize;
        out[i * ATTRS + 5 + 7] = 0.5;

        let vehicles = decode_yolox(&out, (416, 416), VEHICLE_CLASSES, VEHICLE_SCORE_THRESHOLD);
        assert_eq!(
            vehicles.len(),
            1,
            "duplicate truck suppressed: {vehicles:?}"
        );
        assert!((vehicles[0].score - 0.9025).abs() < 1e-5);

        let persons = decode_yolox(&out, (416, 416), PERSON_CLASSES, PERSON_SCORE_THRESHOLD);
        assert_eq!(persons.len(), 2, "{persons:?}");

        let both: Vec<(usize, &str)> = PERSON_CLASSES
            .iter()
            .chain(VEHICLE_CLASSES)
            .copied()
            .collect();
        let mixed = decode_yolox(&out, (416, 416), &both, PERSON_SCORE_THRESHOLD);
        let n_person = mixed.iter().filter(|d| d.klasa == PERSON_CLASS).count();
        let n_vehicle = mixed.iter().filter(|d| d.klasa == VEHICLE_CLASS).count();
        assert_eq!((n_person, n_vehicle), (2, 1));
    }

    #[test]
    fn host_fill_letterboxes_bgr_raw_with_pad() {
        let (w, h) = (832u32, 468u32); // exact 2× of the 416×234 content
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        for px in rgb.chunks_exact_mut(3) {
            px.copy_from_slice(&[200, 100, 10]);
        }
        let mut data = vec![0f32; 2 * frame_elements()];
        let content = fill_frame_yolox(&mut data, 1, &rgb, w, h).unwrap();
        assert_eq!(content, (416, 234));
        let plane = (RESOLUTION * RESOLUTION) as usize;
        let slot = &data[frame_elements()..];
        assert_eq!(&data[..frame_elements()], &vec![0f32; frame_elements()][..]);
        // B, G, R planes, raw values.
        assert_eq!(slot[0], 10.0);
        assert_eq!(slot[plane], 100.0);
        assert_eq!(slot[2 * plane], 200.0);
        // Row 234 is the first pad row.
        let row = 234 * RESOLUTION as usize;
        assert_eq!(slot[row - 1], 10.0);
        assert_eq!(slot[row], 114.0);
        assert_eq!(slot[2 * plane + row + 7], 114.0);
    }

    /// Reference detections of the official YOLOX-tiny on the Ultralytics
    /// `bus.jpg`, persons as (score, xyxy frame px), from Python/onnxruntime
    /// (CPU EP) with the SAME preprocessing chain as the pipeline: the photo
    /// half-pixel-bilinear resized to 1280×720 (`resize_rgb`, the fixture),
    /// then letterboxed into 416 with the same non-antialiased bilinear the
    /// host fill and the CUDA kernel use. With an antialiased (PIL) resize
    /// instead the scores are 0.883 / 0.864 / 0.831 / 0.429 — the gap is the
    /// resize, not the decode.
    const REFERENCE_PERSONS: [(f32, [f32; 4]); 4] = [
        (0.884, [87.0, 253.0, 369.0, 604.0]),
        (0.817, [1116.0, 267.0, 1280.0, 577.0]),
        (0.809, [358.0, 265.0, 541.0, 571.0]),
        (0.367, [2.0, 368.0, 124.0, 582.0]),
    ];

    /// Reference frame size the detections above are measured in.
    const REF_W: f32 = 1280.0;
    const REF_H: f32 = 720.0;

    fn frame_px(d: &Detection) -> [f32; 4] {
        [
            d.bbox[0] * REF_W,
            d.bbox[1] * REF_H,
            (d.bbox[0] + d.bbox[2]) * REF_W,
            (d.bbox[1] + d.bbox[3]) * REF_H,
        ]
    }

    /// Asserts `persons` reproduce [`REFERENCE_PERSONS`] within `max_px` and
    /// `max_score`, printing the per-box deviation.
    fn assert_matches_reference(
        label: &str,
        mut persons: Vec<Detection>,
        max_score: f32,
        max_px: f32,
    ) {
        persons.sort_by(|a, b| b.score.total_cmp(&a.score));
        assert_eq!(
            persons.len(),
            REFERENCE_PERSONS.len(),
            "{label}: {persons:?}"
        );
        for (d, (score, bbox)) in persons.iter().zip(REFERENCE_PERSONS) {
            let px = frame_px(d);
            let dev = px
                .iter()
                .zip(bbox)
                .map(|(g, w)| (g - w).abs())
                .fold(0.0f32, f32::max);
            eprintln!(
                "{label}: person {:.3} (ref {score:.3}) {:?} max px dev {dev:.1}",
                d.score,
                px.map(|v| v.round())
            );
            assert!(
                (d.score - score).abs() < max_score,
                "{label}: score {} vs {score}",
                d.score
            );
            assert!(dev < max_px, "{label}: {px:?} vs {bbox:?}");
        }
    }

    /// p50/p95 of `iters` timed calls after a warm-up, in ms.
    fn time_ms<T>(iters: usize, mut f: impl FnMut() -> T) -> (f64, f64, T) {
        for _ in 0..20 {
            f();
        }
        let mut times = Vec::with_capacity(iters);
        let mut last = None;
        for _ in 0..iters {
            let t = std::time::Instant::now();
            last = Some(f());
            times.push(t.elapsed().as_secs_f64() * 1e3);
        }
        times.sort_by(|a, b| a.total_cmp(b));
        (
            times[times.len() / 2],
            times[times.len() * 95 / 100],
            last.expect("iters > 0"),
        )
    }

    /// Real model on a real photo through the HOST letterbox preprocess, the
    /// pool forward and the decode. Needs `yolox_tiny.onnx` in
    /// `vision_models_dir()` and `TENTAFLOW_YOLO_TEST_IMAGE` (the Ultralytics
    /// `bus.jpg`).
    #[test]
    #[ignore = "needs yolox_tiny.onnx and TENTAFLOW_YOLO_TEST_IMAGE"]
    fn yolox_host_path_matches_python_reference() {
        let rgb = crate::vision::test_frames::bus_rgb_1280x720();
        let det = CocoDetector::load().expect("load detector");
        let mut data = vec![0f32; frame_elements()];
        let content = fill_frame_yolox(&mut data, 0, &rgb, 1280, 720).unwrap();
        assert_eq!(content, (416, 234));
        let (p50, p95, out) = time_ms(100, || {
            det.forward(data.as_mut_ptr() as usize, InputMemory::Host, 1)
                .unwrap()
        });
        eprintln!("YOLOX-tiny host-tensor forward ms: p50={p50:.2} p95={p95:.2}");
        let persons = decode_yolox(&out[0], content, PERSON_CLASSES, PERSON_SCORE_THRESHOLD);
        // Same input bytes as the reference: only FP16/EP rounding remains.
        assert_matches_reference("host", persons, 0.01, 2.0);
        let vehicles = det.detect_batch(&[(rgb.as_slice(), 1280, 720)]).unwrap();
        assert_eq!(vehicles[0].len(), 1, "one bus: {:?}", vehicles[0]);
    }

    /// Real model on a real photo through the GPU preprocess and the
    /// device-input entry point, plus the host and NV12-batch paths. Needs
    /// `yolox_tiny.onnx` in `vision_models_dir()`, CUDA and the image in
    /// `TENTAFLOW_YOLO_TEST_IMAGE` (the Ultralytics `bus.jpg`).
    #[cfg(all(
        any(target_os = "linux", target_os = "windows"),
        feature = "vision-cuda-preprocess"
    ))]
    #[test]
    #[ignore = "needs CUDA, yolox_tiny.onnx and TENTAFLOW_YOLO_TEST_IMAGE"]
    fn yolox_matches_python_reference_on_real_image() {
        use crate::vision::gpu_preprocess::{
            preprocess_nv12_device_into, GpuStream, Nv12Frame, OwnedDeviceTensor,
        };
        use crate::vision::test_frames::{bus_frame_1280x720, DeviceNv12};

        let (rgb, nv12) = bus_frame_1280x720();
        let (w, h) = (nv12.w, nv12.h);
        let dev = DeviceNv12::upload(&nv12);
        let det = CocoDetector::load().expect("load detector");
        let stream = GpuStream::new().unwrap();
        let mut input = OwnedDeviceTensor::alloc(RESOLUTION as usize).unwrap();
        let run = || {
            let content = preprocess_nv12_device_into(
                dev.planes(),
                &mut input,
                INPUT_MEAN,
                INPUT_STD,
                INPUT_FIT,
                INPUT_ORDER,
                nv12.color,
                &stream,
            )
            .unwrap();
            stream.synchronize().unwrap();
            unsafe {
                det.detect_device_input(
                    input.device_ptr(),
                    content,
                    PERSON_CLASSES,
                    PERSON_SCORE_THRESHOLD,
                )
            }
            .unwrap()
        };
        let (p50, p95, persons) = time_ms(200, run);
        eprintln!(
            "YOLOX-tiny NV12 preprocess+forward+decode ms: p50={p50:.2} p95={p95:.2} (pool={})",
            det.pool_size()
        );
        // NV12 (4:2:0 chroma) round trip on top of the same resize.
        assert_matches_reference("device", persons, 0.03, 6.0);

        // Host letterbox path and NV12 batch path agree with the device path.
        let host = det.detect_batch(&[(rgb.as_slice(), w, h)]).unwrap();
        let frame = Nv12Frame {
            y: &nv12.y,
            y_stride: nv12.w as usize,
            uv: &nv12.uv,
            uv_stride: nv12.w as usize,
            w,
            h,
        };
        let batch = det.detect_batch_gpu(&[frame, frame], nv12.color).unwrap();
        let content = letterbox_content(w, h, RESOLUTION);
        let device_vehicles = {
            preprocess_nv12_device_into(
                dev.planes(),
                &mut input,
                INPUT_MEAN,
                INPUT_STD,
                INPUT_FIT,
                INPUT_ORDER,
                nv12.color,
                &stream,
            )
            .unwrap();
            stream.synchronize().unwrap();
            unsafe {
                det.detect_device_input(
                    input.device_ptr(),
                    content,
                    VEHICLE_CLASSES,
                    VEHICLE_SCORE_THRESHOLD,
                )
            }
            .unwrap()
        };
        for d in host[0].iter().chain(&device_vehicles) {
            eprintln!(
                "vehicle {:.3} {:?}",
                d.score,
                frame_px(d).map(|v| v.round())
            );
        }
        assert_eq!(device_vehicles.len(), 1, "one bus: {device_vehicles:?}");
        assert_eq!(host[0].len(), 1, "host path finds the bus: {:?}", host[0]);
        assert_eq!(batch.len(), 2);
        for b in &batch {
            assert_eq!(b.len(), 1);
            for k in 0..4 {
                assert!((b[0].bbox[k] - device_vehicles[0].bbox[k]).abs() < 1e-3);
            }
        }
        let iou_host = {
            let (a, b) = (frame_px(&host[0][0]), frame_px(&device_vehicles[0]));
            let iw = (a[2].min(b[2]) - a[0].max(b[0])).max(0.0);
            let ih = (a[3].min(b[3]) - a[1].max(b[1])).max(0.0);
            let i = iw * ih;
            i / ((a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - i)
        };
        assert!(iou_host > 0.95, "host vs device bus IoU {iou_host}");
    }
}
