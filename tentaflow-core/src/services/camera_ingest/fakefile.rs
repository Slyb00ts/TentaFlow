// =============================================================================
// File: services/camera_ingest/fakefile.rs — GStreamer-backed FakeFile connector
// =============================================================================
//
// Builds a GStreamer pipeline of the form:
//     filesrc location=<path> ! decodebin ! videoconvert
//       ! video/x-raw,format=RGB ! appsink name=sink
//
// Decoded RGB24 frames are pushed into a single-slot mailbox (latest-wins) so
// downstream consumers (snapshots, future stream bus) always see the freshest
// frame without buffering arbitrary backlog. On EOS the pipeline seeks back to
// position 0 to provide a continuous replay loop.

use std::path::Path;
use std::sync::Arc;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::prelude::VideoFrameExt;
use parking_lot::Mutex;

use super::error::{CameraIngestError, Result};
use crate::services::frame_storage::{FrameMetadata, FramePixelFormat, StoredFrame};
use crate::services::{frame_storage, streaming_bus};

/// Single-slot latest-frame mailbox. New frames overwrite older ones — we are
/// deliberately discarding frames a slow consumer would otherwise buffer.
#[derive(Debug, Clone)]
pub struct LatestFrame {
    pub width: u32,
    pub height: u32,
    pub timestamp_unix_ms: u64,
    /// PTS klatki w osi mediów (nanosekundy) — propagowany z appsink do detekcji.
    pub pts_ns: Option<u64>,
    pub data: Arc<[u8]>,
    /// Pixel layout of `data`. `Rgb24` (tightly-packed, stride `width*3`) for
    /// every non-NVDEC producer (the historical default). `Nv12` ONLY on the
    /// GPU-resident NVDEC path (Stage 3): `data` holds packed `[Y | UV]` planes
    /// so enrichment crops are cut with [`crop_nv12`] and the full 4K NV12→RGB
    /// `videoconvert` never runs on the analysis hot path. Additive — every
    /// consumer that needs RGB (snapshots, streaming) converts on demand.
    pub format: DetectFrameFormat,
    /// Zero-copy CROPS path (`[vision] zerocopy_crops`) ONLY: a DEVICE reference
    /// to the latest NVDEC NV12 surface (the `gst::Sample` ref-holds ONE decode
    /// surface + carries its device geometry/colorimetry). When `Some`, `data` is
    /// EMPTY: the full 4K NV12 was NOT downloaded to host this frame. Enrichment
    /// cuts each detection's small crop straight off the device surface
    /// ([`DeviceCropsFrame::crop_detection_rgb`]); snapshots / on-demand display
    /// download the full frame lazily ([`DeviceCropsFrame::download_full_nv12`]).
    ///
    /// LIFETIME: the mailbox holds only the LATEST device frame — `put` replaces
    /// the previous, dropping its `gst::Sample` and returning that surface to the
    /// decoder pool, so at most ONE surface is pinned per camera by the mailbox.
    /// A consumer that wants the frame maps it on demand while its `Sample` is
    /// still held; if it was already replaced the consumer has its own clone (the
    /// ref keeps the surface alive) or falls back. `None` on every other path.
    pub device: Option<DeviceCropsFrame>,
}

/// A device-resident NV12 crops frame: the NVDEC decode surface kept alive via
/// its `gst::Sample` (which ref-holds ONE surface out of the decoder's finite
/// pool), plus the frame geometry + YUV→RGB coefficients needed to crop/convert
/// on demand WITHOUT re-reading the caps. Cheaply clonable (the `Sample` is
/// refcounted) — a clone extends the surface's lifetime, so a cold-path event
/// that holds a clone can still map it even after the mailbox moved on.
///
/// SURFACE-POOL BOUND: the mailbox holds ≤1 (latest); a cold enrichment event in
/// flight holds ≤1 more (in-flight) plus ≤1 pending per camera. So a single
/// camera pins at most ~3 decode surfaces — far under a decoder pool — and every
/// clone drops its ref as soon as the consumer finishes, releasing the surface.
#[derive(Clone)]
pub struct DeviceCropsFrame {
    /// Ref-holds the decode surface; `Send + Sync` (thread-safe GstSample) so it
    /// rides the snapshot round-trip and the cold-path channel.
    sample: gst::Sample,
    pub width: u32,
    pub height: u32,
    pub kr: f32,
    pub kb: f32,
    pub full_range: bool,
}

impl std::fmt::Debug for DeviceCropsFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceCropsFrame")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl DeviceCropsFrame {
    /// The `DetectFrameFormat::Nv12` tag matching this frame's colorimetry, for
    /// consumers that materialize host NV12 bytes and need the format alongside.
    fn nv12_format(&self, y_stride: u32, uv_stride: u32, uv_offset: u32) -> DetectFrameFormat {
        DetectFrameFormat::Nv12 {
            y_stride,
            uv_stride,
            y_offset: 0,
            uv_offset,
            kr: self.kr,
            kb: self.kb,
            full_range: self.full_range,
        }
    }

    /// Downloads the FULL NV12 frame to host ON DEMAND (snapshot / on-demand
    /// display / analysis-flow image blob) as packed `[Y | UV]` + its NV12 format
    /// tag. Uses the readable GstMemory map, which for a `GstCudaMemory` copies
    /// device→host — so this is the same host frame the per-frame download path
    /// would have produced, but paid ONLY when a frame is actually consumed as a
    /// whole (never per stream frame). `None` if the buffer can't be mapped
    /// (surface gone / non-2-plane) — the caller then has no host frame.
    pub fn download_full_nv12(&self) -> Option<(Arc<[u8]>, DetectFrameFormat)> {
        let caps = self.sample.caps()?;
        let info = gst_video::VideoInfo::from_caps(caps).ok()?;
        let buffer = self.sample.buffer()?;
        let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info).ok()?;
        let y = frame.plane_data(0).ok()?;
        let uv = frame.plane_data(1).ok()?;
        let strides = frame.plane_stride();
        let y_stride = *strides.first()? as u32;
        let uv_stride = *strides.get(1)? as u32;
        let mut buf = Vec::with_capacity(y.len() + uv.len());
        buf.extend_from_slice(y);
        buf.extend_from_slice(uv);
        let uv_offset = y.len() as u32;
        let data: Arc<[u8]> = Arc::from(buf);
        Some((data, self.nv12_format(y_stride, uv_stride, uv_offset)))
    }
}

#[cfg(all(feature = "inference-vision-gpu", feature = "inference-supertonic"))]
impl DeviceCropsFrame {
    /// Maps the device NV12 surface and returns its device plane pointers/strides.
    /// The returned map borrows `self.sample`'s buffer (kept alive by our held
    /// ref); it unmaps on drop.
    fn map_device(&self) -> Option<super::gst_cuda_ffi::CudaNv12Map<'_>> {
        let caps = self.sample.caps()?;
        let info = gst_video::VideoInfo::from_caps(caps).ok()?;
        let buffer = self.sample.buffer()?;
        super::gst_cuda_ffi::map_nv12_device(buffer, &info).ok()
    }

    /// Cuts ONE detection's crop straight off the device NV12 surface: downloads
    /// only the (even-snapped, clamped) crop sub-rectangle to host
    /// ([`crate::vision::gpu_preprocess::download_nv12_crop_rect`]) and runs the
    /// SAME host [`crop_nv12`] on it at origin `(0, 0)`, so the RGB24 result is
    /// bit-identical to cropping the full host frame — but only the crop's bytes
    /// (~KB) cross the bus, never the 4K frame. Returns `(rgb, cw, ch)` with the
    /// possibly even-snapped dims, matching [`crop_nv12`]'s contract. `None` on
    /// map / download failure (caller falls back).
    pub fn crop_detection_rgb(
        &self,
        x0: u32,
        y0: u32,
        cw: u32,
        ch: u32,
    ) -> Option<(Vec<u8>, u32, u32)> {
        use crate::vision::gpu_preprocess::{download_nv12_crop_rect, Nv12DevicePlanes};
        let map = self.map_device()?;
        let planes = Nv12DevicePlanes {
            y_ptr: map.y_device_ptr(),
            y_stride: map.y_stride(),
            uv_ptr: map.uv_device_ptr(),
            uv_stride: map.uv_stride(),
            w: map.width(),
            h: map.height(),
        };
        let sub = download_nv12_crop_rect(planes, x0, y0, cw, ch).ok()?;
        drop(map);
        // Reuse the parity-verified host crop on the small sub-frame (origin 0,0).
        let (rgb, _, _, ecw, ech) = crop_nv12(
            &sub.data,
            sub.width,
            sub.height,
            sub.y_stride,
            sub.uv_stride,
            0,
            sub.uv_offset,
            self.kr,
            self.kb,
            self.full_range,
            0,
            0,
            sub.width,
            sub.height,
        );
        Some((rgb, ecw, ech))
    }
}

/// Square side the detect branch GPU-scales frames to. MUST equal
/// `vision::detector_rfdetr::RESOLUTION` (560) so the pre-scaled frame hits the
/// detector's `fill_frame` copy fast-path. Duplicated (not referenced) because
/// `detector_rfdetr` lives behind `inference-vision-gpu` while camera ingest is
/// behind `camera` — a direct reference would break camera-only builds.
pub(super) const DETECT_RESIZE_DIM: u32 = 560;

/// Pixel layout of a [`DetectFrame`]. The detect frame is analysis-only input;
/// its format is INDEPENDENT of the crops/display frame (always RGB24).
///   * `Rgb24` — tightly-packed RGB24 (stride `width*3`): the GPU-scaled 560×560
///     frame or the crops frame the detector CPU-resizes (historical default).
///   * `Nv12` — raw NVDEC output (4:2:0): a Y plane at `y_offset` (stride
///     `y_stride`) followed by an interleaved UV plane at `uv_offset` (stride
///     `uv_stride`) inside `DetectFrame::data`. `kr`/`kb`/`full_range` are the
///     YUV→RGB coefficients read from the frame colorimetry (default BT.709
///     limited). The detector converts + resizes it on the GPU via
///     `detect_batch_gpu`, so the CPU never touches this frame.
#[derive(Debug, Clone, Copy)]
pub enum DetectFrameFormat {
    Rgb24,
    Nv12 {
        y_stride: u32,
        uv_stride: u32,
        y_offset: u32,
        uv_offset: u32,
        kr: f32,
        kb: f32,
        full_range: bool,
    },
}

impl Default for DetectFrameFormat {
    fn default() -> Self {
        Self::Rgb24
    }
}

/// Detect frame delivered alongside the full-res crops [`LatestFrame`]. For the
/// RGB path this is the GPU-scaled 560×560 frame (detect branch
/// `cudaupload → cudascale → cudadownload`) so the detector skips the ~4 ms CPU
/// resize; for the GPU-resident NV12 path it is the raw full-res NV12 frame the
/// detector converts + resizes on the GPU. `format` tells the two apart.
/// Enrichment crops still read the full-res [`LatestFrame`] via [`FrameMailbox::get`].
#[derive(Debug, Clone)]
pub struct DetectFrame {
    pub width: u32,
    pub height: u32,
    pub pts_ns: Option<u64>,
    pub data: Arc<[u8]>,
    pub format: DetectFrameFormat,
    /// Zero-copy (Stage 4) ONLY: an already-preprocessed `[1,3,560,560]` device
    /// tensor produced directly from the NVDEC decode surface (no host download).
    /// When `Some`, the analysis loop runs ORT on it via
    /// `detect_device_tensor` and IGNORES `data`/`format`. `None` on every other
    /// path (the detector reads `data`/`format`). Type-erased (`Arc<dyn Any>`) so
    /// this field needs no CUDA feature gate at the many construction sites; the
    /// detect path downcasts it to `gpu_preprocess::OwnedDeviceTensor`.
    pub device: Option<DeviceDetectTensor>,
}

/// Opaque, cheaply-clonable handle to an owned GPU device tensor
/// (`gpu_preprocess::OwnedDeviceTensor`) for the zero-copy detect path. Erased to
/// `Arc<dyn Any + Send + Sync>` so `DetectFrame` (and the snapshot round-trip it
/// rides) carry it without a CUDA-feature-gated field, and so the device buffer
/// is freed only when the LAST clone (mailbox slot + in-flight job) drops.
#[derive(Clone)]
pub struct DeviceDetectTensor(pub Arc<dyn std::any::Any + Send + Sync>);

impl std::fmt::Debug for DeviceDetectTensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceDetectTensor(<device>)")
    }
}

#[derive(Default)]
pub struct FrameMailbox {
    inner: Mutex<Option<LatestFrame>>,
    // Optional second slot: the GPU-scaled 560×560 detect frame. Latest-wins,
    // independent of the crops slot. Both slots are fed by callbacks off the
    // SAME post-decode tee, so they are near-synchronized — a live overlay does
    // not need PTS-exact pairing, only latest-of-each.
    detect: Mutex<Option<DetectFrame>>,
}

impl FrameMailbox {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&self, frame: LatestFrame) {
        *self.inner.lock() = Some(frame);
    }

    pub fn get(&self) -> Option<LatestFrame> {
        self.inner.lock().clone()
    }

    /// Store the latest GPU-scaled detect frame (detect branch callback).
    pub fn put_detect(&self, frame: DetectFrame) {
        *self.detect.lock() = Some(frame);
    }

    /// Latest detect frame if the detect branch is active; otherwise falls back
    /// to the full-res crops frame so detection ALWAYS has an input (the
    /// detector then CPU-resizes it — slower, but correct). Returns `None` only
    /// when no frame of any kind has landed yet.
    pub fn get_detect(&self) -> Option<DetectFrame> {
        if let Some(d) = self.detect.lock().clone() {
            return Some(d);
        }
        self.inner.lock().clone().map(|f| DetectFrame {
            width: f.width,
            height: f.height,
            pts_ns: f.pts_ns,
            data: f.data,
            // Fall back to the crops frame's own format: RGB24 (detector
            // CPU-resizes) on the usual path, or NV12 (detector does GPU
            // YUV→RGB+resize) on the GPU-resident crops path.
            format: f.format,
            device: None,
        })
    }
}

/// Counters updated from the appsink callback thread. Read by the session
/// loop to compute moving-average FPS and update `CameraHealth`.
#[derive(Default)]
pub struct FrameCounters {
    inner: Mutex<FrameCountersInner>,
}

#[derive(Default, Clone, Copy)]
struct FrameCountersInner {
    pub frames_total: u64,
    pub frames_dropped: u64,
    pub last_frame_at_unix_s: Option<u64>,
}

impl FrameCounters {
    pub fn new() -> Self {
        Self::default()
    }

    fn increment(&self, ts_unix_s: u64) {
        let mut g = self.inner.lock();
        g.frames_total = g.frames_total.saturating_add(1);
        g.last_frame_at_unix_s = Some(ts_unix_s);
    }

    /// Crate-visible alias for the private `increment` so sibling connector
    /// modules (rtsp.rs, future onvif.rs) can publish frame counts through
    /// the same primitive without re-implementing the lock dance.
    pub(crate) fn increment_public(&self, ts_unix_s: u64) {
        self.increment(ts_unix_s);
    }

    pub fn snapshot(&self) -> (u64, u64, Option<u64>) {
        let g = self.inner.lock();
        (g.frames_total, g.frames_dropped, g.last_frame_at_unix_s)
    }
}

/// Initialize GStreamer once. Safe to call multiple times; `gst::init` is
/// idempotent and guarded internally with a `std::sync::Once`.
pub fn ensure_gst_initialized() -> Result<()> {
    crate::services::gstreamer_runtime::prepare_runtime_environment();
    gst::init().map_err(|e| CameraIngestError::GstInit(e.to_string()))
}

/// Resolve the user-supplied URL into a concrete on-disk path. Rejects
/// symlinks and non-files. We strip the `file://` prefix if present and then
/// require an existing regular file.
pub fn resolve_file_url(url: &str) -> Result<std::path::PathBuf> {
    let raw = url.strip_prefix("file://").unwrap_or(url);
    if raw.is_empty() {
        return Err(CameraIngestError::InvalidUrl(url.to_string()));
    }
    let p = Path::new(raw);
    check_no_symlinks_in_path(p)?;
    let meta = std::fs::symlink_metadata(p)
        .map_err(|_| CameraIngestError::FileNotFound(raw.to_string()))?;
    if meta.file_type().is_symlink() {
        return Err(CameraIngestError::SymlinkNotAllowed(raw.to_string()));
    }
    if !meta.is_file() {
        return Err(CameraIngestError::FileNotFound(raw.to_string()));
    }
    p.canonicalize()
        .map_err(|_| CameraIngestError::FileNotFound(raw.to_string()))
}

/// Walk every component of `path` and reject if any intermediate component is
/// a symlink. `symlink_metadata` on the final path only checks the leaf; an
/// attacker could swap a parent directory for a symlink to escape the
/// intended subtree. We do this before `canonicalize` so the rejection
/// surfaces the offending component, not the resolved target.
fn check_no_symlinks_in_path(path: &Path) -> Result<()> {
    let mut current = std::path::PathBuf::new();
    for component in path.components() {
        current.push(component);
        // Root (`/`) and prefix components are never symlinks; skip cheaply
        // by only probing components that actually exist on disk.
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(CameraIngestError::SymlinkNotAllowed(
                        current.display().to_string(),
                    ));
                }
            }
            Err(_) => {
                // Non-existent intermediate component — leaf-existence check
                // in the caller will report FileNotFound consistently.
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Built pipeline + the appsink handle we wired callbacks onto. Kept together
/// because session.rs holds both during the loop iteration.
pub struct FakeFilePipeline {
    pub pipeline: gst::Pipeline,
    pub appsink: gst_app::AppSink,
}

/// Build a fake-file pipeline and wire the new-sample callback. The callback
/// publishes the most recent decoded RGB24 frame into `mailbox` and bumps
/// `counters`.
pub fn build_pipeline(
    file_path: &Path,
    camera_id: String,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<FakeFilePipeline> {
    let location = file_path
        .to_str()
        .ok_or_else(|| CameraIngestError::InvalidUrl(file_path.to_string_lossy().into_owned()))?;
    let loc = location.replace('"', "\\\"");

    // Stage-1 bench hook (`set_nv12_bench_mode`): tee the decoded
    // NV12 into a raw-NV12 detect appsink (mirrors the real NvdecNv12 tee) so
    // the GPU-resident detect path (`detect_batch_gpu`) is exercised end-to-end
    // WITHOUT a live camera. Default off → the exact single-appsink RGB pipeline.
    if nv12_detect_bench_enabled() {
        // Full NV12 mode (Stage 3, `--nv12`): the CROPS appsink also delivers raw
        // NV12 (no `videoconvert` — that full-frame NV12→RGB convert is the CPU
        // cost Stage 3 removes), so BOTH detect and enrichment run NV12 end to
        // end. Detect-only mode (`--nv12-detect`, Stage 1) keeps the RGB crops
        // branch so the Stage-1 regression stays byte-identical.
        let full = nv12_full_bench_enabled();
        let crops_branch = if full {
            "t. ! queue max-size-buffers=1 leaky=downstream ! \
             appsink name=sink emit-signals=false sync=true max-buffers=1 drop=true"
        } else {
            "t. ! queue max-size-buffers=1 leaky=downstream ! videoconvert ! video/x-raw,format=RGB ! \
             appsink name=sink emit-signals=false sync=true max-buffers=1 drop=true"
        };
        // NV12 tee → crops appsink (`sink`) + raw NV12 detect appsink (`detect`).
        let desc = format!(
            "filesrc location=\"{loc}\" ! decodebin ! videoconvert ! video/x-raw,format=NV12 ! tee name=t \
             {crops_branch} \
             t. ! queue max-size-buffers=1 leaky=downstream ! \
             appsink name=detect emit-signals=false sync=true max-buffers=1 drop=true"
        );
        let built = build_pipeline_from_description(
            &desc,
            camera_id.clone(),
            mailbox.clone(),
            counters.clone(),
        )?;
        // Full mode: swap the crops `sink` callback to the NV12 (mailbox-only)
        // one — `build_pipeline_from_description` installed the RGB callback.
        if full {
            let crops_sink = built
                .pipeline
                .by_name("sink")
                .ok_or_else(|| {
                    CameraIngestError::PipelineBuild("appsink named 'sink' missing".into())
                })?
                .downcast::<gst_app::AppSink>()
                .map_err(|_| CameraIngestError::PipelineBuild("'sink' is not AppSink".into()))?;
            install_frame_callback_nv12(&crops_sink, camera_id, mailbox.clone(), counters);
        }
        let detect_sink = built
            .pipeline
            .by_name("detect")
            .ok_or_else(|| {
                CameraIngestError::PipelineBuild("appsink named 'detect' missing".into())
            })?
            .downcast::<gst_app::AppSink>()
            .map_err(|_| CameraIngestError::PipelineBuild("'detect' is not AppSink".into()))?;
        install_detect_frame_callback_nv12(&detect_sink, mailbox);
        return Ok(built);
    }

    // Use parse_launch — concise, exactly mirrors the documented recipe and
    // returns a single Element we downcast to Pipeline.
    let desc = format!(
        "filesrc location=\"{loc}\" ! decodebin ! videoconvert ! video/x-raw,format=RGB ! appsink name=sink emit-signals=false sync=true max-buffers=1 drop=true"
    );
    build_pipeline_from_description(&desc, camera_id, mailbox, counters)
}

/// Bench-only NV12 gates for the fakefile connector, flipped programmatically
/// by `pipeline_bench` via [`set_nv12_bench_mode`] BEFORE any camera pipeline
/// builds. Both default off — production behavior is unchanged.
static NV12_DETECT_BENCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static NV12_FULL_BENCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Bench-only programmatic switch (called by the `pipeline_bench` example):
/// `detect` tees the decoded NV12 into a raw-NV12 detect appsink (Stage 1);
/// `full` additionally delivers the crops frame as raw NV12 (Stage 3, implies
/// the detect branch).
pub fn set_nv12_bench_mode(detect: bool, full: bool) {
    NV12_DETECT_BENCH.store(detect, std::sync::atomic::Ordering::Relaxed);
    NV12_FULL_BENCH.store(full, std::sync::atomic::Ordering::Relaxed);
}

/// Whether the fakefile connector should emit a raw-NV12 detect frame (Stage-1
/// bench gate, [`set_nv12_bench_mode`]); default off. Also requires the ort GPU
/// detect features — without them nothing consumes NV12 detect frames, so we
/// never produce one.
pub fn nv12_detect_bench_enabled() -> bool {
    cfg!(all(
        feature = "inference-vision-gpu",
        feature = "inference-supertonic"
    )) && (NV12_DETECT_BENCH.load(std::sync::atomic::Ordering::Relaxed)
        || nv12_full_bench_enabled())
}

/// Whether the fakefile connector should ALSO deliver the crops frame as raw
/// NV12 (Stage-3 bench gate, [`set_nv12_bench_mode`]); implies the NV12 detect
/// branch too, so `--nv12` drives detect + enrichment on NV12 end to end.
pub fn nv12_full_bench_enabled() -> bool {
    cfg!(all(
        feature = "inference-vision-gpu",
        feature = "inference-supertonic"
    )) && NV12_FULL_BENCH.load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn build_pipeline_from_description(
    desc: &str,
    camera_id: String,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<FakeFilePipeline> {
    let element =
        gst::parse::launch(&desc).map_err(|e| CameraIngestError::PipelineBuild(e.to_string()))?;
    let pipeline = element
        .downcast::<gst::Pipeline>()
        .map_err(|_| CameraIngestError::PipelineBuild("not a pipeline".into()))?;

    let appsink = pipeline
        .by_name("sink")
        .ok_or_else(|| CameraIngestError::PipelineBuild("appsink named 'sink' missing".into()))?
        .downcast::<gst_app::AppSink>()
        .map_err(|_| CameraIngestError::PipelineBuild("'sink' is not AppSink".into()))?;

    install_frame_callback(&appsink, camera_id, mailbox, counters);

    Ok(FakeFilePipeline { pipeline, appsink })
}

/// Wire the new-sample callback onto an RGB24 appsink so decoded frames flow
/// into the shared `FrameMailbox`, `FrameStorage` and `StreamingBus`. Shared by
/// the parse-launch path (fakefile/local) and the element-built webrtc pipeline
/// so the frame contract (Branch A) is byte-for-byte identical across sources.
pub(crate) fn install_frame_callback(
    appsink: &gst_app::AppSink,
    camera_id: String,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) {
    let mailbox_cb = mailbox.clone();
    let counters_cb = counters.clone();
    let camera_id_cb = camera_id;
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                let s = caps.structure(0).ok_or(gst::FlowError::Error)?;
                let width: i32 = s.get("width").map_err(|_| gst::FlowError::Error)?;
                let height: i32 = s.get("height").map_err(|_| gst::FlowError::Error)?;
                let pts_ns = buffer.pts().map(|t| t.nseconds());
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                // Single Arc<[u8]> shared between mailbox + storage + future
                // consumers, copied once straight from the GStreamer map
                // (no intermediate Vec).
                let shared: Arc<[u8]> = Arc::from(map.as_slice());
                let frame_size = shared.len();
                mailbox_cb.put(LatestFrame {
                    width: width as u32,
                    height: height as u32,
                    timestamp_unix_ms: ts_ms,
                    pts_ns,
                    data: shared.clone(),
                    format: DetectFrameFormat::Rgb24,
                    device: None,
                });
                counters_cb.increment(ts_ms / 1000);

                let metadata = FrameMetadata {
                    camera_id: camera_id_cb.clone(),
                    width: width as u32,
                    height: height as u32,
                    pixel_format: FramePixelFormat::Rgb24,
                    timestamp_unix_ms: ts_ms,
                    pts: pts_ns,
                    frame_size_bytes: frame_size,
                };
                let stored = StoredFrame {
                    metadata: metadata.clone(),
                    data: shared,
                    created_at: std::time::Instant::now(),
                };
                let frame_ref = frame_storage().insert(stored);
                streaming_bus().broadcast(&camera_id_cb, frame_ref, metadata);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
}

/// Wire the new-sample callback onto the detect appsink (RGB 560×560). Stores
/// the GPU-scaled frame into the mailbox's detect slot only — the detect frame
/// is analysis input, NOT a preview/storage frame, so it deliberately skips
/// FrameStorage / StreamingBus / counters (those belong to the crops branch).
pub(crate) fn install_detect_frame_callback(
    appsink: &gst_app::AppSink,
    mailbox: Arc<FrameMailbox>,
) {
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                let s = caps.structure(0).ok_or(gst::FlowError::Error)?;
                let width: i32 = s.get("width").map_err(|_| gst::FlowError::Error)?;
                let height: i32 = s.get("height").map_err(|_| gst::FlowError::Error)?;
                let pts_ns = buffer.pts().map(|t| t.nseconds());
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                let shared: Arc<[u8]> = Arc::from(map.as_slice());
                mailbox.put_detect(DetectFrame {
                    width: width as u32,
                    height: height as u32,
                    pts_ns,
                    data: shared,
                    format: DetectFrameFormat::Rgb24,
                    device: None,
                });
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
}

/// Wire the new-sample callback onto the ON-DEMAND RGB streaming appsink
/// (Stage 3). This is the RGB producer for the raw-frame `StreamingBus` +
/// `FrameStorage` consumers on the GPU-resident path: it runs the full NV12→RGB
/// `videoconvert` ONCE per frame, but ONLY while a viewer is subscribed (the
/// branch is attached on first subscribe, detached on last). It deliberately
/// does NOT touch the mailbox (analysis owns that, in NV12) nor `counters` (the
/// NV12 crops callback already counts frames), so FPS is not double-counted.
pub(crate) fn install_rgb_stream_callback(appsink: &gst_app::AppSink, camera_id: String) {
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                let s = caps.structure(0).ok_or(gst::FlowError::Error)?;
                let width: i32 = s.get("width").map_err(|_| gst::FlowError::Error)?;
                let height: i32 = s.get("height").map_err(|_| gst::FlowError::Error)?;
                let pts_ns = buffer.pts().map(|t| t.nseconds());
                let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let shared: Arc<[u8]> = Arc::from(map.as_slice());
                let frame_size = shared.len();
                let metadata = FrameMetadata {
                    camera_id: camera_id.clone(),
                    width: width as u32,
                    height: height as u32,
                    pixel_format: FramePixelFormat::Rgb24,
                    timestamp_unix_ms: ts_ms,
                    pts: pts_ns,
                    frame_size_bytes: frame_size,
                };
                let stored = StoredFrame {
                    metadata: metadata.clone(),
                    data: shared,
                    created_at: std::time::Instant::now(),
                };
                let frame_ref = frame_storage().insert(stored);
                streaming_bus().broadcast(&camera_id, frame_ref, metadata);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
}

/// Wire the new-sample callback onto a raw-NV12 detect appsink (GPU-resident
/// path). Unlike [`install_detect_frame_callback`] this stores the FULL-RES NV12
/// frame (no CPU videoconvert / resize): the detector does YUV→RGB + resize on
/// the GPU via `detect_batch_gpu`. Y and UV planes are packed contiguously into
/// one `Arc<[u8]>` ([Y | UV]) preserving each plane's stride, and the YUV→RGB
/// coefficients are read from the frame colorimetry (default BT.709 limited when
/// absent/unknown — the usual H.264 camera decode). Detect-slot only: no
/// FrameStorage / StreamingBus / counters (those belong to the crops branch).
pub(crate) fn install_detect_frame_callback_nv12(
    appsink: &gst_app::AppSink,
    mailbox: Arc<FrameMailbox>,
) {
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                let info =
                    gst_video::VideoInfo::from_caps(caps).map_err(|_| gst::FlowError::Error)?;
                let pts_ns = buffer.pts().map(|t| t.nseconds());
                // VideoFrameRef respects any GstVideoMeta (real strides/offsets),
                // so padded NVDEC surfaces are read correctly.
                let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
                    .map_err(|_| gst::FlowError::Error)?;
                let y = frame.plane_data(0).map_err(|_| gst::FlowError::Error)?;
                let uv = frame.plane_data(1).map_err(|_| gst::FlowError::Error)?;
                let strides = frame.plane_stride();
                let y_stride = *strides.first().ok_or(gst::FlowError::Error)? as u32;
                let uv_stride = *strides.get(1).ok_or(gst::FlowError::Error)? as u32;
                let (kr, kb, full_range) = nv12_color_from_info(&info);

                // Pack [Y | UV] contiguously, recording the UV offset. Each plane
                // keeps its native stride so the GPU kernel samples correctly.
                let mut buf = Vec::with_capacity(y.len() + uv.len());
                buf.extend_from_slice(y);
                buf.extend_from_slice(uv);
                let uv_offset = y.len() as u32;
                let data: Arc<[u8]> = Arc::from(buf);
                mailbox.put_detect(DetectFrame {
                    width: info.width(),
                    height: info.height(),
                    pts_ns,
                    data,
                    format: DetectFrameFormat::Nv12 {
                        y_stride,
                        uv_stride,
                        y_offset: 0,
                        uv_offset,
                        kr,
                        kb,
                        full_range,
                    },
                    device: None,
                });
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
}

/// Zero-copy (Stage 4) detect callback: the appsink delivers the NVDEC frame in
/// DEVICE memory (`memory:CUDAMemory`, no `cudadownload`). This maps the CUDA
/// surface in place, runs the fused NV12→RGB + resize + normalize kernel DIRECTLY
/// on the decoder device memory (no host download, no re-upload) producing the
/// owned `[1,3,560,560]` device tensor, unmaps immediately, and stores that
/// tensor in the detect slot for the analysis loop's ORT forward.
///
/// GstBuffer LIFETIME (precise): `pull_sample` holds a ref on the buffer for the
/// whole callback; the buffer keeps ONE NVDEC decode surface out of the decoder's
/// finite pool. The [`gst_cuda_ffi::CudaNv12Map`] guard maps the surface, the
/// kernel reads it, `preprocess_nv12_device_gpu` syncs (so the read completed),
/// then the guard drops → `gst_memory_unmap`, and the callback returns → the
/// buffer ref drops → the surface returns to the pool. The borrow spans ONLY the
/// synchronous map+kernel+sync (a few ms), never the async ORT run (that reads
/// the OWNED tensor, decoupled from the surface). At most one in-flight surface
/// per camera is held (`max-buffers=1, drop=true`), well under the decode pool.
///
/// FALLBACK (never regress): any map failure / non-device-0 pointer / null ptr →
/// host-download exactly like [`install_detect_frame_callback_nv12`] (the
/// readable host map of a GstCudaMemory copies device→host), storing a normal
/// NV12 detect frame. A camera never breaks.
///
/// VERIFY: with `[vision] zerocopy_verify = true`, after the device preprocess it
/// ALSO downloads the NV12 and runs the download preprocess, asserting the two
/// device tensors are element-identical (same kernel, same pixels) on the first
/// few frames — the correctness gate for the zero-copy path.
#[cfg(all(feature = "inference-vision-gpu", feature = "inference-supertonic"))]
pub(crate) fn install_detect_frame_callback_cuda(
    appsink: &gst_app::AppSink,
    mailbox: Arc<FrameMailbox>,
) {
    use crate::vision::gpu_preprocess::{
        preprocess_nv12_device_gpu, ColorCoeffs, Nv12DevicePlanes,
    };
    // RF-DETR detect input side + ImageNet normalization — must match the
    // detector's `detect_batch_gpu` preprocess so device and download paths agree.
    const S: usize = DETECT_RESIZE_DIM as usize;
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];

    let fallback_logged = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let verify_frames = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(
        if crate::vision::settings::get().zerocopy_verify {
            8
        } else {
            0
        },
    ));

    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                let info =
                    gst_video::VideoInfo::from_caps(caps).map_err(|_| gst::FlowError::Error)?;
                let pts_ns = buffer.pts().map(|t| t.nseconds());
                let (kr, kb, full_range) = nv12_color_from_info(&info);
                let color = ColorCoeffs { kr, kb, full_range };

                // Bind the match result to a local so the `map_nv12_device`
                // scrutinee temporary (which borrows `buffer` → `sample`) is
                // dropped at the end of this statement, before `sample` itself.
                let outcome = match super::gst_cuda_ffi::map_nv12_device(buffer, &info) {
                    Ok(map) => {
                        let planes = Nv12DevicePlanes {
                            y_ptr: map.y_device_ptr(),
                            y_stride: map.y_stride(),
                            uv_ptr: map.uv_device_ptr(),
                            uv_stride: map.uv_stride(),
                            w: map.width(),
                            h: map.height(),
                        };
                        let tensor = match preprocess_nv12_device_gpu(planes, S, MEAN, STD, color) {
                            Ok(t) => t,
                            Err(e) => {
                                // Kernel failed on a valid map: unmap and fall
                                // back to the host download for this frame.
                                drop(map);
                                warn_zerocopy_fallback(
                                    &fallback_logged,
                                    &format!("device preprocess failed: {e:#}"),
                                );
                                return download_nv12_detect(&mailbox, buffer, &info, pts_ns);
                            }
                        };

                        // Correctness gate: compare against the download path.
                        let remaining = verify_frames.load(std::sync::atomic::Ordering::Relaxed);
                        if remaining > 0 {
                            verify_zerocopy(&map, &info, S, MEAN, STD, color, &tensor);
                            verify_frames
                                .store(remaining - 1, std::sync::atomic::Ordering::Relaxed);
                        }

                        // Kernel synced inside preprocess; the borrowed surface is
                        // fully consumed. Unmap now, BEFORE the async ORT run.
                        drop(map);

                        let handle = DeviceDetectTensor(std::sync::Arc::new(tensor));
                        mailbox.put_detect(DetectFrame {
                            width: info.width(),
                            height: info.height(),
                            pts_ns,
                            // `data`/`format` are ignored when `device` is set; keep
                            // them cheap + colorimetry-tagged for logs/consistency.
                            data: Arc::from(&[][..]),
                            format: DetectFrameFormat::Nv12 {
                                y_stride: 0,
                                uv_stride: 0,
                                y_offset: 0,
                                uv_offset: 0,
                                kr,
                                kb,
                                full_range,
                            },
                            device: Some(handle),
                        });
                        Ok(gst::FlowSuccess::Ok)
                    }
                    Err(e) => {
                        warn_zerocopy_fallback(&fallback_logged, &format!("map failed: {e}"));
                        download_nv12_detect(&mailbox, buffer, &info, pts_ns)
                    }
                };
                outcome
            })
            .build(),
    );
}

/// Host-download fallback shared by the zero-copy callback: read the (possibly
/// device) buffer to host (GstCudaMemory's readable map copies device→host),
/// pack `[Y | UV]`, and store a normal NV12 detect frame (`device: None`, the
/// detector runs the download preprocess). Identical to the deployed NV12 detect
/// callback — this is the guaranteed no-regress path.
#[cfg(all(feature = "inference-vision-gpu", feature = "inference-supertonic"))]
fn download_nv12_detect(
    mailbox: &Arc<FrameMailbox>,
    buffer: &gst::BufferRef,
    info: &gst_video::VideoInfo,
    pts_ns: Option<u64>,
) -> std::result::Result<gst::FlowSuccess, gst::FlowError> {
    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, info)
        .map_err(|_| gst::FlowError::Error)?;
    let y = frame.plane_data(0).map_err(|_| gst::FlowError::Error)?;
    let uv = frame.plane_data(1).map_err(|_| gst::FlowError::Error)?;
    let strides = frame.plane_stride();
    let y_stride = *strides.first().ok_or(gst::FlowError::Error)? as u32;
    let uv_stride = *strides.get(1).ok_or(gst::FlowError::Error)? as u32;
    let (kr, kb, full_range) = nv12_color_from_info(info);
    let mut buf = Vec::with_capacity(y.len() + uv.len());
    buf.extend_from_slice(y);
    buf.extend_from_slice(uv);
    let uv_offset = y.len() as u32;
    let data: Arc<[u8]> = Arc::from(buf);
    mailbox.put_detect(DetectFrame {
        width: info.width(),
        height: info.height(),
        pts_ns,
        data,
        format: DetectFrameFormat::Nv12 {
            y_stride,
            uv_stride,
            y_offset: 0,
            uv_offset,
            kr,
            kb,
            full_range,
        },
        device: None,
    });
    Ok(gst::FlowSuccess::Ok)
}

/// Logs the first zero-copy fallback (once) at warn, quieter afterwards — a
/// camera silently degrading to the download path should be visible but not spammy.
#[cfg(all(feature = "inference-vision-gpu", feature = "inference-supertonic"))]
fn warn_zerocopy_fallback(logged: &std::sync::atomic::AtomicBool, reason: &str) {
    if !logged.swap(true, std::sync::atomic::Ordering::Relaxed) {
        tracing::warn!(
            reason,
            "zero-copy detect: falling back to host download for this camera"
        );
    }
}

/// `[vision] zerocopy_verify`: run the DOWNLOAD preprocess on the same frame and
/// assert its device tensor is element-identical to the zero-copy one (same
/// kernel, same pixels → must be bit-identical). Logged, not panicking, so a
/// mismatch surfaces without killing a live camera.
#[cfg(all(feature = "inference-vision-gpu", feature = "inference-supertonic"))]
fn verify_zerocopy(
    map: &super::gst_cuda_ffi::CudaNv12Map<'_>,
    info: &gst_video::VideoInfo,
    s: usize,
    mean: [f32; 3],
    stdv: [f32; 3],
    color: crate::vision::gpu_preprocess::ColorCoeffs,
    device_tensor: &crate::vision::gpu_preprocess::OwnedDeviceTensor,
) {
    // Download the NV12 to host and run the download preprocess for reference.
    // `map` is still valid here (we compare before unmapping) but we cannot host-
    // map the same GstMemory twice, so download via a fresh readable frame is not
    // available; instead reconstruct host NV12 from a device→host copy is complex.
    // The zero-copy and download kernels are IDENTICAL and read identical device
    // bytes, so we compare the device tensor to a HOST-side reference computed
    // from the SAME device planes via the download preprocess over a host copy.
    let host = match device_tensor.copy_to_host() {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(error = %e, "zero-copy verify: device copy_to_host failed");
            return;
        }
    };
    // Reference: pull the NV12 planes to host and run `preprocess_nv12_batch_gpu`.
    let y_bytes = map.y_stride() * map.height() as usize;
    let uv_bytes = map.uv_stride() * ((map.height() as usize + 1) / 2);
    let mut y_host = vec![0u8; y_bytes];
    let mut uv_host = vec![0u8; uv_bytes];
    if crate::vision::gpu_preprocess::device_to_host_copy(map.y_device_ptr(), &mut y_host).is_err()
        || crate::vision::gpu_preprocess::device_to_host_copy(map.uv_device_ptr(), &mut uv_host)
            .is_err()
    {
        tracing::warn!("zero-copy verify: NV12 device→host copy failed");
        return;
    }
    let frame = crate::vision::gpu_preprocess::Nv12Frame {
        y: &y_host,
        y_stride: map.y_stride(),
        uv: &uv_host,
        uv_stride: map.uv_stride(),
        w: map.width(),
        h: map.height(),
    };
    let _ = info;
    match crate::vision::gpu_preprocess::preprocess_nv12_batch_gpu(&[frame], s, mean, stdv, color)
        .and_then(|b| b.copy_to_host())
    {
        Ok(reference) => {
            let mut mismatches = 0usize;
            let mut max_diff = 0f32;
            for (a, b) in host.iter().zip(reference.iter()) {
                let d = (a - b).abs();
                if d > 0.0 {
                    mismatches += 1;
                    if d > max_diff {
                        max_diff = d;
                    }
                }
            }
            if mismatches == 0 {
                tracing::info!(
                    elements = host.len(),
                    "zero-copy verify: device tensor MATCHES download path (bit-identical)"
                );
            } else {
                tracing::error!(
                    mismatches,
                    max_diff,
                    elements = host.len(),
                    "zero-copy verify: MISMATCH between zero-copy and download tensors"
                );
            }
        }
        Err(e) => tracing::warn!(error = %e, "zero-copy verify: reference preprocess failed"),
    }
}

/// Wire the new-sample callback onto a raw-NV12 CROPS appsink (Stage 3,
/// GPU-resident path). Unlike [`install_frame_callback`] this keeps the frame in
/// NV12 (no CPU `videoconvert` to RGB — that full-4K convert per frame is the
/// bottleneck Stage 3 removes) and feeds ONLY the mailbox: the analysis loop cuts
/// enrichment crops from NV12 ([`crop_nv12`]) and snapshots convert lazily. It
/// deliberately skips `FrameStorage`/`StreamingBus` (those RGB consumers are fed
/// by the on-demand RGB branch, only while a viewer is watching), but still bumps
/// `counters` so FPS health tracking is unchanged. Y and UV planes are packed
/// contiguously into one `Arc<[u8]>` (`[Y | UV]`) preserving each plane's stride,
/// and the YUV→RGB coefficients come from the frame colorimetry.
pub(crate) fn install_frame_callback_nv12(
    appsink: &gst_app::AppSink,
    _camera_id: String,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) {
    let counters_cb = counters.clone();
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                let info =
                    gst_video::VideoInfo::from_caps(caps).map_err(|_| gst::FlowError::Error)?;
                let pts_ns = buffer.pts().map(|t| t.nseconds());
                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
                    .map_err(|_| gst::FlowError::Error)?;
                let y = frame.plane_data(0).map_err(|_| gst::FlowError::Error)?;
                let uv = frame.plane_data(1).map_err(|_| gst::FlowError::Error)?;
                let strides = frame.plane_stride();
                let y_stride = *strides.first().ok_or(gst::FlowError::Error)? as u32;
                let uv_stride = *strides.get(1).ok_or(gst::FlowError::Error)? as u32;
                let (kr, kb, full_range) = nv12_color_from_info(&info);
                // Pack [Y | UV] contiguously, recording the UV offset. Each plane
                // keeps its native stride so `crop_nv12` samples correctly.
                let mut buf = Vec::with_capacity(y.len() + uv.len());
                buf.extend_from_slice(y);
                buf.extend_from_slice(uv);
                let uv_offset = y.len() as u32;
                let data: Arc<[u8]> = Arc::from(buf);
                mailbox.put(LatestFrame {
                    width: info.width(),
                    height: info.height(),
                    timestamp_unix_ms: ts_ms,
                    pts_ns,
                    data,
                    format: DetectFrameFormat::Nv12 {
                        y_stride,
                        uv_stride,
                        y_offset: 0,
                        uv_offset,
                        kr,
                        kb,
                        full_range,
                    },
                    device: None,
                });
                counters_cb.increment(ts_ms / 1000);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
}

/// Whether the zero-copy CROPS path is active (`[vision] zerocopy_crops = true`).
/// OFF by default: the mailbox keeps the full host `[Y | UV]` download
/// ([`install_frame_callback_nv12`]) and everything is byte-identical to today.
/// ON: the crops appsink consumes the NVDEC surface in DEVICE memory
/// ([`install_frame_callback_crops_cuda`]) and the mailbox holds a
/// [`DeviceCropsFrame`] instead of copying the 4K frame to host every stream
/// frame. Only meaningful with the GPU inference features (they link cudart +
/// provide the device crop path); false without them.
pub fn zerocopy_crops_enabled() -> bool {
    #[cfg(all(feature = "inference-vision-gpu", feature = "inference-supertonic"))]
    {
        crate::vision::settings::get().zerocopy_crops
    }
    #[cfg(not(all(feature = "inference-vision-gpu", feature = "inference-supertonic")))]
    {
        false
    }
}

/// Zero-copy CROPS callback: the appsink delivers the NVDEC frame in DEVICE
/// memory (`video/x-raw(memory:CUDAMemory), NV12`, off `tee_cuda` BEFORE
/// `cudadownload`). This validates the surface maps as device-0 NV12, then stores
/// a [`DeviceCropsFrame`] (ref-holding the `gst::Sample`) in the mailbox with an
/// EMPTY `data` — the full 4K NV12 is NEVER downloaded on the per-frame path.
/// Enrichment cuts small crops off the device surface, snapshots/display download
/// on demand.
///
/// SURFACE LIFETIME: `mailbox.put` replaces the previous `LatestFrame`, dropping
/// its `Sample` → the old surface returns to the decoder pool. So the mailbox
/// pins exactly ONE surface per camera (the latest). `max-buffers=1, drop=true`
/// on the appsink means the decoder is never blocked on this branch.
///
/// FALLBACK (never regress): if the map fails / the pointer is not device-0 /
/// the buffer is not a 2-plane NV12 surface, this frame falls back to the FULL
/// host download exactly like [`install_frame_callback_nv12`] (a normal NV12
/// `LatestFrame`, `device: None`), logged once. A camera never breaks.
#[cfg(all(feature = "inference-vision-gpu", feature = "inference-supertonic"))]
pub(crate) fn install_frame_callback_crops_cuda(
    appsink: &gst_app::AppSink,
    _camera_id: String,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) {
    let counters_cb = counters.clone();
    let fallback_logged = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    appsink.set_callbacks(
        gst_app::AppSinkCallbacks::builder()
            .new_sample(move |sink| {
                let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                let caps = sample.caps().ok_or(gst::FlowError::Error)?;
                let info =
                    gst_video::VideoInfo::from_caps(caps).map_err(|_| gst::FlowError::Error)?;
                let pts_ns = buffer.pts().map(|t| t.nseconds());
                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let (kr, kb, full_range) = nv12_color_from_info(&info);

                // Validate the surface maps as device-0 NV12 before we commit to
                // the zero-copy path. We only need the geometry here; the map
                // guard drops immediately (the `Sample` keeps the surface alive
                // for later on-demand crops).
                let mapped_ok = super::gst_cuda_ffi::map_nv12_device(buffer, &info).is_ok();
                if mapped_ok {
                    mailbox.put(LatestFrame {
                        width: info.width(),
                        height: info.height(),
                        timestamp_unix_ms: ts_ms,
                        pts_ns,
                        // No host copy — device consumers map the `Sample`.
                        data: Arc::from(&[][..]),
                        format: DetectFrameFormat::Nv12 {
                            y_stride: 0,
                            uv_stride: 0,
                            y_offset: 0,
                            uv_offset: 0,
                            kr,
                            kb,
                            full_range,
                        },
                        device: Some(DeviceCropsFrame {
                            sample: sample.clone(),
                            width: info.width(),
                            height: info.height(),
                            kr,
                            kb,
                            full_range,
                        }),
                    });
                    counters_cb.increment(ts_ms / 1000);
                    return Ok(gst::FlowSuccess::Ok);
                }

                // Map failed → full host download for THIS frame (guaranteed
                // fallback, byte-identical to the deployed NV12 crops path).
                if !fallback_logged.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    tracing::warn!(
                        "zero-copy crops: device map failed; falling back to host download for this camera"
                    );
                }
                let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info)
                    .map_err(|_| gst::FlowError::Error)?;
                let y = frame.plane_data(0).map_err(|_| gst::FlowError::Error)?;
                let uv = frame.plane_data(1).map_err(|_| gst::FlowError::Error)?;
                let strides = frame.plane_stride();
                let y_stride = *strides.first().ok_or(gst::FlowError::Error)? as u32;
                let uv_stride = *strides.get(1).ok_or(gst::FlowError::Error)? as u32;
                let mut buf = Vec::with_capacity(y.len() + uv.len());
                buf.extend_from_slice(y);
                buf.extend_from_slice(uv);
                let uv_offset = y.len() as u32;
                let data: Arc<[u8]> = Arc::from(buf);
                mailbox.put(LatestFrame {
                    width: info.width(),
                    height: info.height(),
                    timestamp_unix_ms: ts_ms,
                    pts_ns,
                    data,
                    format: DetectFrameFormat::Nv12 {
                        y_stride,
                        uv_stride,
                        y_offset: 0,
                        uv_offset,
                        kr,
                        kb,
                        full_range,
                    },
                    device: None,
                });
                counters_cb.increment(ts_ms / 1000);
                Ok(gst::FlowSuccess::Ok)
            })
            .build(),
    );
}

/// BT.601/709/2020 luma coefficients + range read from a decoded frame's
/// colorimetry. Defaults to BT.709 limited (the usual H.264 camera decode) when
/// the matrix is unknown/unspecified. Shared by the detect and crops NV12
/// callbacks so both tag a frame identically.
fn nv12_color_from_info(info: &gst_video::VideoInfo) -> (f32, f32, bool) {
    let color = info.colorimetry();
    let (kr, kb) = match color.matrix() {
        gst_video::VideoColorMatrix::Bt601 => (0.299f32, 0.114f32),
        gst_video::VideoColorMatrix::Bt2020 => (0.2627f32, 0.0593f32),
        _ => (0.2126f32, 0.0722f32),
    };
    let full_range = matches!(color.range(), gst_video::VideoColorRange::Range0_255);
    (kr, kb, full_range)
}

/// YUV→RGB (u8) for one NV12 source pixel. Bit-for-bit mirror of the CUDA
/// `nv12_yuv_to_rgb_u8` device function (`cuda/nv12_to_rgb_resize_normalize.cu`)
/// so a CPU-cropped RGB pixel matches what the GPU parity kernel would produce:
/// same f32 formula, same limited/full range scaling, same `*255 + 0.5` round.
#[inline]
pub fn nv12_yuv_to_rgb_u8(yv: u8, uu: u8, vv: u8, kr: f32, kb: f32, full_range: bool) -> [u8; 3] {
    let kg = 1.0f32 - kr - kb;
    let (y, cb, cr) = if full_range {
        (
            yv as f32 / 255.0,
            (uu as f32 - 128.0) / 255.0,
            (vv as f32 - 128.0) / 255.0,
        )
    } else {
        (
            (yv as f32 - 16.0) / 219.0,
            (uu as f32 - 128.0) / 224.0,
            (vv as f32 - 128.0) / 224.0,
        )
    };
    let r = y + 2.0 * (1.0 - kr) * cr;
    let b = y + 2.0 * (1.0 - kb) * cb;
    let g = y - (2.0 * kr * (1.0 - kr) / kg) * cr - (2.0 * kb * (1.0 - kb) / kg) * cb;
    let enc = |v: f32| -> u8 { (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8 };
    [enc(r), enc(g), enc(b)]
}

/// Cut a tightly-packed RGB24 rectangle out of a packed NV12 buffer (`[Y | UV]`
/// as tagged by [`DetectFrameFormat::Nv12`]). Chroma is the nearest 2×2-shared
/// sample (`sx>>1, sy>>1`), matching the parity kernel's simple NV12→RGB siting.
/// `x0`/`y0` are forced EVEN so every 2×2 luma block reads its own chroma sample
/// (an odd offset would shift the whole crop's chroma by one sample). The crop is
/// then clamped to the frame so an even-snap never runs past the right/bottom
/// edge. Returns the RGB24 crop and its (possibly even-snapped/clamped) rect.
#[allow(clippy::too_many_arguments)]
pub fn crop_nv12(
    data: &[u8],
    frame_w: u32,
    frame_h: u32,
    y_stride: u32,
    uv_stride: u32,
    y_offset: u32,
    uv_offset: u32,
    kr: f32,
    kb: f32,
    full_range: bool,
    x0: u32,
    y0: u32,
    cw: u32,
    ch: u32,
) -> (Vec<u8>, u32, u32, u32, u32) {
    // Snap the origin down to even so 2×2 chroma alignment is preserved, then
    // clamp width/height so the crop stays inside the frame.
    let ex0 = x0 & !1;
    let ey0 = y0 & !1;
    let ecw = cw.min(frame_w.saturating_sub(ex0));
    let ech = ch.min(frame_h.saturating_sub(ey0));
    let yp = &data[y_offset as usize..];
    let uvp = &data[uv_offset as usize..];
    let (ys, uvs) = (y_stride as usize, uv_stride as usize);
    let mut out = Vec::with_capacity(ecw as usize * ech as usize * 3);
    for row in 0..ech as usize {
        let sy = ey0 as usize + row;
        let uv_row = (sy >> 1) * uvs;
        let y_row = sy * ys;
        for col in 0..ecw as usize {
            let sx = ex0 as usize + col;
            let yv = yp[y_row + sx];
            let cx = sx >> 1;
            let uu = uvp[uv_row + cx * 2];
            let vv = uvp[uv_row + cx * 2 + 1];
            out.extend_from_slice(&nv12_yuv_to_rgb_u8(yv, uu, vv, kr, kb, full_range));
        }
    }
    (out, ex0, ey0, ecw, ech)
}

/// Convert a whole packed NV12 frame to tightly-packed RGB24. Used by the lazy
/// snapshot convert and the analysis-flow image blob — both rare paths where an
/// RGB frame is genuinely needed (never the per-frame hot path). Delegates to
/// [`crop_nv12`] over the full frame (origin already even).
pub fn nv12_frame_to_rgb24(
    data: &[u8],
    width: u32,
    height: u32,
    format: &DetectFrameFormat,
) -> Option<Vec<u8>> {
    let DetectFrameFormat::Nv12 {
        y_stride,
        uv_stride,
        y_offset,
        uv_offset,
        kr,
        kb,
        full_range,
    } = *format
    else {
        return None;
    };
    let (rgb, _, _, _, _) = crop_nv12(
        data, width, height, y_stride, uv_stride, y_offset, uv_offset, kr, kb, full_range, 0, 0,
        width, height,
    );
    Some(rgb)
}

/// Seek the pipeline back to position 0. Used on EOS to implement the replay
/// loop without tearing down the entire pipeline.
pub fn seek_to_start(pipeline: &gst::Pipeline) -> Result<()> {
    pipeline
        .seek_simple(
            gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
            gst::ClockTime::ZERO,
        )
        .map_err(|e| CameraIngestError::PipelineState(format!("seek failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_file_url_strips_scheme() {
        // We expect the function to strip `file://` and accept the canonical
        // on-disk path. Use the sample mp4 if available; otherwise skip.
        let p = std::path::PathBuf::from("assets/test/sample_traffic.mp4");
        if !p.exists() {
            eprintln!("skipping — sample mp4 missing");
            return;
        }
        let url = format!("file://{}", p.canonicalize().unwrap().to_string_lossy());
        let resolved = resolve_file_url(&url).expect("resolve");
        assert!(resolved.is_file());
    }

    #[test]
    fn test_resolve_file_url_rejects_missing() {
        let err = resolve_file_url("/no/such/file/sample.mp4").unwrap_err();
        assert!(matches!(err, CameraIngestError::FileNotFound(_)));
    }

    #[test]
    fn test_resolve_file_url_rejects_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.bin");
        std::fs::write(&target, b"x").unwrap();
        let link = dir.path().join("link.bin");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = resolve_file_url(link.to_str().unwrap()).unwrap_err();
        assert!(matches!(err, CameraIngestError::SymlinkNotAllowed(_)));
    }
}
