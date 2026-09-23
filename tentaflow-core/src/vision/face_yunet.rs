// =============================================================================
// File: vision/face_yunet.rs — YuNet face detector on ort (CUDA) for the privacy probe
// =============================================================================
//
// YuNet (OpenCV zoo, MIT, `face_detection_yunet_2023mar.onnx`) finds faces the
// person detector cannot: people cut off at the frame edge, faces behind a
// window, a face too close to the camera for a whole-body box. The privacy
// probe blurs its boxes next to the head regions derived from person boxes.
//
// Model contract: input `input [1,3,640,640]` f32 in B,G,R order with RAW
// 0..255 values, STRETCH-resized (measured better than a letterbox on the
// reference photo). Per stride s ∈ {8,16,32} over an (640/s)² grid (row-major)
// the graph emits `cls_s`/`obj_s [1,N,1]`, `bbox_s [1,N,4]` and `kps_s [1,N,10]`
// (landmarks, not read here). Decode: score = sqrt(clamp(cls)·clamp(obj)),
// `cx = (gx + b0)·s`, `w = exp(b2)·s` in input pixels; a stretch makes input
// pixels / 640 the fraction of the frame directly. NMS IoU 0.3.

#![cfg(all(feature = "inference-vision-gpu", feature = "vision-ort"))]

use anyhow::{anyhow, bail, Result};
use tracing::info;

use crate::paths;
use crate::vision::nms::nms;
use crate::vision::preprocessing::{ChannelOrder, FrameFit};
use crate::vision::FaceDetection;

/// Square input side of the exported graph.
pub const RESOLUTION: u32 = 640;

/// How a frame is fitted into the input.
pub const INPUT_FIT: FrameFit = FrameFit::Stretch;

/// Channel order of the input (OpenCV-trained).
pub const INPUT_ORDER: ChannelOrder = ChannelOrder::Bgr;

/// Mean/std pair for the fused GPU kernel (`(v/255 - mean)/std`): std = 1/255
/// yields the raw 0..255 value the model reads.
pub const INPUT_MEAN: [f32; 3] = [0.0, 0.0, 0.0];
pub const INPUT_STD: [f32; 3] = [1.0 / 255.0, 1.0 / 255.0, 1.0 / 255.0];

/// Default score floor for privacy. OpenCV's demo uses 0.9 because it wants
/// clean detections; here a missed face is a privacy leak while a false
/// positive only pixelates a patch of background, so the floor sits much lower.
/// On the reference photo the real faces score ≥ 0.9 and nothing else clears
/// 0.5.
pub const FACE_SCORE_THRESHOLD: f32 = 0.5;

/// NMS IoU threshold (the OpenCV zoo default for this model).
const NMS_IOU_THRESHOLD: f32 = 0.3;

/// Head strides; each emits its own group of four outputs.
const STRIDES: [u32; 3] = [8, 16, 32];

/// Input tensor name in the ONNX graph.
const INPUT_NAME: &str = "input";

/// Pool size: the privacy probe needs at most `min(privacy_cameras, 2)`
/// concurrent forwards and a session costs a few MB of VRAM.
const SESSIONS: usize = 2;

/// One detected face: `bbox` is `[x, y, w, h]` as fractions of the frame
/// (0..1), `score` the YuNet confidence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceBox {
    pub bbox: [f32; 4],
    pub score: f32,
}

/// Raw outputs of one stride group, borrowed from the ort result.
pub(crate) struct StrideOutputs<'a> {
    pub stride: u32,
    pub cls: &'a [f32],
    pub obj: &'a [f32],
    pub bbox: &'a [f32],
}

/// Loaded YuNet detector: one ort session pool, `&self` detect (the pool is
/// internally concurrent), shared by every privacy camera.
pub struct YuNetDetector {
    pool: crate::vision::ort_common::SessionPool,
}

impl YuNetDetector {
    /// Builds the detector from `vision_models_dir()/face_detection_yunet_2023mar.onnx`.
    pub fn load() -> Result<Self> {
        let dir = paths::vision_models_dir();
        let onnx_path = dir.join(crate::vision::camera_cv_models::YUNET_FILE);
        if !onnx_path.exists() {
            bail!("YuNet ONNX missing: {}", onnx_path.display());
        }
        crate::vision::ort_common::ensure_ort_dylib();
        let n = crate::vision::ort_common::pool_size(SESSIONS);
        let pool = crate::vision::ort_common::build_session_pool_from_file(
            &onnx_path,
            &dir.join("trt-cache-yunet"),
            // Static [1,3,640,640] graph: no dynamic-batch profile to pin.
            None,
            n,
            // FP32: small, distant faces sit near the score floor, and the
            // model is so small that FP16 buys nothing worth that risk.
            false,
        )
        .map_err(|e| anyhow!("building YuNet ort session pool of {n} session(s): {e:#}"))?;
        info!(
            "[yunet] loaded {} (backend ort TensorRT→CUDA→CPU, pool={} session(s))",
            onnx_path.display(),
            pool.len()
        );
        Ok(Self { pool })
    }

    /// Number of pooled ort sessions.
    pub fn pool_size(&self) -> usize {
        self.pool.len()
    }

    /// Single-frame face detect on a model input that ALREADY lives in CUDA
    /// memory. `input` is a `[1, 3, 640, 640]` f32 NCHW tensor on CUDA device 0
    /// prepared per [`INPUT_FIT`] / [`INPUT_ORDER`] / [`INPUT_MEAN`] /
    /// [`INPUT_STD`] (e.g. by `preprocess_nv12_device_into`), owned and reused
    /// by the caller. Returns the faces above `score_threshold` after NMS,
    /// normalized to 0..1 of the frame.
    ///
    /// Blocks the calling thread until the forward finished; the forward runs
    /// on the pool's dedicated session thread.
    ///
    /// # Safety
    /// * `input` must point to at least `3·640·640` initialized f32 in CUDA
    ///   device-0 memory and stay allocated until this call returns.
    /// * Every write to `input` must be COMPLETE before the call (ort reads it
    ///   on the CUDA EP's own stream, which does not wait on the caller's).
    /// * On return ort no longer reads `input`.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    pub unsafe fn detect_device_input(
        &self,
        input: *const f32,
        score_threshold: f32,
    ) -> Result<Vec<FaceBox>> {
        let ptr = input as usize;
        let res = RESOLUTION as i64;
        self.pool.run(move |session| {
            use ort::memory::{AllocationDevice, AllocatorType, MemoryInfo, MemoryType};
            use ort::value::{Shape, TensorRefMut};

            let info = MemoryInfo::new(
                AllocationDevice::CUDA,
                0,
                AllocatorType::Device,
                MemoryType::Default,
            )
            .map_err(|e| anyhow!("yunet-ort gpu: MemoryInfo::new: {e}"))?;
            // SAFETY: the caller guarantees 3·640·640 initialized f32 on device 0
            // at `ptr`, alive until this blocking run returns.
            let tensor = unsafe {
                TensorRefMut::<f32>::from_raw(
                    info,
                    (ptr as *mut ()).cast(),
                    Shape::new([1, 3, res, res]),
                )
            }
            .map_err(|e| anyhow!("yunet-ort gpu: TensorRefMut::from_raw: {e}"))?;
            let outputs = session
                .run(ort::inputs! { INPUT_NAME => tensor })
                .map_err(|e| anyhow!("yunet-ort gpu: session.run: {e}"))?;
            let mut groups = Vec::with_capacity(STRIDES.len());
            for stride in STRIDES {
                let n = ((RESOLUTION / stride) * (RESOLUTION / stride)) as usize;
                groups.push(StrideOutputs {
                    stride,
                    cls: output(&outputs, &format!("cls_{stride}"), n)?,
                    obj: output(&outputs, &format!("obj_{stride}"), n)?,
                    bbox: output(&outputs, &format!("bbox_{stride}"), 4 * n)?,
                });
            }
            Ok(decode_yunet(&groups, score_threshold))
        })
    }
}

/// Borrows the f32 data of output `name`, which must hold exactly `len` values.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn output<'a>(
    outputs: &'a ort::session::SessionOutputs<'_>,
    name: &str,
    len: usize,
) -> Result<&'a [f32]> {
    let (_, data) = outputs[name]
        .try_extract_tensor::<f32>()
        .map_err(|e| anyhow!("yunet-ort: extract {name}: {e}"))?;
    if data.len() != len {
        bail!("yunet-ort: {name} has {} values, want {len}", data.len());
    }
    Ok(data)
}

/// Decodes the per-stride YuNet outputs into NMS'd faces normalized by the
/// (stretched) input side. `pub(crate)` so unit tests drive it synthetically.
pub(crate) fn decode_yunet(groups: &[StrideOutputs<'_>], score_threshold: f32) -> Vec<FaceBox> {
    let mut candidates: Vec<FaceDetection> = Vec::new();
    for g in groups {
        let side = RESOLUTION / g.stride;
        let s = g.stride as f32;
        for (i, (&cls, &obj)) in g.cls.iter().zip(g.obj).enumerate() {
            let score = (cls.clamp(0.0, 1.0) * obj.clamp(0.0, 1.0)).sqrt();
            if score < score_threshold {
                continue;
            }
            let gx = (i as u32 % side) as f32;
            let gy = (i as u32 / side) as f32;
            let b = &g.bbox[i * 4..i * 4 + 4];
            let cx = (gx + b[0]) * s;
            let cy = (gy + b[1]) * s;
            let w = b[2].exp() * s;
            let h = b[3].exp() * s;
            candidates.push(FaceDetection {
                bbox: (cx - w * 0.5, cy - h * 0.5, cx + w * 0.5, cy + h * 0.5),
                score,
                keypoints: None,
            });
        }
    }
    let r = RESOLUTION as f32;
    nms(candidates, NMS_IOU_THRESHOLD)
        .into_iter()
        .map(|d| {
            let x1 = (d.bbox.0 / r).clamp(0.0, 1.0);
            let y1 = (d.bbox.1 / r).clamp(0.0, 1.0);
            let x2 = (d.bbox.2 / r).clamp(0.0, 1.0);
            let y2 = (d.bbox.3 / r).clamp(0.0, 1.0);
            FaceBox {
                bbox: [x1, y1, (x2 - x1).max(0.0), (y2 - y1).max(0.0)],
                score: d.score,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Synthetic {
        cls: Vec<Vec<f32>>,
        obj: Vec<Vec<f32>>,
        bbox: Vec<Vec<f32>>,
    }

    impl Synthetic {
        fn new() -> Self {
            let n = |s: u32| ((RESOLUTION / s) * (RESOLUTION / s)) as usize;
            Self {
                cls: STRIDES.iter().map(|&s| vec![0.0; n(s)]).collect(),
                obj: STRIDES.iter().map(|&s| vec![0.0; n(s)]).collect(),
                bbox: STRIDES.iter().map(|&s| vec![0.0; 4 * n(s)]).collect(),
            }
        }

        /// Encodes a face centred at `(cx, cy)` of `w`×`h` input px in cell
        /// `(gx, gy)` of stride group `k`.
        #[allow(clippy::too_many_arguments)]
        fn put(&mut self, k: usize, gx: u32, gy: u32, b: (f32, f32, f32, f32), cls: f32, obj: f32) {
            let s = STRIDES[k] as f32;
            let i = (gy * (RESOLUTION / STRIDES[k]) + gx) as usize;
            self.cls[k][i] = cls;
            self.obj[k][i] = obj;
            self.bbox[k][i * 4..i * 4 + 4].copy_from_slice(&[
                b.0 / s - gx as f32,
                b.1 / s - gy as f32,
                (b.2 / s).ln(),
                (b.3 / s).ln(),
            ]);
        }

        fn groups(&self) -> Vec<StrideOutputs<'_>> {
            STRIDES
                .iter()
                .enumerate()
                .map(|(k, &stride)| StrideOutputs {
                    stride,
                    cls: &self.cls[k],
                    obj: &self.obj[k],
                    bbox: &self.bbox[k],
                })
                .collect()
        }
    }

    #[test]
    fn decode_places_faces_on_each_stride_and_scores_by_geometric_mean() {
        let mut o = Synthetic::new();
        o.put(0, 10, 20, (84.0, 164.0, 24.0, 30.0), 0.81, 1.0);
        o.put(1, 30, 5, (488.0, 88.0, 60.0, 70.0), 0.9, 0.9);
        o.put(2, 3, 16, (120.0, 520.0, 150.0, 180.0), 1.2, 0.64); // cls clamped to 1
        o.put(0, 50, 50, (404.0, 404.0, 20.0, 20.0), 0.2, 0.9); // sqrt(0.18) < 0.5
        let mut faces = decode_yunet(&o.groups(), FACE_SCORE_THRESHOLD);
        faces.sort_by(|a, b| a.bbox[1].total_cmp(&b.bbox[1]));
        assert_eq!(faces.len(), 3, "{faces:?}");
        let r = RESOLUTION as f32;
        let want = [
            ([458.0, 53.0, 60.0, 70.0], 0.9),
            ([72.0, 149.0, 24.0, 30.0], 0.9),
            ([45.0, 430.0, 150.0, 180.0], 0.8),
        ];
        for (f, (b, score)) in faces.iter().zip(want) {
            for k in 0..4 {
                assert!((f.bbox[k] - b[k] / r).abs() < 1e-4, "{:?} vs {b:?}", f.bbox);
            }
            assert!((f.score - score).abs() < 1e-5, "score {}", f.score);
        }
    }

    #[test]
    fn decode_suppresses_overlapping_faces_at_iou_0_3() {
        let mut o = Synthetic::new();
        o.put(0, 10, 10, (84.0, 84.0, 40.0, 40.0), 0.95, 0.95);
        // Same-size boxes shifted by d px: IoU = (40-d)/(40+d). d = 12 → 0.54
        // and d = 20 → 0.33 are suppressed, d = 24 → 0.25 is kept.
        o.put(0, 11, 10, (96.0, 84.0, 40.0, 40.0), 0.9, 0.9);
        o.put(0, 12, 10, (104.0, 84.0, 40.0, 40.0), 0.9, 0.9);
        o.put(0, 13, 11, (108.0, 84.0, 40.0, 40.0), 0.8, 0.8);
        let faces = decode_yunet(&o.groups(), FACE_SCORE_THRESHOLD);
        assert_eq!(faces.len(), 2, "{faces:?}");
        assert!((faces[0].score - 0.95).abs() < 1e-5);
        assert!((faces[1].score - 0.8).abs() < 1e-5);
    }

    /// Reference faces of YuNet on the Ultralytics `bus.jpg` (score, xyxy
    /// frame px), from Python/onnxruntime with the pipeline's preprocessing:
    /// half-pixel bilinear to 1280×720, then stretched to 640 with the same
    /// non-antialiased bilinear as the CUDA kernel (an antialiased PIL resize
    /// gives 0.910 / 0.901 on the same boxes ±1 px).
    const REFERENCE_FACES: [(f32, [f32; 4]); 2] = [
        (0.907, [184.0, 276.0, 242.0, 315.0]),
        (0.903, [426.0, 278.0, 485.0, 319.0]),
    ];

    /// Real model on a real photo through the GPU preprocess and the
    /// device-input entry point. Needs `face_detection_yunet_2023mar.onnx` in
    /// `vision_models_dir()`, CUDA and `TENTAFLOW_YOLO_TEST_IMAGE`.
    #[cfg(all(
        any(target_os = "linux", target_os = "windows"),
        feature = "vision-cuda-preprocess"
    ))]
    #[test]
    #[ignore = "needs CUDA, the YuNet model and TENTAFLOW_YOLO_TEST_IMAGE"]
    fn yunet_matches_python_reference_on_real_image() {
        use crate::vision::gpu_preprocess::{
            preprocess_nv12_device_into, GpuStream, OwnedDeviceTensor,
        };
        use crate::vision::test_frames::{bus_frame_1280x720, DeviceNv12};

        let (_, nv12) = bus_frame_1280x720();
        let (w, h) = (nv12.w as f32, nv12.h as f32);
        let dev = DeviceNv12::upload(&nv12);
        let det = YuNetDetector::load().expect("load YuNet");
        let stream = GpuStream::new().unwrap();
        let mut input = OwnedDeviceTensor::alloc(RESOLUTION as usize).unwrap();
        let mut run = || {
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
            unsafe { det.detect_device_input(input.device_ptr(), FACE_SCORE_THRESHOLD) }.unwrap()
        };
        for _ in 0..20 {
            run();
        }
        let mut times = Vec::new();
        let mut faces = Vec::new();
        for _ in 0..200 {
            let t = std::time::Instant::now();
            faces = run();
            times.push(t.elapsed().as_secs_f64() * 1e3);
        }
        times.sort_by(|a, b| a.total_cmp(b));
        eprintln!(
            "YuNet preprocess+forward+decode ms: p50={:.2} p95={:.2} (pool={})",
            times[times.len() / 2],
            times[times.len() * 95 / 100],
            det.pool_size()
        );
        let px = |f: &FaceBox| {
            [
                f.bbox[0] * w,
                f.bbox[1] * h,
                (f.bbox[0] + f.bbox[2]) * w,
                (f.bbox[1] + f.bbox[3]) * h,
            ]
        };
        faces.sort_by(|a, b| a.bbox[0].total_cmp(&b.bbox[0]));
        for f in &faces {
            eprintln!("face {:.3} {:?}", f.score, px(f).map(|v| v.round()));
        }
        assert_eq!(faces.len(), REFERENCE_FACES.len(), "{faces:?}");
        for (f, (score, bbox)) in faces.iter().zip(REFERENCE_FACES) {
            assert!(
                (f.score - score).abs() < 0.03,
                "score {} vs {score}",
                f.score
            );
            for (got, want) in px(f).iter().zip(bbox) {
                assert!((got - want).abs() < 5.0, "{:?} vs {bbox:?}", px(f));
            }
        }
    }
}
