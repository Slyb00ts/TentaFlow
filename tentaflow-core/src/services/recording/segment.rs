// =============================================================================
// File: services/recording/segment.rs — GStreamer MP4 segment recorder
// =============================================================================
//
// F1a: ad-hoc recording from a `file://` source built via typed GStreamer
// elements (no parse_launch). We re-encode through `x264enc tune=zerolatency`
// + `mp4mux` so unit tests can drive the path off the bundled sample mp4
// without depending on a live camera. F1b will swap this for a tee tap off
// the existing camera supervisor pipeline so live RTSP segments record
// without re-encode.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;

use super::error::{RecordingError, Result};
use super::storage::{
    camera_subdir, recording_base_dir, sha256_hex, validate_camera_id, RecordingKind,
    RecordingRef,
};
use super::SavedRecording;

/// Record `duration_secs` of `source_url` into an mp4 under the per-camera
/// `segments/` directory. Returns the catalog entry the caller will persist.
pub async fn save_segment_mp4(
    camera_id: &str,
    source_url: &str,
    duration_secs: u32,
) -> Result<SavedRecording> {
    validate_camera_id(camera_id)?;
    if duration_secs == 0 {
        return Err(RecordingError::GstPipeline("duration_secs must be > 0".into()));
    }

    gst::init().map_err(|e| RecordingError::GstPipeline(format!("gst init: {e}")))?;

    let source_path = parse_file_url(source_url)?;

    let recording_ref = RecordingRef(format!("clip_{}", uuid::Uuid::new_v4()));
    let base = recording_base_dir()?;
    let dir = camera_subdir(&base, camera_id, RecordingKind::Segment);
    tokio::fs::create_dir_all(&dir).await?;
    let file_path: PathBuf = dir.join(format!("{}.mp4", recording_ref.0));
    // Atomic write: GStreamer mp4mux scribbles to `<final>.tmp`; on success we
    // rename to the final path, on any error/timeout we delete the partial. The
    // final path therefore only ever exists if the moov atom was finalized.
    let tmp_path: PathBuf = dir.join(format!("{}.mp4.tmp", recording_ref.0));
    // Stale tmp from a previous crash — drop it before opening filesink.
    let _ = tokio::fs::remove_file(&tmp_path).await;

    let pipeline = build_segment_pipeline(&source_path, &tmp_path)?;

    if let Err(e) = pipeline.set_state(gst::State::Playing) {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(RecordingError::GstPipeline(format!("set Playing: {e}")));
    }

    let bus = pipeline
        .bus()
        .ok_or_else(|| RecordingError::GstPipeline("pipeline missing bus".into()))?;
    let start = std::time::Instant::now();
    let deadline = Duration::from_secs(duration_secs as u64);

    // Drain bus messages until duration elapses or we hit EOS/Error.
    let drain_result = loop {
        let elapsed = start.elapsed();
        if elapsed >= deadline {
            break Ok(());
        }
        let remaining = deadline - elapsed;
        let bus_clone = bus.clone();
        let remaining_ns = remaining.as_nanos().min(u64::MAX as u128) as u64;
        let msg = tokio::task::spawn_blocking(move || {
            bus_clone.timed_pop(gst::ClockTime::from_nseconds(remaining_ns))
        })
        .await
        .map_err(|e| RecordingError::GstPipeline(format!("join: {e}")))?;
        match msg {
            Some(m) => match m.view() {
                gst::MessageView::Error(e) => {
                    break Err(RecordingError::GstPipeline(e.error().to_string()));
                }
                gst::MessageView::Eos(_) => break Ok(()),
                _ => continue,
            },
            None => break Ok(()),
        }
    };

    // Send EOS so mp4mux finalizes the moov atom. Without a successful
    // finalization the mp4 is unplayable, so we MUST confirm the EOS made it
    // through the bus — a timeout or downstream Error here means the file is
    // truncated and the caller has to know.
    pipeline.send_event(gst::event::Eos::new());
    let bus_finalize = bus.clone();
    let finalize_join = tokio::task::spawn_blocking(move || {
        bus_finalize.timed_pop_filtered(
            gst::ClockTime::from_seconds(2),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
    })
    .await;
    let _ = pipeline.set_state(gst::State::Null);

    // Helper that wipes the partial tmp on any failure path. `_ =` because
    // there is nothing useful the caller can do with a cleanup error; the
    // primary cause is what matters.
    let cleanup_tmp = || {
        let p = tmp_path.clone();
        async move {
            let _ = tokio::fs::remove_file(&p).await;
        }
    };

    if let Err(e) = drain_result {
        cleanup_tmp().await;
        return Err(e);
    }

    let finalize_msg = match finalize_join {
        Ok(m) => m,
        Err(e) => {
            cleanup_tmp().await;
            return Err(RecordingError::GstPipeline(format!("finalize join: {e}")));
        }
    };
    match finalize_msg {
        Some(m) => match m.view() {
            gst::MessageView::Eos(_) => {}
            gst::MessageView::Error(e) => {
                let msg = e.error().to_string();
                cleanup_tmp().await;
                return Err(RecordingError::GstPipeline(format!(
                    "mp4mux finalize error: {msg}"
                )));
            }
            _ => {
                cleanup_tmp().await;
                return Err(RecordingError::GstPipeline(
                    "unexpected bus message during finalize".into(),
                ));
            }
        },
        None => {
            cleanup_tmp().await;
            return Err(RecordingError::GstPipeline(
                "mp4mux finalize timeout after 2s".into(),
            ));
        }
    }

    // Pipeline drained cleanly — promote tmp → final atomically. After this
    // point the final path is observable; before it, only the .tmp exists.
    if let Err(e) = tokio::fs::rename(&tmp_path, &file_path).await {
        cleanup_tmp().await;
        return Err(RecordingError::GstPipeline(format!(
            "rename tmp -> final failed: {e}"
        )));
    }

    let meta = tokio::fs::metadata(&file_path).await.map_err(|e| {
        RecordingError::GstPipeline(format!(
            "output file missing after pipeline drain: {e}"
        ))
    })?;
    let file_size_bytes = meta.len();
    let bytes = tokio::fs::read(&file_path).await?;
    let hash = sha256_hex(&bytes);
    let duration_ms = start.elapsed().as_millis().min(u32::MAX as u128) as u32;

    Ok(SavedRecording {
        recording_ref,
        kind: RecordingKind::Segment,
        file_path,
        file_size_bytes,
        duration_ms: Some(duration_ms),
        width: None,
        height: None,
        pixel_format: None,
        hash_sha256: hash,
        created_at: now_unix_secs(),
    })
}

/// Reject anything that isn't a `file://` URL. We do not yet support remote
/// schemes for ad-hoc segments — the live RTSP path goes through F1b's tee.
fn parse_file_url(source_url: &str) -> Result<PathBuf> {
    let rest = source_url.strip_prefix("file://").ok_or_else(|| {
        RecordingError::GstPipeline(format!(
            "unsupported URL scheme (expected file://): {source_url}"
        ))
    })?;
    if rest.is_empty() {
        return Err(RecordingError::GstPipeline(
            "file:// URL has empty path".into(),
        ));
    }
    Ok(PathBuf::from(rest))
}

/// Build the segment recording pipeline programmatically. Using typed
/// ElementFactory calls (instead of parse_launch) means caller-controlled
/// strings — paths, URLs — can never be interpreted as pipeline syntax.
fn build_segment_pipeline(
    source_path: &Path,
    output_path: &Path,
) -> Result<gst::Pipeline> {
    let source_str = source_path.to_str().ok_or_else(|| {
        RecordingError::GstPipeline("source path is not valid UTF-8".into())
    })?;
    let output_str = output_path.to_str().ok_or_else(|| {
        RecordingError::GstPipeline("output path is not valid UTF-8".into())
    })?;

    let pipeline = gst::Pipeline::with_name("tentaflow-segment");

    let filesrc = gst::ElementFactory::make("filesrc")
        .property("location", source_str)
        .build()
        .map_err(|e| RecordingError::GstPipeline(format!("filesrc: {e}")))?;
    let decodebin = gst::ElementFactory::make("decodebin")
        .build()
        .map_err(|e| RecordingError::GstPipeline(format!("decodebin: {e}")))?;
    let videoconvert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|e| RecordingError::GstPipeline(format!("videoconvert: {e}")))?;
    let x264enc = gst::ElementFactory::make("x264enc")
        .property_from_str("tune", "zerolatency")
        .build()
        .map_err(|e| RecordingError::GstPipeline(format!("x264enc: {e}")))?;
    let mp4mux = gst::ElementFactory::make("mp4mux")
        .build()
        .map_err(|e| RecordingError::GstPipeline(format!("mp4mux: {e}")))?;
    let filesink = gst::ElementFactory::make("filesink")
        .property("location", output_str)
        .build()
        .map_err(|e| RecordingError::GstPipeline(format!("filesink: {e}")))?;

    pipeline
        .add_many([&filesrc, &decodebin, &videoconvert, &x264enc, &mp4mux, &filesink])
        .map_err(|e| RecordingError::GstPipeline(format!("add_many: {e}")))?;

    filesrc
        .link(&decodebin)
        .map_err(|e| RecordingError::GstPipeline(format!("filesrc->decodebin: {e}")))?;
    videoconvert
        .link(&x264enc)
        .map_err(|e| RecordingError::GstPipeline(format!("videoconvert->x264enc: {e}")))?;
    x264enc
        .link(&mp4mux)
        .map_err(|e| RecordingError::GstPipeline(format!("x264enc->mp4mux: {e}")))?;
    mp4mux
        .link(&filesink)
        .map_err(|e| RecordingError::GstPipeline(format!("mp4mux->filesink: {e}")))?;

    // decodebin builds its src pads only once it knows the stream type, so we
    // wire the video branch lazily via pad-added. Weak ref on videoconvert
    // avoids keeping the element alive past pipeline teardown.
    let videoconvert_weak = videoconvert.downgrade();
    decodebin.connect_pad_added(move |_, src_pad| {
        let Some(videoconvert) = videoconvert_weak.upgrade() else {
            return;
        };
        let Some(sink_pad) = videoconvert.static_pad("sink") else {
            return;
        };
        if sink_pad.is_linked() {
            return;
        }
        // Only link video pads — audio streams from the input get dropped.
        let caps = src_pad
            .current_caps()
            .unwrap_or_else(|| src_pad.query_caps(None));
        let is_video = caps
            .structure(0)
            .map(|s| s.name().starts_with("video/"))
            .unwrap_or(false);
        if !is_video {
            return;
        }
        let _ = src_pad.link(&sink_pad);
    });

    Ok(pipeline)
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home_guard() -> (
        std::sync::MutexGuard<'static, ()>,
        tempfile::TempDir,
    ) {
        let guard = super::super::storage::home_sandbox_lock();
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", d.path());
        (guard, d)
    }

    /// On any pipeline failure the partial `.mp4.tmp` must be removed and the
    /// final `.mp4` must never appear. We hit a guaranteed failure path —
    /// missing source — and assert both invariants.
    #[tokio::test]
    async fn test_save_segment_partial_files_cleaned_up_on_error() {
        let _home = temp_home_guard();
        let _ = save_segment_mp4("cam_seg_partial", "file:///no/such/source.mp4", 2).await;
        // Walk the per-camera segments directory (if it exists) and assert
        // there is no `.mp4` or `.mp4.tmp` file lingering after a failure.
        let base = match super::super::storage::recording_base_dir() {
            Ok(p) => p,
            Err(_) => return,
        };
        let dir = base.join("cam_seg_partial").join("segments");
        if !dir.exists() {
            return;
        }
        for entry in std::fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            let s = p.to_string_lossy();
            assert!(
                !s.ends_with(".mp4") && !s.ends_with(".mp4.tmp"),
                "leftover partial after error: {s}"
            );
        }
    }

    #[tokio::test]
    async fn test_save_segment_invalid_source() {
        let _home = temp_home_guard();
        // GStreamer's filesrc errors out on missing files via the bus; we
        // surface it as GstPipeline. The check has to drain at least one bus
        // message which can take longer than the 1 s duration on slow CI,
        // hence the explicit 3 s ceiling — still well under the test timeout.
        let err =
            save_segment_mp4("cam_seg", "file:///no/such/path/xyz.mp4", 3).await.unwrap_err();
        assert!(matches!(err, RecordingError::GstPipeline(_)));
    }

    #[tokio::test]
    async fn test_save_segment_rejects_zero_duration() {
        let _home = temp_home_guard();
        let err = save_segment_mp4("cam_seg", "file:///dev/null", 0).await.unwrap_err();
        assert!(matches!(err, RecordingError::GstPipeline(_)));
    }

    #[tokio::test]
    async fn test_save_segment_invalid_url_scheme_rejected() {
        let _home = temp_home_guard();
        // Non-file:// schemes must be rejected before any pipeline is built.
        for url in [
            "http://example.com/video.mp4",
            "rtsp://10.0.0.1/stream",
            "/etc/passwd",
            "",
        ] {
            let err = save_segment_mp4("cam_seg", url, 1).await.unwrap_err();
            match err {
                RecordingError::GstPipeline(msg) => {
                    assert!(
                        msg.contains("unsupported URL scheme") || msg.contains("empty path"),
                        "unexpected error for {url:?}: {msg}"
                    );
                }
                other => panic!("expected GstPipeline error for {url:?}, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    #[ignore = "requires assets/test/sample_traffic.mp4 + full GStreamer plugin stack"]
    async fn test_save_segment_basic() {
        let _home = temp_home_guard();
        let sample = std::path::PathBuf::from("assets/test/sample_traffic.mp4");
        if !sample.exists() {
            eprintln!("skipping — sample mp4 missing");
            return;
        }
        let url = format!("file://{}", sample.canonicalize().unwrap().display());
        let saved = save_segment_mp4("cam_seg", &url, 2).await.expect("record ok");
        assert!(saved.file_path.exists());
        assert!(saved.file_size_bytes > 0);
        assert_eq!(saved.hash_sha256.len(), 64);
        // ~2 s wall clock; allow a generous 500 ms slack on either side.
        let d = saved.duration_ms.unwrap_or(0);
        assert!(d >= 1_500 && d <= 3_500, "duration_ms = {d}");
    }
}
