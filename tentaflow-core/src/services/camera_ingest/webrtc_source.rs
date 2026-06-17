// =============================================================================
// File: services/camera_ingest/webrtc_source.rs
// Purpose: Camera source backed by a WebRTC video track. An H.264 Annex-B byte
//          stream (depacketized in tentaflow-hardware, delivered over an mpsc)
//          is pushed into a GStreamer appsrc, decoded and converted to RGB —
//          reusing the same appsink/frame path as every other camera. This is
//          the first appsrc pipeline in the repo.
// =============================================================================


use std::sync::Arc;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use tokio::sync::mpsc;

use super::error::{CameraIngestError, Result};
use super::fakefile::{
    build_pipeline_from_description, ensure_gst_initialized, FakeFilePipeline, FrameCounters,
    FrameMailbox,
};
use super::session::CameraConfig;

/// Build the appsrc → decode → RGB appsink pipeline. The appsink callback (frame
/// storage + streaming bus) is installed by `build_pipeline_from_description`;
/// we additionally fetch the appsrc so the pump can feed it.
pub fn build_webrtc_pipeline(
    config: &CameraConfig,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<(FakeFilePipeline, gst_app::AppSrc)> {
    ensure_gst_initialized()?;

    let desc = "appsrc name=src is-live=true format=time do-timestamp=true ! \
         h264parse ! decodebin ! videoconvert ! video/x-raw,format=RGB ! \
         appsink name=sink emit-signals=false sync=false max-buffers=2 drop=true";
    let built = build_pipeline_from_description(desc, config.camera_id.clone(), mailbox, counters)?;

    let appsrc = built
        .pipeline
        .by_name("src")
        .ok_or_else(|| CameraIngestError::PipelineBuild("appsrc 'src' missing".into()))?
        .downcast::<gst_app::AppSrc>()
        .map_err(|_| CameraIngestError::PipelineBuild("'src' is not AppSrc".into()))?;

    let caps = gst::Caps::builder("video/x-h264")
        .field("stream-format", "byte-stream")
        .field("alignment", "au")
        .build();
    appsrc.set_caps(Some(&caps));
    // Bounded internal buffering — the gate already drops/re-primes upstream, so
    // back-pressure here just bounds memory, not correctness.
    appsrc.set_max_bytes(4 * 1024 * 1024);

    Ok((built, appsrc))
}

/// Pump Annex-B chunks from the channel into the appsrc until the stream ends
/// (sender dropped) or the pipeline rejects a push (torn down). Sends EOS on
/// exit so the pipeline can shut down cleanly.
pub async fn webrtc_pump(mut rx: mpsc::Receiver<bytes::Bytes>, appsrc: gst_app::AppSrc) {
    while let Some(chunk) = rx.recv().await {
        let buffer = gst::Buffer::from_slice(chunk);
        if appsrc.push_buffer(buffer).is_err() {
            break;
        }
    }
    let _ = appsrc.end_of_stream();
}
