// =============================================================================
// File: vision/detector_vehicle.rs — YOLOv8 vehicle detector (ort+CUDA)
// =============================================================================
//
// A SECOND, parallel detector that runs alongside RF-DETR (ADR/plate/sticker
// detector) on the SAME frame so each ADR placard / plate / sticker can be
// associated to the vehicle it sits on — the per-truck separation. It uses its
// OWN ort session pool (independent CUDA streams), so a `tokio::join!` of the
// two forwards costs ~max(DETR, YOLO), not their sum.
//
// Model: YOLOv8n COCO ONNX 640×640, single output `output0 [1,84,8400]` (4 bbox
// cxcywh in 0..640 + 80 class scores, NO objectness). Input `images
// [1,3,640,640]` f32 RGB, `/255`, NO ImageNet normalize (this DIFFERS from
// RF-DETR, which per-channel ImageNet-normalizes). We keep the COCO vehicle
// classes {2 car, 5 bus, 7 truck}, map them all to a single `klasa="vehicle"`,
// NMS, and normalize the bbox to 0..1 of the frame — the exact `Detection` shape
// the rest of the pipeline consumes.
//
// This detector is generation-only in the sense that it has no cold/enrichment
// stage: it only produces vehicle boxes. Association + tracking happen in the
// camera-ingest layer. `detect_batch` / `detect_batch_gpu` / `detect_device_pixels`
// mirror the RF-DETR device/NV12/RGB entry points so the launcher can feed the
// same frame the RF-DETR path already has.

#![cfg(all(feature = "inference-vision-gpu", feature = "vision-ort"))]

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use tracing::info;

use crate::paths;
use crate::services::detection_bus::Detection;
use crate::vision::nms::nms;
use crate::vision::FaceDetection;

/// Square input resolution the exported YOLOv8 graph expects.
pub const RESOLUTION: u32 = 640;

/// Input tensor name in the YOLOv8 ONNX graph.
const INPUT_NAME: &str = "images";

/// Output tensor name in the YOLOv8 ONNX graph.
const OUTPUT_NAME: &str = "output0";

/// Confidence floor for a kept vehicle box (max class score over the 80 COCO
/// classes, restricted to the vehicle set). Vehicles are large, high-contrast
/// objects; 0.35 keeps distant trucks while dropping background noise.
const SCORE_THRESHOLD: f32 = 0.35;

/// NMS IoU threshold for overlapping vehicle boxes.
const NMS_IOU_THRESHOLD: f32 = 0.45;

/// COCO class ids we treat as a vehicle: 2 = car, 5 = bus, 7 = truck. All map to
/// a single `klasa = "vehicle"` — the association layer only needs the box.
const VEHICLE_COCO_IDS: [usize; 3] = [2, 5, 7];

/// The one class name every kept box carries.
pub const VEHICLE_CLASS: &str = "vehicle";

/// Number of COCO classes in the YOLOv8 head (`84 = 4 bbox + 80 classes`).
const NUM_CLASSES: usize = 80;

/// Small session pool — vehicle detection is a coarse task and the model is tiny
/// (~tens of MB per session), so 2 sessions give parallel forwards at negligible
/// VRAM. Configurable via `[vision] vehicle_sessions`.
const DEFAULT_VEHICLE_SESSIONS: usize = 2;

/// `vehicle-classes.json` shape: `{ "classes": [...80 COCO names...] }`. Only the
/// count is validated (the mapping to `{2,5,7}` is by index, not name), but the
/// file is required so the deploy artifact stays self-describing.
#[derive(Debug, Deserialize)]
struct ClassesFile {
    classes: Vec<String>,
}

thread_local! {
    /// Reusable host input buffer for `detect_batch`, mirroring the RF-DETR
    /// scratch: grows to the largest batch this thread has seen and stays
    /// resident so the per-batch alloc + page-fault churn is paid once. Passed
    /// to ort as a BORROWED tensor (raw pointer, blocking run).
    static HOST_INPUT_SCRATCH: std::cell::RefCell<Vec<f32>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Loaded YOLOv8 vehicle detector: its own ort session pool + the COCO class
/// table. `detect*` take `&self` (the pool is internally concurrent), so the
/// launcher can drive it in parallel with RF-DETR.
pub struct VehicleDetector {
    pool: crate::vision::ort_common::SessionPool,
}

impl VehicleDetector {
    /// Builds the detector from the deploy-time model dir
    /// (`vision_models_dir()/yolov8n-vehicle.onnx` + `vehicle-classes.json`).
    pub fn load() -> Result<Self> {
        let dir = paths::vision_models_dir();
        let classes_path = dir.join("vehicle-classes.json");
        let classes_bytes = std::fs::read(&classes_path)
            .with_context(|| format!("read {}", classes_path.display()))?;
        let parsed: ClassesFile = serde_json::from_slice(&classes_bytes)
            .with_context(|| format!("parse {}", classes_path.display()))?;
        if parsed.classes.len() != NUM_CLASSES {
            bail!(
                "vehicle-classes.json has {} classes, expected {NUM_CLASSES} (COCO)",
                parsed.classes.len()
            );
        }

        let onnx_path = dir.join("yolov8n-vehicle.onnx");
        if !onnx_path.exists() {
            bail!("vehicle ONNX missing: {}", onnx_path.display());
        }
        crate::vision::ort_common::ensure_ort_dylib();
        let trt_profile = crate::vision::ort_common::TrtShapeProfile {
            input_name: INPUT_NAME.to_string(),
            min_batch: 1,
            opt_batch: 1,
            max_batch: crate::vision::detector_rfdetr::MODEL_BATCH,
            channels: 3,
            height: RESOLUTION,
            width: RESOLUTION,
        };
        let n = crate::vision::ort_common::pool_size(vehicle_sessions());
        let pool = crate::vision::ort_common::build_session_pool_from_file(
            &onnx_path,
            &dir.join("trt-cache-vehicle"),
            Some(&trt_profile),
            n,
            // FP16 ok — vehicle localization is coarse and robust to it.
            true,
        )
        .map_err(|e| {
            anyhow!("building YOLOv8 vehicle ort session pool of {n} session(s): {e:#}")
        })?;
        info!(
            "[vehicle] loaded {} (backend ort TensorRT→CUDA→CPU, pool={} session(s))",
            onnx_path.display(),
            pool.len()
        );
        Ok(Self { pool })
    }

    /// Number of pooled ort sessions.
    pub fn pool_size(&self) -> usize {
        self.pool.len()
    }

    /// Single-frame RGB convenience. Delegates to `detect_batch` (N=1).
    pub fn detect(&self, rgb: &[u8], w: u32, h: u32) -> Result<Vec<Detection>> {
        Ok(self
            .detect_batch(&[(rgb, w, h)])?
            .into_iter()
            .next()
            .unwrap_or_default())
    }

    /// Runs N RGB frames through one host-tensor forward on the dynamic-batch
    /// graph. Preprocess = stretch-resize to 640 + `/255`, RGB, NCHW (no
    /// ImageNet normalize). Output order == input order, length == `frames.len()`.
    pub fn detect_batch(&self, frames: &[(&[u8], u32, u32)]) -> Result<Vec<Vec<Detection>>> {
        if frames.is_empty() {
            return Ok(Vec::new());
        }
        let res = RESOLUTION as usize;
        let n = frames.len();
        HOST_INPUT_SCRATCH.with(|cell| {
            let mut data = cell.borrow_mut();
            let need = n * 3 * res * res;
            if data.len() < need {
                data.resize(need, 0.0);
            }
            for (bi, &(rgb, w, h)) in frames.iter().enumerate() {
                fill_frame_yolo(&mut data[..need], bi, rgb, w, h)?;
            }
            let data_ptr = data.as_mut_ptr() as usize;
            let (out_owned, attrs, anchors) = self.pool.run(move |session| {
                // SAFETY: `data_ptr` covers exactly n·3·res·res initialized f32 in
                // this thread's scratch, held under a RefCell borrow for the whole
                // blocking run; nothing can reallocate it mid-run.
                let value = unsafe {
                    ort::value::TensorRefMut::<f32>::from_raw(
                        ort::memory::MemoryInfo::default(),
                        (data_ptr as *mut ()).cast(),
                        ort::value::Shape::new([n as i64, 3, res as i64, res as i64]),
                    )
                }
                .map_err(|e| anyhow!("vehicle-ort: TensorRefMut::from_raw: {e}"))?;
                let outputs = session
                    .run(ort::inputs! { INPUT_NAME => value })
                    .map_err(|e| anyhow!("vehicle-ort: session.run: {e}"))?;
                let (shape, out) = outputs[OUTPUT_NAME]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| anyhow!("vehicle-ort: extract {OUTPUT_NAME}: {e}"))?;
                let (attrs, anchors) = validate_output_shape(shape, out.len(), n)?;
                Ok((out.to_vec(), attrs, anchors))
            })?;
            Ok(decode_yolo_batch(&out_owned, n, attrs, anchors))
        })
    }

    /// GPU-resident detect from a batch of NV12 frames — mirror of the RF-DETR
    /// `detect_batch_gpu`, but with YOLO preprocessing (mean 0, std 255, no
    /// ImageNet normalize) so the fused kernel yields the `/255` RGB the graph
    /// expects. Preprocesses on the GPU into a device buffer and hands its
    /// pointer to ort (zero host↔device copy of the model input).
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
        let res = RESOLUTION as usize;
        let n = frames.len();
        let batch = crate::vision::gpu_preprocess::preprocess_nv12_batch_gpu(
            frames, res, YOLO_MEAN, YOLO_STD, color,
        )?;
        let out = self.forward_device_ptr(batch.device_ptr() as usize, n, res);
        drop(batch);
        out
    }

    /// GPU-resident detect from a batch of tightly-packed RGB24 frames (each
    /// `(&[u8], w, h)`, `len == w*h*3`) — the RGB analogue of `detect_batch_gpu`.
    /// The launcher uses this when the detect frame is host RGB but a GPU
    /// preprocess is still cheaper than the CPU resize. Same YOLO mean/std.
    #[cfg(all(
        any(target_os = "linux", target_os = "windows"),
        feature = "vision-cuda-preprocess"
    ))]
    pub fn detect_device_pixels(
        &self,
        frames: &[(&[u8], u32, u32)],
    ) -> Result<Vec<Vec<Detection>>> {
        if frames.is_empty() {
            return Ok(Vec::new());
        }
        let res = RESOLUTION as usize;
        let n = frames.len();
        let batch =
            crate::vision::gpu_preprocess::preprocess_batch_gpu(frames, res, YOLO_MEAN, YOLO_STD)?;
        let out = self.forward_device_ptr(batch.device_ptr() as usize, n, res);
        drop(batch);
        out
    }

    /// Shared ort device-tensor forward + decode for both GPU preprocess paths.
    /// `dev_ptr` is a CUDA device-0 buffer of exactly `n·3·res·res` f32 that the
    /// caller keeps alive for the whole blocking run.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    fn forward_device_ptr(
        &self,
        dev_ptr: usize,
        n: usize,
        res: usize,
    ) -> Result<Vec<Vec<Detection>>> {
        let (out_owned, attrs, anchors) = self.pool.run(move |session| {
            use ort::memory::{AllocationDevice, AllocatorType, MemoryInfo, MemoryType};
            use ort::value::{Shape, TensorRefMut};

            let info = MemoryInfo::new(
                AllocationDevice::CUDA,
                0,
                AllocatorType::Device,
                MemoryType::Default,
            )
            .map_err(|e| anyhow!("vehicle-ort gpu: MemoryInfo::new: {e}"))?;
            // SAFETY: `dev_ptr` is a CUDA device buffer of exactly n·3·res·res f32,
            // valid on device 0, kept alive by the caller for the whole run.
            let tensor = unsafe {
                TensorRefMut::<f32>::from_raw(
                    info,
                    (dev_ptr as *mut ()).cast(),
                    Shape::new([n as i64, 3, res as i64, res as i64]),
                )
            }
            .map_err(|e| anyhow!("vehicle-ort gpu: TensorRefMut::from_raw: {e}"))?;
            let outputs = session
                .run(ort::inputs! { INPUT_NAME => tensor })
                .map_err(|e| anyhow!("vehicle-ort gpu: session.run: {e}"))?;
            let (shape, out) = outputs[OUTPUT_NAME]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow!("vehicle-ort gpu: extract {OUTPUT_NAME}: {e}"))?;
            let (attrs, anchors) = validate_output_shape(shape, out.len(), n)?;
            Ok((out.to_vec(), attrs, anchors))
        })?;
        Ok(decode_yolo_batch(&out_owned, n, attrs, anchors))
    }
}

/// Per-channel YOLO normalize: `/255`, no ImageNet mean/std. Expressed as the
/// mean/std pair the fused GPU kernel and `normalize_hwc` share
/// (`v/255 → (v - 0) / 255`).
#[cfg(any(target_os = "linux", target_os = "windows"))]
const YOLO_MEAN: [f32; 3] = [0.0, 0.0, 0.0];
#[cfg(any(target_os = "linux", target_os = "windows"))]
const YOLO_STD: [f32; 3] = [255.0, 255.0, 255.0];

/// Resolves the vehicle detector pool size from `[vision] vehicle_sessions`
/// (defaults to [`DEFAULT_VEHICLE_SESSIONS`] when unset/zero).
fn vehicle_sessions() -> usize {
    let cfg = crate::vision::settings::get().vehicle_sessions;
    if cfg == 0 {
        DEFAULT_VEHICLE_SESSIONS
    } else {
        cfg
    }
}

/// Validates the `output0` shape against the batch size and returns
/// `(attrs=84, anchors=8400)`. Requires the standard COCO YOLOv8 layout `[n,
/// 84, 8400]` (channel-major); a transposed `[n, 8400, 84]` export is rejected
/// with a clear message (the exporter contract emits the standard layout).
/// `shape` derefs to `[i64]` (ort `Shape`).
fn validate_output_shape(shape: &[i64], data_len: usize, n: usize) -> Result<(usize, usize)> {
    if shape.len() != 3 {
        bail!("vehicle-ort: unexpected output rank {shape:?}, want [n, 84, 8400]");
    }
    if shape[0] as usize != n {
        bail!("vehicle-ort: output batch {} != {n}", shape[0]);
    }
    let attrs = 4 + NUM_CLASSES;
    if shape[1] as usize != attrs {
        bail!(
            "vehicle-ort: output axis 1 = {} != attrs {attrs}; export the standard [n, 84, 8400] \
             (transposed [n, 8400, 84] unsupported)",
            shape[1]
        );
    }
    let anchors = shape[2] as usize;
    let need = n * attrs * anchors;
    if data_len < need {
        bail!("vehicle-ort: output buffer too short: {data_len} < {need}");
    }
    Ok((attrs, anchors))
}

/// Decodes the flat `[n, attrs, anchors]` YOLOv8 output into per-image vehicle
/// detections. Shared by every forward path so the decode is identical.
/// `pub(crate)` so the unit test can drive it against a synthetic tensor.
pub(crate) fn decode_yolo_batch(
    data: &[f32],
    n: usize,
    attrs: usize,
    anchors: usize,
) -> Vec<Vec<Detection>> {
    let mut results = Vec::with_capacity(n);
    let slot = attrs * anchors;
    for bi in 0..n {
        let base = bi * slot;
        results.push(decode_yolo_image(&data[base..base + slot], anchors));
    }
    results
}

/// Decodes one image's `[attrs, anchors]` (row-major, channel-major) YOLOv8
/// slice: for each anchor take the max class score over the 80 classes, keep it
/// only if the winning class is a vehicle {2,5,7} and the score clears the floor,
/// convert cxcywh (0..640) → xyxy, NMS, then normalize to 0..1 of the 640 grid
/// (which is the whole frame, since the preprocess stretch-resized it).
fn decode_yolo_image(slice: &[f32], anchors: usize) -> Vec<Detection> {
    // Channel-major: attribute `a` of anchor `i` is at `a * anchors + i`.
    let at = |a: usize, i: usize| slice[a * anchors + i];
    let mut candidates: Vec<FaceDetection> = Vec::with_capacity(64);
    for i in 0..anchors {
        // Overall argmax over the 80 class scores. An anchor is a vehicle ONLY if
        // its TOP class is one of {car,bus,truck}; the winning score is then its
        // confidence. (Taking the max over just the vehicle classes would let a
        // "person" anchor with a weak truck score sneak in.)
        let mut overall_best = 0.0f32;
        let mut overall_cls = usize::MAX;
        for c in 0..NUM_CLASSES {
            let s = at(4 + c, i);
            if s > overall_best {
                overall_best = s;
                overall_cls = c;
            }
        }
        if !VEHICLE_COCO_IDS.contains(&overall_cls) || overall_best < SCORE_THRESHOLD {
            continue;
        }
        let best_score = overall_best;
        let cx = at(0, i);
        let cy = at(1, i);
        let w = at(2, i);
        let h = at(3, i);
        let x1 = cx - w * 0.5;
        let y1 = cy - h * 0.5;
        let x2 = cx + w * 0.5;
        let y2 = cy + h * 0.5;
        candidates.push(FaceDetection {
            bbox: (x1, y1, x2, y2),
            score: best_score,
            keypoints: None,
        });
    }
    let kept = nms(candidates, NMS_IOU_THRESHOLD);
    let res = RESOLUTION as f32;
    kept.into_iter()
        .map(|d| {
            // xyxy (0..640) → normalized [x, y, w, h] (0..1). Clamp to the frame.
            let x1 = (d.bbox.0 / res).clamp(0.0, 1.0);
            let y1 = (d.bbox.1 / res).clamp(0.0, 1.0);
            let x2 = (d.bbox.2 / res).clamp(0.0, 1.0);
            let y2 = (d.bbox.3 / res).clamp(0.0, 1.0);
            Detection {
                klasa: VEHICLE_CLASS.to_string(),
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
            }
        })
        .collect()
}

/// Writes one RGB24 frame into batch slot `bi` of a flat NCHW buffer with YOLO
/// preprocessing: stretch-resize to 640×640, `/255`, RGB, NCHW. No ImageNet
/// normalize (the key difference from RF-DETR's `fill_frame`). `pub(crate)` for
/// the resize fast-path parity with the RF-DETR fill.
pub(crate) fn fill_frame_yolo(
    data: &mut [f32],
    bi: usize,
    rgb: &[u8],
    w: u32,
    h: u32,
) -> Result<()> {
    let res = RESOLUTION as usize;
    let plane = res * res;
    let base = bi * 3 * plane;
    let slot = &mut data[base..base + 3 * plane];
    if w == RESOLUTION && h == RESOLUTION && rgb.len() == plane * 3 {
        normalize_hwc(slot, rgb, plane);
        return Ok(());
    }
    let resized = crate::vision::resize::resize_rgb(rgb, w, h, RESOLUTION, RESOLUTION)
        .map_err(|e| anyhow!("resize_rgb failed: {e}"))?;
    normalize_hwc(slot, &resized, plane);
    Ok(())
}

/// Packs one 640×640 RGB24 frame into a `[3, plane]` NCHW slot with `/255`, no
/// normalize: single pass over interleaved pixels, contiguous per-plane writes.
fn normalize_hwc(out: &mut [f32], rgb: &[u8], plane: usize) {
    debug_assert_eq!(rgb.len(), plane * 3, "RGB24 length must cover the plane");
    let (r_plane, rest) = out.split_at_mut(plane);
    let (g_plane, b_plane) = rest.split_at_mut(plane);
    for (((px, r), g), b) in rgb
        .chunks_exact(3)
        .zip(r_plane.iter_mut())
        .zip(g_plane.iter_mut())
        .zip(b_plane.iter_mut())
    {
        *r = px[0] as f32 / 255.0;
        *g = px[1] as f32 / 255.0;
        *b = px[2] as f32 / 255.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a synthetic `[1, 84, 8400]` YOLOv8 output (channel-major) with a
    /// single strong TRUCK anchor and one strong PERSON anchor, verifying the
    /// decode keeps ONLY the vehicle (right class filtering), converts the box to
    /// normalized [x,y,w,h], and NMS leaves a single box.
    #[test]
    fn decode_keeps_only_vehicle_and_normalizes() {
        let attrs = 4 + NUM_CLASSES; // 84
        let anchors = 8400usize;
        let mut data = vec![0f32; attrs * anchors];
        let set = |data: &mut [f32], a: usize, i: usize, v: f32| {
            data[a * anchors + i] = v;
        };
        // Anchor 10: a TRUCK (class 7) centered at (320, 320) size 128×256.
        set(&mut data, 0, 10, 320.0);
        set(&mut data, 1, 10, 320.0);
        set(&mut data, 2, 10, 128.0);
        set(&mut data, 3, 10, 256.0);
        set(&mut data, 4 + 7, 10, 0.91); // truck score
                                         // Anchor 20: a PERSON (class 0) — must be dropped (not a vehicle).
        set(&mut data, 0, 20, 100.0);
        set(&mut data, 1, 20, 100.0);
        set(&mut data, 2, 20, 40.0);
        set(&mut data, 3, 20, 40.0);
        set(&mut data, 4, 20, 0.99); // person score
                                     // Anchor 11: a duplicate TRUCK overlapping anchor 10 (lower score) —
                                     // NMS must suppress it, leaving one box.
        set(&mut data, 0, 11, 322.0);
        set(&mut data, 1, 11, 322.0);
        set(&mut data, 2, 11, 130.0);
        set(&mut data, 3, 11, 258.0);
        set(&mut data, 4 + 7, 11, 0.80);

        let out = decode_yolo_batch(&data, 1, attrs, anchors);
        assert_eq!(out.len(), 1);
        let dets = &out[0];
        assert_eq!(dets.len(), 1, "one vehicle after NMS, person dropped");
        let d = &dets[0];
        assert_eq!(d.klasa, VEHICLE_CLASS);
        // (320,320) center, 128×256 → x1=256/640=0.4, y1=192/640=0.3, w=0.2, h=0.4.
        assert!((d.bbox[0] - 0.4).abs() < 1e-4, "x={}", d.bbox[0]);
        assert!((d.bbox[1] - 0.3).abs() < 1e-4, "y={}", d.bbox[1]);
        assert!((d.bbox[2] - 0.2).abs() < 1e-4, "w={}", d.bbox[2]);
        assert!((d.bbox[3] - 0.4).abs() < 1e-4, "h={}", d.bbox[3]);
        assert!((d.score - 0.91).abs() < 1e-6);
    }

    /// A frame with no vehicle-class anchor above the floor yields no boxes.
    #[test]
    fn decode_empty_when_no_vehicle() {
        let attrs = 4 + NUM_CLASSES;
        let anchors = 8400usize;
        let mut data = vec![0f32; attrs * anchors];
        // One strong CAT (class 15) — not a vehicle.
        data[(4 + 15) * anchors + 5] = 0.99;
        let out = decode_yolo_batch(&data, 1, attrs, anchors);
        assert_eq!(out.len(), 1);
        assert!(out[0].is_empty());
    }
}
