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
    pub data: Arc<[u8]>,
}

#[derive(Default)]
pub struct FrameMailbox {
    inner: Mutex<Option<LatestFrame>>,
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

    pub fn snapshot(&self) -> (u64, u64, Option<u64>) {
        let g = self.inner.lock();
        (g.frames_total, g.frames_dropped, g.last_frame_at_unix_s)
    }
}

/// Initialize GStreamer once. Safe to call multiple times; `gst::init` is
/// idempotent and guarded internally with a `std::sync::Once`.
pub fn ensure_gst_initialized() -> Result<()> {
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

    // Use parse_launch — concise, exactly mirrors the documented recipe and
    // returns a single Element we downcast to Pipeline.
    let desc = format!(
        "filesrc location=\"{}\" ! decodebin ! videoconvert ! video/x-raw,format=RGB ! appsink name=sink emit-signals=false sync=true max-buffers=1 drop=true",
        location.replace('"', "\\\"")
    );
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
                let bytes = map.as_slice().to_vec();
                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                // Single Arc<[u8]> shared between mailbox + storage + future
                // consumers; the per-frame allocation here is still the
                // GStreamer buffer copy (improving that needs a zero-copy
                // pull which is out of scope for F1a).
                let shared: Arc<[u8]> = Arc::from(bytes.into_boxed_slice());
                let frame_size = shared.len();
                mailbox_cb.put(LatestFrame {
                    width: width as u32,
                    height: height as u32,
                    timestamp_unix_ms: ts_ms,
                    data: shared.clone(),
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

    Ok(FakeFilePipeline { pipeline, appsink })
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
