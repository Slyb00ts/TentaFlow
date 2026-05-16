// =============================================================================
// File: services/recording/segment.rs — GStreamer MP4 segment recorder
// =============================================================================
//
// F1a: ad-hoc recording from a `file://` source via parse_launch. We re-encode
// through `x264enc tune=zerolatency` + `mp4mux` so unit tests can drive the
// path off the bundled sample mp4 without depending on a live camera. F1b
// will swap this for a tee tap off the existing camera supervisor pipeline
// so live RTSP segments record without re-encode.

use std::path::PathBuf;
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

    let recording_ref = RecordingRef(format!("clip_{}", uuid::Uuid::new_v4()));
    let base = recording_base_dir()?;
    let dir = camera_subdir(&base, camera_id, RecordingKind::Segment);
    tokio::fs::create_dir_all(&dir).await?;
    let file_path: PathBuf = dir.join(format!("{}.mp4", recording_ref.0));

    let src = source_url.trim_start_matches("file://");
    let pipeline_desc = format!(
        "filesrc location=\"{}\" ! decodebin ! videoconvert ! x264enc tune=zerolatency ! mp4mux ! filesink location=\"{}\"",
        src.replace('"', "\\\""),
        file_path.display().to_string().replace('"', "\\\"")
    );

    let element = gst::parse::launch(&pipeline_desc)
        .map_err(|e| RecordingError::GstPipeline(format!("parse_launch: {e}")))?;
    let pipeline = element
        .downcast::<gst::Pipeline>()
        .map_err(|_| RecordingError::GstPipeline("not a pipeline".into()))?;

    pipeline
        .set_state(gst::State::Playing)
        .map_err(|e| RecordingError::GstPipeline(format!("set Playing: {e}")))?;

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

    // Send EOS so mp4mux finalizes the moov atom, then wait briefly for the
    // pipeline to drain. Without this the mp4 is unplayable.
    let _ = pipeline.send_event(gst::event::Eos::new());
    let bus_finalize = bus.clone();
    let _ = tokio::task::spawn_blocking(move || {
        bus_finalize.timed_pop_filtered(
            gst::ClockTime::from_seconds(2),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        )
    })
    .await;
    let _ = pipeline.set_state(gst::State::Null);

    drain_result?;

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
