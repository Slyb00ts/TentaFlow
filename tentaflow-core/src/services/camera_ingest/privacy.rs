// =============================================================================
// File: services/camera_ingest/privacy.rs — GPU privacy probe
// =============================================================================
//
// Anonymizes a camera's frames on the GPU before ANY consumer sees them: the
// probe sits on the decoder side of the CUDA tee (`webrtc_source.rs`), so the
// live view, recordings, snapshots, depth mapping and analysis all read the
// processed pixels, and no unprocessed copy exists anywhere.
//
// Per frame, synchronously on the streaming thread (a few ms on GB10 against a
// 40 ms frame budget at 25 fps):
//   1. map the NVDEC surface in place (device NV12, no download),
//   2. run BOTH detectors on THIS frame — YOLOX-tiny for persons and YuNet for
//      faces — so a person entering the picture is covered in the very frame
//      they appear (no prediction, no delay),
//   3. pixelate every face, every person's head region (top of the box +
//      margin: covers heads seen from behind or too small for the face model)
//      and the regions of the previous frame (extra cover at detection edges),
//   4. publish the persons to the detection bus when detection is enabled.
// Two independent detectors are two nets: a face right in front of the lens
// whose body is out of frame is caught by YuNet, a head turned away by YOLOX.
//
// Fail-closed: when a frame cannot be analysed by both detectors (a model not
// installed yet, a forward error) the WHOLE frame is pixelated; when it cannot
// even be mapped it is dropped. A frame never leaves unprocessed. The GPU path is the only one — a
// host without it refuses the camera (`webrtc_source::link_privacy_branch`).
//
// Head boxes and person boxes are personal data: nothing here logs them, only
// counts, timings and ids.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_video as gst_video;

use super::fakefile::{nv12_color_from_info, FrameCounters};
use super::gst_cuda_ffi::map_nv12_device;
use super::privacy_options::PrivacyOptions;
use crate::services::detection_bus::{self, Detection, MotionSignal};
use crate::vision::gpu_preprocess::{
    mosaic_nv12_device, preprocess_nv12_device_into, wait_for_decoded_surface, BlurRect,
    ColorCoeffs, GpuStream, MosaicMode, MosaicScratch, Nv12DevicePlanes, OwnedDeviceTensor,
    MOSAIC_MAX_RECTS,
};
use crate::vision::runners::{get_coco_detector, get_face_detector, CocoHandle, FaceHandle};
use crate::vision::{detector_coco, face_yunet};

/// Head = the top fraction of a person box. A standing person's head is the top
/// ~15 %; 30 % also covers bowed heads and people seen from a low robot camera.
const HEAD_FRACTION: f32 = 0.30;
/// Margin added on every side of a head or face region, as a fraction of its size.
const REGION_MARGIN: f32 = 0.25;
/// Smallest region in luma pixels: a distant face must still vanish.
const REGION_MIN_PX: f32 = 24.0;
/// Person score the probe accepts. Lower than the detector's default
/// (`PERSON_SCORE_THRESHOLD`, tuned for clean detections): here a missed person
/// is a leaked face, while a false positive only pixelates a patch of scenery.
const PRIVACY_PERSON_SCORE: f32 = 0.2;
/// Period of the per-camera summary log line.
const SUMMARY_EVERY_FRAMES: u64 = 500;

/// Install the probe on `pad` (the src pad in front of the CUDA tee). The
/// detectors are resolved from the frames through the runtime captured here,
/// because the probe runs on a GStreamer streaming thread outside tokio.
pub fn install_privacy_probe(
    pad: &gst::Pad,
    camera_id: &str,
    options: PrivacyOptions,
    source_fps: u32,
    counters: Arc<FrameCounters>,
) -> Result<()> {
    let runtime = tokio::runtime::Handle::try_current()
        .context("privacy probe must be installed from within the tokio runtime")?;
    let state = Mutex::new(ProbeState {
        camera_id: camera_id.to_string(),
        options,
        frame_budget_ms: 1000.0 / source_fps.max(1) as f32,
        runtime,
        coco: None,
        faces: None,
        gpu: None,
        prev_regions: Vec::new(),
        counters,
        stats: ProbeStats::default(),
    });
    pad.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
        let Some(buffer) = info.buffer() else {
            return gst::PadProbeReturn::Ok;
        };
        let Some(video_info) = pad
            .current_caps()
            .and_then(|caps| gst_video::VideoInfo::from_caps(&caps).ok())
        else {
            // Caps are set before the first buffer; a buffer without them cannot
            // be interpreted, so it cannot be processed either.
            return gst::PadProbeReturn::Drop;
        };
        let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
        match state.process(buffer, &video_info) {
            Ok(()) => gst::PadProbeReturn::Ok,
            Err(e) => {
                state.stats.dropped += 1;
                if state.stats.dropped == 1 || state.stats.dropped % SUMMARY_EVERY_FRAMES == 0 {
                    tracing::warn!(
                        camera_id = %state.camera_id,
                        dropped = state.stats.dropped,
                        "privacy probe: frame dropped, it could not be processed: {e:#}"
                    );
                }
                gst::PadProbeReturn::Drop
            }
        }
    });
    Ok(())
}

/// GPU resources of one probe, created on the first frame (the mosaic scratch
/// is sized from the frame).
struct GpuResources {
    stream: GpuStream,
    coco_input: OwnedDeviceTensor,
    face_input: OwnedDeviceTensor,
    mosaic: MosaicScratch,
}

#[derive(Default)]
struct ProbeStats {
    frames: u64,
    whole_frame: u64,
    dropped: u64,
    analysis_errors: u64,
    work_ms_total: f64,
}

struct ProbeState {
    camera_id: String,
    options: PrivacyOptions,
    frame_budget_ms: f32,
    runtime: tokio::runtime::Handle,
    /// Resolved detectors. `None` until a load succeeded; asked again on later
    /// frames (the loaders back off between attempts) so a model installed after
    /// the camera started is picked up without a restart.
    coco: Option<CocoHandle>,
    faces: Option<FaceHandle>,
    gpu: Option<GpuResources>,
    prev_regions: Vec<BlurRect>,
    counters: Arc<FrameCounters>,
    stats: ProbeStats,
}

impl Drop for ProbeState {
    fn drop(&mut self) {
        super::tracker::remove(&self.camera_id);
    }
}

impl ProbeState {
    fn process(&mut self, buffer: &gst::BufferRef, video_info: &gst_video::VideoInfo) -> Result<()> {
        let started = Instant::now();
        let ts_ms = unix_ms();
        self.counters.increment_public(ts_ms / 1000);

        let (width, height) = (video_info.width(), video_info.height());
        if self.gpu.is_none() {
            self.gpu = Some(GpuResources {
                stream: GpuStream::new()?,
                coco_input: OwnedDeviceTensor::alloc(detector_coco::RESOLUTION as usize)?,
                face_input: OwnedDeviceTensor::alloc(face_yunet::RESOLUTION as usize)?,
                mosaic: MosaicScratch::new(width, height)?,
            });
        }
        self.resolve_detectors();
        let detectors = self.coco.clone().zip(self.faces.clone());
        let gpu = self.gpu.as_mut().expect("initialized above");

        let map = map_nv12_device(buffer, video_info, self.options.face_blur)
            .map_err(|e| anyhow!("map NVDEC surface: {e}"))?;
        let planes = Nv12DevicePlanes {
            y_ptr: map.y_device_ptr(),
            y_stride: map.y_stride(),
            uv_ptr: map.uv_device_ptr(),
            uv_stride: map.uv_stride(),
            w: width,
            h: height,
        };
        wait_for_decoded_surface()?;

        let color = {
            let (kr, kb, full_range) = nv12_color_from_info(video_info);
            ColorCoeffs { kr, kb, full_range }
        };
        let analysis = detectors.and_then(|(coco, faces)| {
            analyse(&coco, &faces, gpu, planes, color)
                .map_err(|e| {
                    self.stats.analysis_errors += 1;
                    if self.stats.analysis_errors == 1
                        || self.stats.analysis_errors % SUMMARY_EVERY_FRAMES == 0
                    {
                        tracing::warn!(
                            camera_id = %self.camera_id,
                            errors = self.stats.analysis_errors,
                            "privacy probe: analysis failed, frame pixelated whole: {e:#}"
                        );
                    }
                })
                .ok()
        });

        if self.options.face_blur {
            let mode = match analysis.as_ref() {
                Some(found) => {
                    // Faces FIRST, heads after: where the two overlap the later
                    // region wins, and the head band is both larger and coarser
                    // — a face must never end up less pixelated than the head
                    // around it.
                    let current: Vec<BlurRect> = found
                        .faces
                        .iter()
                        .map(|f| face_region(f.bbox, width, height))
                        .chain(
                            found
                                .persons
                                .iter()
                                .map(|p| head_region(p.bbox, width, height)),
                        )
                        .collect();
                    let mut regions = current.clone();
                    regions.extend_from_slice(&self.prev_regions);
                    self.prev_regions = current;
                    if regions.is_empty() {
                        None
                    } else if regions.len() > MOSAIC_MAX_RECTS {
                        Some(OwnedMode::WholeFrame)
                    } else {
                        Some(OwnedMode::Regions(regions))
                    }
                }
                None => {
                    self.prev_regions.clear();
                    Some(OwnedMode::WholeFrame)
                }
            };
            if let Some(mode) = mode {
                if matches!(mode, OwnedMode::WholeFrame) {
                    self.stats.whole_frame += 1;
                }
                mosaic_nv12_device(planes, mode.as_mode(), &mut gpu.mosaic, &gpu.stream)?;
                gpu.stream.synchronize()?;
            }
        }
        drop(map);

        let work_ms = started.elapsed().as_secs_f64() * 1000.0;
        if self.options.person_detect {
            if let Some(Analysis { mut persons, .. }) = analysis {
                let pts_ns = buffer.pts().map(|t| t.nseconds());
                let key = super::tracker::key(&self.camera_id, "persons");
                super::tracker::update(&key, &mut persons, pts_ns);
                detection_bus::publish_detections(
                    &self.camera_id,
                    ts_ms,
                    pts_ns,
                    work_ms.round() as u32,
                    false,
                    persons,
                    MotionSignal::default(),
                );
            }
        }
        self.record(work_ms);
        Ok(())
    }

    /// Ask the shared loaders for any detector not resolved yet. Each call is
    /// cheap once loaded or while the loader backs off after a failure.
    fn resolve_detectors(&mut self) {
        if self.coco.is_none() {
            self.coco = self.runtime.block_on(get_coco_detector());
        }
        if self.faces.is_none() {
            self.faces = self.runtime.block_on(get_face_detector());
        }
    }

    fn record(&mut self, work_ms: f64) {
        self.stats.frames += 1;
        self.stats.work_ms_total += work_ms;
        if self.stats.frames % SUMMARY_EVERY_FRAMES == 0 {
            let avg = self.stats.work_ms_total / SUMMARY_EVERY_FRAMES as f64;
            tracing::info!(
                camera_id = %self.camera_id,
                frames = self.stats.frames,
                whole_frame = self.stats.whole_frame,
                dropped = self.stats.dropped,
                avg_ms = format!("{avg:.2}"),
                budget_ms = format!("{:.1}", self.frame_budget_ms),
                "privacy probe summary"
            );
            self.stats.work_ms_total = 0.0;
        }
    }
}

/// What to pixelate in this frame, owning its regions (the head lists it was
/// built from are moved into the probe state).
enum OwnedMode {
    Regions(Vec<BlurRect>),
    WholeFrame,
}

impl OwnedMode {
    fn as_mode(&self) -> MosaicMode<'_> {
        match self {
            Self::Regions(r) => MosaicMode::Regions(r),
            Self::WholeFrame => MosaicMode::WholeFrame,
        }
    }
}

/// What both detectors found in one frame (boxes normalized `[x, y, w, h]`).
struct Analysis {
    persons: Vec<Detection>,
    faces: Vec<face_yunet::FaceBox>,
}

/// Run both detectors on the frame in place: each model's input is prepared on
/// the probe's stream in its own layout (YOLOX letterboxed at 416, YuNet
/// stretched at 640, both BGR 0..255), the stream is synchronized (ORT reads on
/// its own stream), then the blocking forwards run on the shared pools.
fn analyse(
    coco: &CocoHandle,
    faces: &FaceHandle,
    gpu: &mut GpuResources,
    planes: Nv12DevicePlanes,
    color: ColorCoeffs,
) -> Result<Analysis> {
    let content = preprocess_nv12_device_into(
        planes,
        &mut gpu.coco_input,
        detector_coco::INPUT_MEAN,
        detector_coco::INPUT_STD,
        detector_coco::INPUT_FIT,
        detector_coco::INPUT_ORDER,
        color,
        &gpu.stream,
    )?;
    preprocess_nv12_device_into(
        planes,
        &mut gpu.face_input,
        face_yunet::INPUT_MEAN,
        face_yunet::INPUT_STD,
        face_yunet::INPUT_FIT,
        face_yunet::INPUT_ORDER,
        color,
        &gpu.stream,
    )?;
    gpu.stream.synchronize()?;
    // SAFETY: both inputs are live device-0 tensors of the models' input shapes,
    // owned by this probe, and the stream sync above completed every write.
    let persons = unsafe {
        coco.detect_device_input(
            gpu.coco_input.device_ptr(),
            content,
            detector_coco::PERSON_CLASSES,
            PRIVACY_PERSON_SCORE,
        )
    }?;
    let faces = unsafe {
        faces.detect_device_input(gpu.face_input.device_ptr(), face_yunet::FACE_SCORE_THRESHOLD)
    }?;
    Ok(Analysis { persons, faces })
}

/// Head region of a person box (`bbox` normalized `[x, y, w, h]`) in luma
/// pixels: the top `HEAD_FRACTION` of the box, widened by `REGION_MARGIN`.
pub(super) fn head_region(bbox: [f32; 4], width: u32, height: u32) -> BlurRect {
    expanded_region([bbox[0], bbox[1], bbox[2], bbox[3] * HEAD_FRACTION], width, height)
}

/// A detected face widened by `REGION_MARGIN` (the detector's box is tight;
/// hair, ears and a jaw at the edge still identify a person).
fn face_region(bbox: [f32; 4], width: u32, height: u32) -> BlurRect {
    expanded_region(bbox, width, height)
}

/// `bbox` (normalized `[x, y, w, h]`) in luma pixels, widened by `REGION_MARGIN`
/// on every side and never smaller than `REGION_MIN_PX`. Sizes round up — a
/// region may only err towards covering more. The mosaic clamps to the frame.
fn expanded_region(bbox: [f32; 4], width: u32, height: u32) -> BlurRect {
    let (fw, fh) = (width as f32, height as f32);
    let (x, y, w, h) = (bbox[0] * fw, bbox[1] * fh, bbox[2] * fw, bbox[3] * fh);
    let rw = (w * (1.0 + 2.0 * REGION_MARGIN)).max(REGION_MIN_PX);
    let rh = (h * (1.0 + 2.0 * REGION_MARGIN)).max(REGION_MIN_PX);
    let x0 = (x + w / 2.0 - rw / 2.0).max(0.0);
    let y0 = (y + h / 2.0 - rh / 2.0).max(0.0);
    BlurRect {
        x: x0 as u32,
        y: y0 as u32,
        w: rw.ceil() as u32,
        h: rh.ceil() as u32,
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_region_covers_the_top_of_the_box_with_margin() {
        // A 100×400 px person at (500, 100) in a 1280×720 frame.
        let r = head_region([500.0 / 1280.0, 100.0 / 720.0, 100.0 / 1280.0, 400.0 / 720.0], 1280, 720);
        // Head band = top 120 px; with 25 % margin each side: 180 px tall, 150 px wide
        // (sizes round UP — a region may only err towards covering more).
        assert!((150..=151).contains(&r.w) && (180..=181).contains(&r.h), "{r:?}");
        assert_eq!((r.x, r.y), (475, 70));
        // The band starts above the box top and reaches past the head band.
        assert!(r.y < 100 && r.y + r.h > 100 + 120);
    }

    #[test]
    fn tiny_or_edge_boxes_still_get_a_minimum_region_inside_the_frame() {
        let r = head_region([0.0, 0.0, 2.0 / 1280.0, 3.0 / 720.0], 1280, 720);
        assert_eq!((r.x, r.y), (0, 0));
        assert!(r.w >= REGION_MIN_PX as u32 && r.h >= REGION_MIN_PX as u32);
    }
}
