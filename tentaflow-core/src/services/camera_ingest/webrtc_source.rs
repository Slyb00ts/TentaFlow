// =============================================================================
// File: services/camera_ingest/webrtc_source.rs
// Purpose: Camera source backed by a WebRTC video track. An H.264 Annex-B byte
//          stream (depacketized in tentaflow-hardware, delivered over an mpsc)
//          is pushed into a GStreamer appsrc, parsed once, then fanned out by a
//          tee into Branch A (decode → RGB → appsink, the always-on frame path
//          shared with every other camera) and an on-demand Branch B
//          (mp4mux → appsink) that feeds the fMP4 publisher for smooth MSE
//          playback. This is the only appsrc pipeline in the repo.
// =============================================================================

use std::sync::Arc;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use tokio::sync::mpsc;

use super::error::{CameraIngestError, Result};
use super::fakefile::{
    ensure_gst_initialized, install_frame_callback, FakeFilePipeline, FrameCounters, FrameMailbox,
};
use super::rtsp::{detach_mp4_branch, wire_mp4_appsink, Mp4BranchState};
use super::session::CameraConfig;
use super::stream_publisher::Mp4StreamPublisher;

/// Build the appsrc → tee pipeline. Branch A (`tee → queue → decodebin →
/// videoconvert → RGB → appsink`) is always present and drives the existing
/// frame_storage / streaming_bus path. The returned `tee` lets the session
/// attach an on-demand fMP4 mux branch (Branch B) without rebuilding. We also
/// return the appsrc so the pump can feed it.
///
/// We tee the RAW Annex-B byte stream (no parse before the tee) and give each
/// branch its OWN h264parse. A single shared parse before the tee broke Branch
/// B on the real robot stream: the front parse consumed the SPS/PPS into its
/// caps (codec_data) and forwarded bare slice NALUs, so Branch B's downstream
/// parse saw "broken/invalid nal" for every frame, dropped them all, and
/// mp4mux never produced an init segment (3 s timeout → detach loop). Branch
/// A's decodebin carries its own parser, so the raw tee feeds both consumers
/// the unmodified byte stream where each IDR re-announces SPS/PPS in-band.
///
/// Pipeline graph:
///
///   appsrc(byte-stream,au) → tee(allow-not-linked)
///       ├─ src_0 → queue → decodebin → videoconvert → RGB → appsink           (Branch A, always on)
///       └─ src_N → queue → h264parse(AVC) → mp4mux(fMP4) → appsink            (Branch B, on demand)
pub fn build_webrtc_pipeline(
    config: &CameraConfig,
    mailbox: Arc<FrameMailbox>,
    counters: Arc<FrameCounters>,
) -> Result<(FakeFilePipeline, gst_app::AppSrc, gst::Element)> {
    ensure_gst_initialized()?;

    let pipeline = gst::Pipeline::new();

    let appsrc = gst::ElementFactory::make("appsrc")
        .property("name", "src")
        .property("is-live", true)
        .property("do-timestamp", true)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("appsrc: {e}")))?;
    appsrc.set_property_from_str("format", "time");

    // `allow-not-linked=true` lets the pipeline run with Branch B absent (before
    // first attach and after detach) without tripping `not-linked (-1)` when the
    // tee pushes to a released request pad.
    let tee = gst::ElementFactory::make("tee")
        .property("name", "h264_tee")
        .property("allow-not-linked", true)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("tee: {e}")))?;

    // Branch A queue — decouples decode latency from the tee fan-out so a slow
    // appsink consumer cannot back-pressure Branch B (and vice versa).
    let queue_a = gst::ElementFactory::make("queue")
        .property("name", "queue_branch_a")
        .property("max-size-buffers", 30u32)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("queue_a: {e}")))?;
    queue_a.set_property_from_str("leaky", "downstream");

    let decodebin = gst::ElementFactory::make("decodebin")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("decodebin: {e}")))?;
    let convert = gst::ElementFactory::make("videoconvert")
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("videoconvert: {e}")))?;
    let caps = gst::Caps::builder("video/x-raw")
        .field("format", "RGB")
        .build();
    let capsfilter = gst::ElementFactory::make("capsfilter")
        .property("caps", &caps)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("capsfilter: {e}")))?;
    let appsink = gst::ElementFactory::make("appsink")
        .property("name", "sink")
        .property("emit-signals", false)
        .property("sync", false)
        .property("max-buffers", 2u32)
        .property("drop", true)
        .build()
        .map_err(|e| CameraIngestError::PipelineBuild(format!("appsink: {e}")))?;

    pipeline
        .add_many([
            &appsrc,
            &tee,
            &queue_a,
            &decodebin,
            &convert,
            &capsfilter,
            &appsink,
        ])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("add_many: {e}")))?;

    // Static segments: appsrc → tee, and convert → capsfilter → appsink.
    // tee.src_0 → queue_a → decodebin and decodebin → convert are wired by
    // request pad / dynamic pad below.
    gst::Element::link(&appsrc, &tee)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("appsrc → tee: {e}")))?;
    let tee_src_a = tee
        .request_pad_simple("src_%u")
        .ok_or_else(|| CameraIngestError::PipelineBuild("tee src_%u request failed".into()))?;
    let queue_a_sink = queue_a
        .static_pad("sink")
        .ok_or_else(|| CameraIngestError::PipelineBuild("queue_a sink pad missing".into()))?;
    tee_src_a
        .link(&queue_a_sink)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("tee → queue_a: {e:?}")))?;
    gst::Element::link(&queue_a, &decodebin)
        .map_err(|e| CameraIngestError::PipelineBuild(format!("queue_a → decodebin: {e}")))?;
    gst::Element::link_many([&convert, &capsfilter, &appsink])
        .map_err(|e| CameraIngestError::PipelineBuild(format!("link_many tail: {e}")))?;

    // decodebin's video output pad appears dynamically once it identifies the
    // codec — wire it into videoconvert. This source is H.264-only and decodes
    // to a single raw video pad, so we link it robustly: some decodebin impls
    // emit `pad-added` BEFORE caps are negotiated (current_caps() == None at
    // that instant). Bailing on missing caps would leave Branch A unlinked and
    // stop ALL frames (frame_storage / JPEG / inference). So when caps are not
    // yet known we attempt the link unconditionally for the new pad; when caps
    // ARE present we still gate on `video/` to skip any stray non-video pad.
    let convert_weak = convert.downgrade();
    decodebin.connect_pad_added(move |_dec, src_pad| {
        let Some(convert) = convert_weak.upgrade() else {
            return;
        };
        let Some(sink_pad) = convert.static_pad("sink") else {
            return;
        };
        if sink_pad.is_linked() {
            return;
        }
        if let Some(structure) = src_pad.current_caps().and_then(|c| c.structure(0).map(|s| s.name().to_string())) {
            if !structure.starts_with("video/") {
                return;
            }
        }
        // Caps unknown OR confirmed video: attempt the link. If decodebin is
        // not ready to negotiate yet the link errors harmlessly and the same
        // raw video pad will not re-trigger pad-added — but for this single-pad
        // H.264 source the pad is the decoded video, so the link succeeds once
        // decodebin pushes caps downstream.
        if let Err(e) = src_pad.link(&sink_pad) {
            tracing::warn!("webrtc: decodebin → videoconvert link failed: {e:?}");
        }
    });

    let appsink_app = appsink
        .clone()
        .downcast::<gst_app::AppSink>()
        .map_err(|_| CameraIngestError::PipelineBuild("'sink' is not AppSink".into()))?;
    install_frame_callback(&appsink_app, config.camera_id.clone(), mailbox, counters);

    let appsrc_app = appsrc
        .downcast::<gst_app::AppSrc>()
        .map_err(|_| CameraIngestError::PipelineBuild("'src' is not AppSrc".into()))?;

    // The robot delivers H.264 as Annex-B access units (start-code prefixed),
    // unlike RTSP where rtph264depay yields RTP-framed NALUs. Pin the appsrc
    // caps so h264parse negotiates without ambiguity on the first push.
    let in_caps = gst::Caps::builder("video/x-h264")
        .field("stream-format", "byte-stream")
        .field("alignment", "au")
        .build();
    appsrc_app.set_caps(Some(&in_caps));
    // Bounded internal buffering — the gate already drops/re-primes upstream, so
    // back-pressure here just bounds memory, not correctness.
    appsrc_app.set_max_bytes(4 * 1024 * 1024);

    Ok((
        FakeFilePipeline {
            pipeline,
            appsink: appsink_app,
        },
        appsrc_app,
        tee,
    ))
}

/// Attach Branch B (`tee → queue → h264parse(AVC) → mp4mux → appsink`) to the
/// running webrtc pipeline and route the mux output into `publisher`. Unlike
/// the RTSP Branch B there is no rtph264depay — the appsrc feeds the tee a raw
/// Annex-B byte stream, so Branch B's own h264parse frames it and switches to
/// AVC sample format (length-prefixed NALUs with in-band SPS/PPS) which mp4mux
/// requires. This is the ONLY parse on the Branch B path: parsing the raw byte
/// stream here (rather than re-parsing an already-parsed stream) lets h264parse
/// see the SPS/PPS the robot re-announces at every IDR and build the avcC
/// codec_data mp4mux needs for the init segment. mp4mux/h264parse properties
/// mirror RTSP exactly so the browser MSE init segment + fragment layout is
/// identical.
pub(super) fn attach_mp4_branch_webrtc(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    publisher: &Arc<Mp4StreamPublisher>,
) -> std::result::Result<Mp4BranchState, String> {
    let queue_b = gst::ElementFactory::make("queue")
        .property("name", "queue_branch_b")
        .property("max-size-buffers", 60u32)
        .build()
        .map_err(|e| format!("queue_b build: {e}"))?;
    queue_b.set_property_from_str("leaky", "downstream");
    // config-interval=-1 makes h264parse repeat SPS/PPS in-band and emit AVC
    // sample format — mp4mux refuses byte-stream input with `not-negotiated`.
    let parse = gst::ElementFactory::make("h264parse")
        .property_from_str("config-interval", "-1")
        .build()
        .map_err(|e| format!("h264parse build: {e}"))?;
    // streamable=true → ftyp+moov init segment on the first fragment, then
    // moof+mdat media fragments with no finalize. fragment-duration in ms.
    let mux = gst::ElementFactory::make("mp4mux")
        .property("fragment-duration", 200u32)
        .property("streamable", true)
        .build()
        .map_err(|e| format!("mp4mux build: {e}"))?;
    let sink = gst::ElementFactory::make("appsink")
        .property("name", "sink_mp4")
        .property("emit-signals", false)
        .property("sync", false)
        .property("max-buffers", 8u32)
        .property("drop", false)
        .build()
        .map_err(|e| format!("appsink_b build: {e}"))?;

    pipeline
        .add_many([&queue_b, &parse, &mux, &sink])
        .map_err(|e| format!("add_many branch B: {e}"))?;

    let tee_src_pad = tee
        .request_pad_simple("src_%u")
        .ok_or_else(|| {
            // No request pad yet, but the elements ARE in the pipeline — remove
            // them so a failed attach never leaves dangling elements that a
            // later attach would trip over.
            let refs: Vec<&gst::Element> = [&queue_b, &parse, &mux, &sink].into_iter().collect();
            let _ = pipeline.remove_many(refs);
            "tee src_%u request for branch B failed".to_string()
        })?;

    // From here the request pad exists alongside the added elements: capture
    // both as a single `Mp4BranchState` so EVERY subsequent failure path can
    // call the shared detach (unlink → NULL → remove → release request pad)
    // instead of leaking elements/pads into the live pipeline. On success the
    // state is returned to the session unchanged.
    let state = Mp4BranchState {
        tee_src_pad,
        elements: vec![queue_b, parse, mux, sink],
    };

    // `wire_and_link` performs every fallible step that follows pad acquisition;
    // on the first error the state is detached and the error is surfaced.
    if let Err(e) = wire_and_link_webrtc_branch(&state, publisher) {
        detach_mp4_branch(pipeline, tee, state);
        return Err(e);
    }

    Ok(state)
}

/// Link Branch B's elements, wire the mux appsink to the publisher, and bring
/// the branch up to the pipeline's current state. Split out from
/// `attach_mp4_branch_webrtc` so a single error path can drive the shared
/// detach/cleanup on any failure after the tee request pad exists. The element
/// order in `state.elements` is `[queue_b, parse, mux, sink]`.
fn wire_and_link_webrtc_branch(
    state: &Mp4BranchState,
    publisher: &Arc<Mp4StreamPublisher>,
) -> std::result::Result<(), String> {
    let queue_b = &state.elements[0];
    let sink = &state.elements[3];

    let queue_b_sink = queue_b
        .static_pad("sink")
        .ok_or_else(|| "queue_b sink pad missing".to_string())?;
    state
        .tee_src_pad
        .link(&queue_b_sink)
        .map_err(|e| format!("tee → queue_b: {e:?}"))?;
    gst::Element::link_many(state.elements.iter())
        .map_err(|e| format!("link branch B: {e}"))?;

    wire_mp4_appsink(sink, publisher)?;

    // Bring every new element up to the pipeline's current state so the mux
    // branch starts producing without a full pipeline restart.
    for el in &state.elements {
        el.sync_state_with_parent()
            .map_err(|e| format!("sync_state branch B element: {e}"))?;
    }
    Ok(())
}

/// Detach Branch B from the webrtc pipeline. Delegates to the shared RTSP
/// teardown — the unlink/NULL/remove/release-pad sequence is source-agnostic.
pub(super) fn detach_mp4_branch_webrtc(
    pipeline: &gst::Pipeline,
    tee: &gst::Element,
    state: Mp4BranchState,
) {
    detach_mp4_branch(pipeline, tee, state);
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
